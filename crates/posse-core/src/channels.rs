//! Named channels of clients.
//!
//! The channels that exist are in a table all clients share. Who's in one isn't:
//! each client lists its own channels in its status, so a client that goes away
//! takes its memberships with it, and no client writes to another's slot.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering, fence};

use crate::board::Region;
use crate::names::{MAX_CHANNEL_NAME, validate_channel_name};

/// Channels in a posse.
pub const MAX_CHANNELS: usize = 64;

/// Room for the longest name and a terminator.
const NAME_WORDS: usize = (MAX_CHANNEL_NAME + 1) / 4;

/// The low bits of an entry's stamp. The rest count the times the entry has
/// been deleted, so a read can tell a name replaced while it was reading. A
/// client crashing in the few instructions it's `WRITING` loses the entry until
/// every client has left the block.
const FREE: u32 = 0;
const WRITING: u32 = 1;
const IN_USE: u32 = 2;
const STATE_MASK: u32 = 3;
const ONE_DELETION: u32 = STATE_MASK + 1;

const _: () = assert!((MAX_CHANNEL_NAME + 1).is_multiple_of(4));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    InvalidName,
    Full,
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelError::InvalidName => write!(f, "a channel name is 1 to {MAX_CHANNEL_NAME} letters, digits, - or _"),
            ChannelError::Full => write!(f, "the posse already has {MAX_CHANNELS} channels"),
        }
    }
}

#[repr(C)]
struct Entry {
    stamp: AtomicU32,
    name: [AtomicU32; NAME_WORDS],
}

#[repr(C)]
pub(crate) struct ChannelTable {
    entries: [Entry; MAX_CHANNELS],
}

impl Entry {
    /// The name in this entry, or None if it has none or it's changing.
    fn read(&self) -> Option<String> {
        let stamp = self.stamp.load(Ordering::Acquire);
        if stamp & STATE_MASK != IN_USE {
            return None;
        }
        let bytes = self.name_bytes();
        fence(Ordering::Acquire);
        if self.stamp.load(Ordering::Relaxed) != stamp {
            return None;
        }
        Some(text_of(&bytes))
    }

    fn name_bytes(&self) -> Vec<u8> {
        self.name.iter().flat_map(|word| word.load(Ordering::Relaxed).to_le_bytes()).collect()
    }

    fn write_name(&self, name: &str) {
        let mut bytes = [0; NAME_WORDS * 4];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        for (word, chunk) in self.name.iter().zip(bytes.chunks(4)) {
            word.store(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]), Ordering::Relaxed);
        }
    }
}

fn text_of(bytes: &[u8]) -> String {
    let len = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

impl Region {
    /// Adds a channel. Returns false if it was already there. Two clients
    /// creating one at the same moment can make two entries, which every read
    /// treats as one.
    pub fn create_channel(&self, name: &str) -> Result<bool, ChannelError> {
        validate_channel_name(name).map_err(|_| ChannelError::InvalidName)?;
        if self.channel(name).is_some() {
            return Ok(false);
        }
        for entry in &self.channels.entries {
            let stamp = entry.stamp.load(Ordering::Acquire);
            if stamp & STATE_MASK == FREE && entry.stamp.compare_exchange(stamp, stamp | WRITING, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                fence(Ordering::Release);
                entry.write_name(name);
                entry.stamp.store(stamp | IN_USE, Ordering::Release);
                return Ok(true);
            }
        }
        Err(ChannelError::Full)
    }

    /// Removes a channel. Returns false if it wasn't there.
    pub fn delete_channel(&self, name: &str) -> bool {
        let mut deleted = false;
        for entry in &self.channels.entries {
            let stamp = entry.stamp.load(Ordering::Acquire);
            if entry.read().is_some_and(|found| found.eq_ignore_ascii_case(name)) {
                deleted |= entry.stamp.compare_exchange(stamp, (stamp & !STATE_MASK).wrapping_add(ONE_DELETION), Ordering::AcqRel, Ordering::Relaxed).is_ok();
            }
        }
        deleted
    }

    /// A channel's name as it was created, whatever the case of `name`.
    pub fn channel(&self, name: &str) -> Option<String> {
        self.channels.entries.iter().filter_map(Entry::read).find(|found| found.eq_ignore_ascii_case(name))
    }

    /// Every channel, sorted by name.
    pub fn channel_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.channels.entries.iter().filter_map(Entry::read).collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::board::tests::{region, zeroed_memory};

    #[test]
    fn a_created_channel_is_found_in_any_case_under_the_name_it_was_created_with() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        assert_eq!(region.channel("Healers"), None);
        assert_eq!(region.create_channel("Healers"), Ok(true));
        assert_eq!(region.channel("HEALERS").as_deref(), Some("Healers"));
        assert_eq!(region.create_channel("healers"), Ok(false), "the same channel in another case");
        assert_eq!(region.channel_names(), ["Healers"]);
    }

    #[test]
    fn channels_are_listed_by_name_in_any_case() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        for name in ["tanks", "Bards", "casters"] {
            region.create_channel(name).unwrap();
        }
        assert_eq!(region.channel_names(), ["Bards", "casters", "tanks"]);
    }

    #[test]
    fn a_bad_name_is_refused_and_creates_nothing() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        for bad in ["", "two words", "a,b", "a=b", &"x".repeat(MAX_CHANNEL_NAME + 1)] {
            assert_eq!(region.create_channel(bad), Err(ChannelError::InvalidName), "{bad}");
        }
        assert_eq!(region.channel_names(), Vec::<String>::new());
    }

    #[test]
    fn the_longest_name_comes_back_whole() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let longest = "x".repeat(MAX_CHANNEL_NAME);
        region.create_channel(&longest).unwrap();
        assert_eq!(region.channel_names(), [longest]);
    }

    #[test]
    fn a_shorter_name_in_a_reused_entry_shows_none_of_the_longer_one() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        region.create_channel("a-much-longer-name").unwrap();
        assert!(region.delete_channel("a-much-longer-name"));
        region.create_channel("short").unwrap();
        assert_eq!(region.channel_names(), ["short"]);
    }

    #[test]
    fn deleting_removes_only_that_channel_and_says_if_there_was_none() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        region.create_channel("healers").unwrap();
        region.create_channel("tanks").unwrap();
        assert!(region.delete_channel("HEALERS"));
        assert!(!region.delete_channel("healers"), "already gone");
        assert!(!region.delete_channel("casters"), "never there");
        assert_eq!(region.channel_names(), ["tanks"]);
    }

    #[test]
    fn the_table_fills_up_and_a_deletion_makes_room() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        for n in 0..MAX_CHANNELS {
            assert_eq!(region.create_channel(&format!("c{n}")), Ok(true));
        }
        assert_eq!(region.create_channel("onemore"), Err(ChannelError::Full));
        assert_eq!(region.create_channel("c0"), Ok(false), "a full table still knows what it has");
        assert!(region.delete_channel("c7"));
        assert_eq!(region.create_channel("onemore"), Ok(true));
        assert_eq!(region.channel_names().len(), MAX_CHANNELS);
    }

    #[test]
    fn clients_creating_the_same_channel_at_once_get_one_channel_that_one_delete_removes() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        for _ in 0..200 {
            thread::scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(|| region.create_channel("healers").unwrap());
                }
            });
            assert_eq!(region.channel_names(), ["healers"]);
            assert!(region.delete_channel("healers"));
            assert_eq!(region.channel_names(), Vec::<String>::new(), "no duplicate left behind");
        }
    }

    #[test]
    fn a_client_that_died_while_creating_leaves_no_name_and_costs_only_that_entry() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        region.channels.entries[0].stamp.store(WRITING, Ordering::Relaxed);
        region.channels.entries[0].write_name("half");
        assert_eq!(region.channel("half"), None);
        region.create_channel("whole").unwrap();
        assert_eq!(region.channel_names(), ["whole"]);
    }

    /// A name that can be checked as all from one write.
    fn patterned(n: u32) -> String {
        char::from(b'a' + (n % 26) as u8).to_string().repeat(MAX_CHANNEL_NAME)
    }

    fn is_torn(name: &str) -> bool {
        name.is_empty() || name.bytes().any(|byte| byte != name.as_bytes()[0])
    }

    /// Runs `read` against a thread that creates and deletes channels as fast
    /// as it can, until `read` sees a torn name or the time is up. Returns
    /// whether it did.
    fn saw_torn_name_while_writing(region: &Region, limit: Duration, mut read: impl FnMut() -> Option<String>) -> bool {
        let stop = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut n = 0;
                while !stop.load(Ordering::Relaxed) {
                    let name = patterned(n);
                    region.create_channel(&name).unwrap();
                    region.delete_channel(&name);
                    n = n.wrapping_add(1);
                }
            });
            let started = Instant::now();
            let mut torn = false;
            while !torn && started.elapsed() < limit {
                torn = read().is_some_and(|name| is_torn(&name));
            }
            stop.store(true, Ordering::Relaxed);
            torn
        })
    }

    #[test]
    fn reads_never_see_a_torn_name() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let read = || region.channels.entries[0].read();
        assert!(!saw_torn_name_while_writing(region, Duration::from_secs(1), read));
    }

    /// The negative for `reads_never_see_a_torn_name`: without the stamp check
    /// the same writer does tear a read, so that test can catch it.
    #[test]
    fn a_read_without_the_stamp_check_does_see_a_torn_name() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let unchecked = || {
            let name = text_of(&region.channels.entries[0].name_bytes());
            (!name.is_empty()).then_some(name)
        };
        assert!(saw_torn_name_while_writing(region, Duration::from_secs(30), unchecked));
    }
}
