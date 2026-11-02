//! The block of memory clients share: a version and one slot per client.
//!
//! Each client writes status only to its own slot, under a sequence counter
//! that's odd during a write, so a reader can tell it read a whole update.
//! Status is copied in atomic words because another process may be writing
//! the same memory while it's read.

use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, Ordering, fence};

use crate::channels::ChannelTable;
use crate::commands::{CommandLog, Reply};
use crate::paths::PathTables;
use crate::status::{STATUS_WORDS, Status};

/// Clients with a different layout refuse to share a block. Change this with
/// any change to [`Region`], [`Status`], the channel table, the command log,
/// the replies or the path tables.
pub const LAYOUT_VERSION: u32 = 0x5053_0004;
pub const MAX_CLIENTS: usize = 64;
pub const REGION_SIZE: usize = size_of::<Region>();

/// A slot's `pid` while a client takes it, so nobody reads the slot half set
/// up. A client crashing in those few instructions loses the slot until every
/// client has left the block.
const CLAIMING: u32 = u32::MAX;
const FREE: u32 = 0;
/// Attempts at reading a slot before giving up, as a read runs on the game
/// thread and can't wait for a writer that isn't running.
const READ_TRIES: u32 = 32;

/// A running process: a process ID alone could be reused after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Owner {
    pub pid: u32,
    pub started: u64,
}

/// Whether a slot's owner is still running.
pub trait Liveness {
    fn is_alive(&mut self, owner: Owner) -> bool;
}

/// A slot's position in the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotId(pub(crate) usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// The memory can't hold this build's layout, or isn't aligned for it.
    TooSmall,
    /// Another build already uses this block.
    VersionMismatch { found: u32 },
}

impl fmt::Display for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttachError::TooSmall => write!(f, "the shared memory doesn't fit this build"),
            AttachError::VersionMismatch { found } => write!(f, "another build is using it (layout {found:#x}, this is {LAYOUT_VERSION:#x})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    Full,
}

#[repr(C)]
struct Slot {
    pid: AtomicU32,
    started_low: AtomicU32,
    started_high: AtomicU32,
    /// 0 before the first publish, odd during a write.
    sequence: AtomicU32,
    status: [AtomicU32; STATUS_WORDS],
    commands: CommandLog,
    /// What this client did with what each slot sent it. Like the log, these are
    /// never reset: the sender matches an answer to a sequence number, and counts
    /// how far its totals have moved.
    replies: [Reply; MAX_CLIENTS],
    /// The paths this client asks others for, and the values of those asked of it.
    paths: PathTables,
}

#[repr(C)]
pub struct Region {
    version: AtomicU32,
    pub(crate) channels: ChannelTable,
    slots: [Slot; MAX_CLIENTS],
}

impl Region {
    /// Checks the layout of a block, writing it if the block is new.
    ///
    /// # Safety
    /// `base` must point to `len` bytes, zeroed when first created, that stay
    /// mapped for `'a` and that everything touching them treats as a `Region`.
    pub unsafe fn attach<'a>(base: *mut u8, len: usize) -> Result<&'a Region, AttachError> {
        if len < size_of::<AtomicU32>() || !base.cast::<Region>().is_aligned() {
            return Err(AttachError::TooSmall);
        }
        let version = unsafe { &*base.cast::<AtomicU32>() };
        if let Err(found) = version.compare_exchange(0, LAYOUT_VERSION, Ordering::AcqRel, Ordering::Acquire)
            && found != LAYOUT_VERSION
        {
            return Err(AttachError::VersionMismatch { found });
        }
        if len < REGION_SIZE {
            return Err(AttachError::TooSmall);
        }
        Ok(unsafe { &*base.cast::<Region>() })
    }

    /// Takes a slot for `me`: one it already held (the plugin reloaded without
    /// releasing it), or else a free one or one whose owner has died.
    pub fn claim(&self, me: Owner, liveness: &mut impl Liveness) -> Result<SlotId, ClaimError> {
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.owner() == Some(me) && slot.take(me.pid, me) {
                return Ok(SlotId(i));
            }
        }
        for (i, slot) in self.slots.iter().enumerate() {
            let available = match slot.pid.load(Ordering::Acquire) {
                FREE => Some(FREE),
                CLAIMING => None,
                pid => slot.owner().filter(|owner| !liveness.is_alive(*owner)).map(|_| pid),
            };
            if available.is_some_and(|pid| slot.take(pid, me)) {
                return Ok(SlotId(i));
            }
        }
        Err(ClaimError::Full)
    }

    /// Gives up a slot, if `me` still holds it.
    pub fn release(&self, id: SlotId, me: Owner) {
        let slot = &self.slots[id.0];
        if slot.owner() == Some(me) {
            let _ = slot.pid.compare_exchange(me.pid, FREE, Ordering::AcqRel, Ordering::Relaxed);
        }
    }

    /// Writes the status of a slot this client holds.
    pub fn publish(&self, id: SlotId, status: &Status) {
        let slot = &self.slots[id.0];
        let sequence = slot.sequence.load(Ordering::Relaxed);
        slot.sequence.store(sequence | 1, Ordering::Relaxed);
        fence(Ordering::Release);
        let words = std::ptr::from_ref(status).cast::<u32>();
        for (i, word) in slot.status.iter().enumerate() {
            // Status is STATUS_WORDS words with no padding.
            word.store(unsafe { words.add(i).read() }, Ordering::Relaxed);
        }
        slot.sequence.store((sequence | 1).wrapping_add(1).max(2), Ordering::Release);
    }

    /// The latest whole status in a slot. None if it has never been published,
    /// or if a writer is holding it up.
    pub fn read(&self, id: SlotId) -> Option<Status> {
        let slot = &self.slots[id.0];
        for _ in 0..READ_TRIES {
            let before = slot.sequence.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let mut status = MaybeUninit::<Status>::uninit();
            let words = status.as_mut_ptr().cast::<u32>();
            for (i, word) in slot.status.iter().enumerate() {
                unsafe { words.add(i).write(word.load(Ordering::Relaxed)) };
            }
            fence(Ordering::Acquire);
            if slot.sequence.load(Ordering::Relaxed) == before {
                // Every word was written and any bits are a valid Status.
                return Some(unsafe { status.assume_init() });
            }
        }
        None
    }

    pub(crate) fn log(&self, id: SlotId) -> &CommandLog {
        &self.slots[id.0].commands
    }

    /// What the client in `id` has reported to the client in `to`.
    pub(crate) fn reply(&self, id: SlotId, to: SlotId) -> &Reply {
        &self.slots[id.0].replies[to.0]
    }

    pub(crate) fn paths(&self, id: SlotId) -> &PathTables {
        &self.slots[id.0].paths
    }

    /// Slots that have an owner, with their owners.
    pub(crate) fn owners(&self) -> impl Iterator<Item = (SlotId, Owner)> + '_ {
        self.slots.iter().enumerate().filter_map(|(i, slot)| Some((SlotId(i), slot.owner()?)))
    }
}

impl Slot {
    fn owner(&self) -> Option<Owner> {
        let pid = self.pid.load(Ordering::Acquire);
        if pid == FREE || pid == CLAIMING {
            return None;
        }
        let (low, high) = (self.started_low.load(Ordering::Relaxed), self.started_high.load(Ordering::Relaxed));
        Some(Owner { pid, started: u64::from(high) << 32 | u64::from(low) })
    }

    /// Takes the slot from `expected`, clearing what it held so nobody sees an
    /// earlier owner's status as this one's.
    fn take(&self, expected: u32, me: Owner) -> bool {
        if self.pid.compare_exchange(expected, CLAIMING, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            return false;
        }
        self.sequence.store(0, Ordering::Release);
        self.paths.clear();
        self.started_low.store(me.started as u32, Ordering::Relaxed);
        self.started_high.store((me.started >> 32) as u32, Ordering::Relaxed);
        self.pid.store(me.pid, Ordering::Release);
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::status::MAX_SONGS;

    /// Zeroed memory for a block, as a new file mapping is.
    pub(crate) fn zeroed_memory() -> Vec<u32> {
        vec![0; REGION_SIZE / 4]
    }

    pub(crate) fn region(memory: &mut [u32]) -> &Region {
        unsafe { Region::attach(memory.as_mut_ptr().cast(), memory.len() * 4) }.unwrap()
    }

    /// Everything's alive except the owners listed.
    #[derive(Default)]
    pub(crate) struct Dead(pub HashSet<Owner>);

    impl Liveness for Dead {
        fn is_alive(&mut self, owner: Owner) -> bool {
            !self.0.contains(&owner)
        }
    }

    pub(crate) fn owner(pid: u32) -> Owner {
        Owner { pid, started: u64::from(pid) * 1000 }
    }

    /// Leaves a slot's first result as a writer stopped partway through it leaves it.
    pub(crate) fn stop_mid_write_paths(region: &Region, slot: SlotId) {
        region.slots[slot.0].paths.stop_first_result_mid_write();
    }

    /// Leaves a slot as a writer stopped partway through publishing leaves it.
    pub(crate) fn stop_mid_write(region: &Region, slot: SlotId) {
        let sequence = &region.slots[slot.0].sequence;
        sequence.store(sequence.load(Ordering::Relaxed) | 1, Ordering::Relaxed);
    }

    fn named(name: &str) -> Status {
        let mut status = Status::default();
        status.set_name(name);
        status
    }

    #[test]
    fn a_published_status_reads_back() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = region.claim(owner(1), &mut Dead::default()).unwrap();
        assert_eq!(region.read(slot), None, "nothing published yet");
        region.publish(slot, &named("Bob"));
        assert_eq!(region.read(slot).unwrap().name(), "Bob");
    }

    #[test]
    fn a_different_layout_is_refused() {
        let mut memory = zeroed_memory();
        memory[0] = LAYOUT_VERSION + 1;
        let attached = unsafe { Region::attach(memory.as_mut_ptr().cast(), memory.len() * 4) };
        assert_eq!(attached.err(), Some(AttachError::VersionMismatch { found: LAYOUT_VERSION + 1 }));
    }

    #[test]
    fn a_block_too_small_is_refused() {
        let mut memory = zeroed_memory();
        let attached = unsafe { Region::attach(memory.as_mut_ptr().cast(), memory.len() * 4 - 4) };
        assert_eq!(attached.err(), Some(AttachError::TooSmall));
    }

    #[test]
    fn a_second_client_joins_the_same_layout() {
        let mut memory = zeroed_memory();
        let base = memory.as_mut_ptr().cast::<u8>();
        let len = memory.len() * 4;
        unsafe { Region::attach(base, len) }.unwrap();
        assert!(unsafe { Region::attach(base, len) }.is_ok());
    }

    #[test]
    fn clients_get_different_slots_until_full() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let mut live = Dead::default();
        let mut slots = Vec::new();
        for pid in 1..=MAX_CLIENTS as u32 {
            slots.push(region.claim(owner(pid), &mut live).unwrap().0);
        }
        slots.dedup();
        assert_eq!(slots.len(), MAX_CLIENTS);
        assert_eq!(region.claim(owner(1000), &mut live), Err(ClaimError::Full));
    }

    #[test]
    fn a_dead_clients_slot_is_reused_without_its_status() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let mut live = Dead::default();
        let slot = region.claim(owner(1), &mut live).unwrap();
        region.publish(slot, &named("Dead"));

        // Alive, its slot is kept.
        assert_ne!(region.claim(owner(2), &mut live).unwrap(), slot);

        live.0.insert(owner(1));
        assert_eq!(region.claim(owner(3), &mut live).unwrap(), slot);
        assert_eq!(region.read(slot), None, "the earlier owner's status must not show as the new one's");
    }

    #[test]
    fn a_reused_process_id_is_a_different_owner() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let mut live = Dead::default();
        let slot = region.claim(owner(1), &mut live).unwrap();
        let reused = Owner { pid: 1, started: 999_999 };
        live.0.insert(owner(1));
        assert_eq!(region.claim(reused, &mut live).unwrap(), slot);
        assert_eq!(region.owners().next().unwrap().1, reused);
    }

    #[test]
    fn a_reloaded_plugin_gets_its_slot_back() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let mut live = Dead::default();
        let slot = region.claim(owner(1), &mut live).unwrap();
        region.publish(slot, &named("Before"));
        assert_eq!(region.claim(owner(1), &mut live).unwrap(), slot);
        assert_eq!(region.read(slot), None);
        assert_eq!(region.owners().count(), 1);
    }

    #[test]
    fn release_frees_the_slot_only_for_its_owner() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = region.claim(owner(1), &mut Dead::default()).unwrap();
        region.release(slot, owner(2));
        assert_eq!(region.owners().count(), 1, "another client can't release it");
        region.release(slot, owner(1));
        assert_eq!(region.owners().count(), 0);
    }

    #[test]
    fn a_writer_that_died_mid_write_reads_as_nothing() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = region.claim(owner(1), &mut Dead::default()).unwrap();
        region.publish(slot, &named("Bob"));
        stop_mid_write(region, slot);
        assert_eq!(region.read(slot), None);
    }

    #[test]
    fn publishing_recovers_from_an_unfinished_write() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = region.claim(owner(1), &mut Dead::default()).unwrap();
        region.publish(slot, &named("Bob"));
        stop_mid_write(region, slot);
        region.publish(slot, &named("Bob"));
        assert_eq!(region.read(slot).unwrap().name(), "Bob");
    }

    /// A status whose words can be checked as all from one write.
    fn patterned(n: u32) -> Status {
        let mut status = Status::default();
        status.id = n;
        status.level = n;
        status.target_id = n;
        status.buffs[0].spell_id = n as i32;
        status.songs[MAX_SONGS - 1].spell_id = n as i32;
        status
    }

    fn is_torn(status: &Status) -> bool {
        let n = status.id;
        status.level != n || status.target_id != n || status.buffs[0].spell_id != n as i32 || status.songs[MAX_SONGS - 1].spell_id != n as i32
    }

    /// Runs `read` against a thread that publishes as fast as it can, until
    /// `read` sees a torn status or the time is up. Returns whether it did.
    fn saw_torn_status_while_publishing(region: &Region, slot: SlotId, limit: Duration, read: impl Fn() -> Option<Status>) -> bool {
        let stop = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut n = 1;
                while !stop.load(Ordering::Relaxed) {
                    region.publish(slot, &patterned(n));
                    n = n.wrapping_add(1).max(1);
                }
            });
            let started = Instant::now();
            let mut torn = false;
            while !torn && started.elapsed() < limit {
                torn = read().is_some_and(|status| is_torn(&status));
            }
            stop.store(true, Ordering::Relaxed);
            torn
        })
    }

    #[test]
    fn reads_never_see_a_torn_status() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = region.claim(owner(1), &mut Dead::default()).unwrap();
        region.publish(slot, &patterned(1));
        assert!(!saw_torn_status_while_publishing(region, slot, Duration::from_secs(1), || region.read(slot)));
    }

    /// The negative for `reads_never_see_a_torn_status`: without the sequence
    /// check the same writer does tear a read, so that test can catch it.
    #[test]
    fn a_read_without_the_sequence_check_does_see_a_torn_status() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = region.claim(owner(1), &mut Dead::default()).unwrap();
        region.publish(slot, &patterned(1));
        let unchecked = || {
            let mut status = MaybeUninit::<Status>::uninit();
            let words = status.as_mut_ptr().cast::<u32>();
            for (i, word) in region.slots[slot.0].status.iter().enumerate() {
                unsafe { words.add(i).write(word.load(Ordering::Relaxed)) };
            }
            Some(unsafe { status.assume_init() })
        };
        assert!(saw_torn_status_while_publishing(region, slot, Duration::from_secs(30), unchecked));
    }

    #[test]
    fn clients_racing_for_slots_never_share_one() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let claimed: Vec<usize> = thread::scope(|scope| {
            let threads: Vec<_> = (1..=MAX_CLIENTS as u32).map(|pid| scope.spawn(move || region.claim(owner(pid), &mut Dead::default()).unwrap().0)).collect();
            threads.into_iter().map(|t| t.join().unwrap()).collect()
        });
        let unique: HashSet<usize> = claimed.iter().copied().collect();
        assert_eq!(unique.len(), MAX_CLIENTS);
    }
}
