//! Commands sent between clients, and the slash commands that send them.
//!
//! A client writes commands only to the log in its own slot, each addressed to
//! the slots it's for, and every reader keeps its own place in each log. So one
//! write reaches any number of clients, and a client that stops can't hold up
//! another. A log's oldest commands are overwritten, and a reader that falls
//! that far behind can tell.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering, fence};

use crate::board::{MAX_CLIENTS, Region, SlotId};
use crate::status::{State, Status};

/// The longest command that can be sent, the most MacroQuest takes in a line.
pub const MAX_COMMAND_LEN: usize = TEXT_WORDS * 4 - 1;
/// A command this old isn't run: what it was for has probably passed.
pub const COMMAND_TIMEOUT_MS: u64 = 5000;

const TEXT_WORDS: usize = 512;
/// Commands a client can send before overwriting its oldest. Readers normally
/// empty a log every frame, so this only has to outlast a burst in one frame.
const LOG_LEN: usize = 16;

/// Who a slash command reaches, besides perhaps the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Group,
    Raid,
    Zone,
}

/// A slash command that sends a command to other clients.
#[derive(Debug)]
pub struct Alias {
    pub name: &'static str,
    pub scope: Scope,
    /// Whether the sender runs it too.
    pub includes_sender: bool,
    pub description: &'static str,
}

pub static ALIASES: [Alias; 8] = [
    Alias { name: "/pc", scope: Scope::All, includes_sender: false, description: "Runs a command on every connected client except you" },
    Alias { name: "/pca", scope: Scope::All, includes_sender: true, description: "Runs a command on every connected client, including you" },
    Alias { name: "/pgc", scope: Scope::Group, includes_sender: false, description: "Runs a command on every client in your group except you" },
    Alias { name: "/pgca", scope: Scope::Group, includes_sender: true, description: "Runs a command on every client in your group, including you" },
    Alias { name: "/prc", scope: Scope::Raid, includes_sender: false, description: "Runs a command on every client in your raid except you, your group included" },
    Alias { name: "/prca", scope: Scope::Raid, includes_sender: true, description: "Runs a command on every client in your raid, including you" },
    Alias { name: "/pzc", scope: Scope::Zone, includes_sender: false, description: "Runs a command on every client in your zone except you" },
    Alias { name: "/pzca", scope: Scope::Zone, includes_sender: true, description: "Runs a command on every client in your zone, including you" },
];

/// The `/posse` subcommands that work so far, as `/posse help` lists them.
pub const SUBCOMMANDS: [(&str, &str); 8] = [
    ("/posse help", "Lists these commands"),
    ("/posse list", "Lists members with their state and data age, as does /posse alone"),
    ("/posse join <name>", "Leaves the current posse and joins that one, until you next go to character select"),
    ("/posse leave", "Leaves the posse until the next /posse join"),
    ("/posse debug on", "Says in chat what this client sends and receives, and keeps doing so for this character"),
    ("/posse debug off", "Stops saying that"),
    ("/posse stats", "Shows what publishing status costs this client per pulse, how old each member's status has been, and how long commands took to reach this client"),
    ("/posse stats reset", "Starts those numbers again"),
];

/// Which members a command is for, as the sender sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    All,
    /// The sender's group or raid.
    Names(Vec<String>),
    /// In game in this zone. Zoning members have left it, wherever they're headed.
    Zone { zone: String, instance: u32 },
}

impl Audience {
    pub fn includes(&self, status: &Status) -> bool {
        match self {
            Audience::All => true,
            Audience::Names(names) => {
                let name = status.name();
                names.iter().any(|other| other.eq_ignore_ascii_case(&name))
            }
            Audience::Zone { zone, instance } => status.state() == State::InGame && status.zone_instance == *instance && status.zone().eq_ignore_ascii_case(zone),
        }
    }
}

/// The slots a command is addressed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Targets(u64);

const _: () = assert!(MAX_CLIENTS <= u64::BITS as usize);

impl Targets {
    pub fn add(&mut self, slot: SlotId) {
        self.0 |= 1 << slot.0;
    }

    pub fn contains(self, slot: SlotId) -> bool {
        self.0 & 1 << slot.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandTooLong;

impl fmt::Display for CommandTooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the command is over {MAX_COMMAND_LEN} characters")
    }
}

#[repr(C)]
struct Entry {
    /// Its sequence number plus 1, or 0 during a write.
    stamp: AtomicU32,
    targets_low: AtomicU32,
    targets_high: AtomicU32,
    sent_ms_low: AtomicU32,
    sent_ms_high: AtomicU32,
    len: AtomicU32,
    text: [AtomicU32; TEXT_WORDS],
}

/// The commands a client has sent. It's never reset, even when another client
/// takes the slot, so a reader's place in it stays good.
#[repr(C)]
pub(crate) struct CommandLog {
    /// The number of commands ever sent, and so the sequence number of the next.
    sent: AtomicU32,
    entries: [Entry; LOG_LEN],
}

struct Sent {
    targets: Targets,
    sent_ms: u64,
    text: String,
}

impl Entry {
    /// The command with this sequence number, or None if it has been overwritten.
    fn read(&self, sequence: u32) -> Option<Sent> {
        let stamp = sequence.wrapping_add(1);
        if self.stamp.load(Ordering::Acquire) != stamp {
            return None;
        }
        let targets = u64::from(self.targets_high.load(Ordering::Relaxed)) << 32 | u64::from(self.targets_low.load(Ordering::Relaxed));
        let sent_ms = u64::from(self.sent_ms_high.load(Ordering::Relaxed)) << 32 | u64::from(self.sent_ms_low.load(Ordering::Relaxed));
        let len = (self.len.load(Ordering::Relaxed) as usize).min(MAX_COMMAND_LEN);
        let mut bytes = Vec::with_capacity(len.next_multiple_of(4));
        for word in &self.text[..len.div_ceil(4)] {
            bytes.extend_from_slice(&word.load(Ordering::Relaxed).to_le_bytes());
        }
        fence(Ordering::Acquire);
        if self.stamp.load(Ordering::Relaxed) != stamp {
            return None;
        }
        bytes.truncate(len);
        Some(Sent { targets: Targets(targets), sent_ms, text: String::from_utf8_lossy(&bytes).into_owned() })
    }
}

impl Region {
    /// Writes a command to the log of a slot this client holds.
    pub fn send(&self, from: SlotId, targets: Targets, sent_ms: u64, text: &str) -> Result<(), CommandTooLong> {
        if text.len() > MAX_COMMAND_LEN {
            return Err(CommandTooLong);
        }
        let log = self.log(from);
        let sequence = log.sent.load(Ordering::Relaxed);
        let entry = &log.entries[sequence as usize % LOG_LEN];
        entry.stamp.store(0, Ordering::Relaxed);
        fence(Ordering::Release);
        entry.targets_low.store(targets.0 as u32, Ordering::Relaxed);
        entry.targets_high.store((targets.0 >> 32) as u32, Ordering::Relaxed);
        entry.sent_ms_low.store(sent_ms as u32, Ordering::Relaxed);
        entry.sent_ms_high.store((sent_ms >> 32) as u32, Ordering::Relaxed);
        entry.len.store(text.len() as u32, Ordering::Relaxed);
        for (word, chunk) in entry.text.iter().zip(text.as_bytes().chunks(4)) {
            let mut bytes = [0; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            word.store(u32::from_le_bytes(bytes), Ordering::Relaxed);
        }
        entry.stamp.store(sequence.wrapping_add(1), Ordering::Release);
        log.sent.store(sequence.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

/// What a client finds addressed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `age_ms` is how long it took to get here.
    Run { from: SlotId, text: String, age_ms: u64 },
    /// Sent over [`COMMAND_TIMEOUT_MS`] ago, so it shouldn't run.
    TooLate { from: SlotId, text: String, age_ms: u64 },
    /// Commands overwritten before this client read them. Any number of them
    /// may have been addressed to it.
    Lost { from: SlotId, count: u32 },
}

/// One client's place in every log.
#[derive(Debug)]
pub struct Inbox {
    me: SlotId,
    next: [u32; MAX_CLIENTS],
}

impl Inbox {
    /// Starts after everything sent so far, which was for whoever was here then.
    pub fn new(region: &Region, me: SlotId) -> Inbox {
        Inbox { me, next: std::array::from_fn(|i| region.log(SlotId(i)).sent.load(Ordering::Acquire)) }
    }

    /// What's arrived since the last call, oldest first from each sender.
    pub fn receive(&mut self, region: &Region, now_ms: u64) -> Vec<Delivery> {
        let mut deliveries = Vec::new();
        for (i, next) in self.next.iter_mut().enumerate() {
            let from = SlotId(i);
            let log = region.log(from);
            let sent = log.sent.load(Ordering::Acquire);
            let behind = sent.wrapping_sub(*next);
            let overwritten = behind.saturating_sub(LOG_LEN as u32);
            let mut lost = overwritten;
            for back in 0..behind - overwritten {
                let sequence = next.wrapping_add(overwritten).wrapping_add(back);
                match log.entries[sequence as usize % LOG_LEN].read(sequence) {
                    None => lost += 1,
                    Some(command) if !command.targets.contains(self.me) => {}
                    Some(Sent { sent_ms, text, .. }) => {
                        let age_ms = now_ms.saturating_sub(sent_ms);
                        deliveries.push(if age_ms > COMMAND_TIMEOUT_MS { Delivery::TooLate { from, text, age_ms } } else { Delivery::Run { from, text, age_ms } });
                    }
                }
            }
            if lost > 0 {
                deliveries.push(Delivery::Lost { from, count: lost });
            }
            *next = sent;
        }
        deliveries
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::board::tests::{Dead, owner, region, zeroed_memory};

    fn targets(slots: &[SlotId]) -> Targets {
        let mut targets = Targets::default();
        slots.iter().for_each(|slot| targets.add(*slot));
        targets
    }

    fn claim(region: &Region, pid: u32) -> SlotId {
        region.claim(owner(pid), &mut Dead::default()).unwrap()
    }

    fn run(from: SlotId, text: &str) -> Delivery {
        Delivery::Run { from, text: text.to_owned(), age_ms: 0 }
    }

    #[test]
    fn a_command_reaches_only_the_slots_addressed_and_only_once() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob, carol) = (claim(region, 1), claim(region, 2), claim(region, 3));
        let (mut bobs, mut carols) = (Inbox::new(region, bob), Inbox::new(region, carol));
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(carols.receive(region, 100), []);
        assert_eq!(bobs.receive(region, 100), [run(sender, "/sit")]);
        assert_eq!(bobs.receive(region, 100), []);
    }

    #[test]
    fn commands_arrive_in_the_order_sent() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        for text in ["/first", "/second", "/third"] {
            region.send(sender, targets(&[bob]), 100, text).unwrap();
        }
        assert_eq!(inbox.receive(region, 100), [run(sender, "/first"), run(sender, "/second"), run(sender, "/third")]);
    }

    #[test]
    fn a_new_inbox_skips_what_was_sent_before_it() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        region.send(sender, targets(&[bob]), 100, "/old").unwrap();
        let mut inbox = Inbox::new(region, bob);
        assert_eq!(inbox.receive(region, 100), []);
        region.send(sender, targets(&[bob]), 100, "/new").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, "/new")]);
    }

    #[test]
    fn a_command_past_the_timeout_is_too_late_and_at_it_is_not() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(inbox.receive(region, 100 + COMMAND_TIMEOUT_MS), [Delivery::Run { from: sender, text: "/sit".to_owned(), age_ms: COMMAND_TIMEOUT_MS }]);
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        let age_ms = COMMAND_TIMEOUT_MS + 1;
        assert_eq!(inbox.receive(region, 100 + age_ms), [Delivery::TooLate { from: sender, text: "/sit".to_owned(), age_ms }]);
    }

    #[test]
    fn a_reader_that_falls_behind_is_told_and_gets_what_remains() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        for n in 0..LOG_LEN + 3 {
            region.send(sender, targets(&[bob]), 100, &format!("/cmd {n}")).unwrap();
        }
        let deliveries = inbox.receive(region, 100);
        assert_eq!(deliveries.len(), LOG_LEN + 1);
        assert_eq!(deliveries[0], run(sender, "/cmd 3"));
        assert_eq!(deliveries[LOG_LEN - 1], run(sender, &format!("/cmd {}", LOG_LEN + 2)));
        assert_eq!(deliveries[LOG_LEN], Delivery::Lost { from: sender, count: 3 });
        assert_eq!(inbox.receive(region, 100), []);
    }

    #[test]
    fn the_longest_command_arrives_whole_and_a_longer_one_is_refused() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        let longest = format!("/{}", "x".repeat(MAX_COMMAND_LEN - 1));
        region.send(sender, targets(&[bob]), 100, &longest).unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, &longest)]);
        assert_eq!(region.send(sender, targets(&[bob]), 100, &format!("{longest}x")), Err(CommandTooLong));
        assert_eq!(inbox.receive(region, 100), []);
    }

    #[test]
    fn text_shorter_than_an_earlier_command_in_the_entry_shows_none_of_it() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        for _ in 0..LOG_LEN {
            region.send(sender, targets(&[bob]), 100, "/a longer command").unwrap();
        }
        inbox.receive(region, 100);
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, "/sit")]);
    }

    #[test]
    fn the_highest_slot_can_be_addressed() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let sender = claim(region, 1);
        let last = SlotId(MAX_CLIENTS - 1);
        let mut inbox = Inbox::new(region, last);
        let mut others = Inbox::new(region, SlotId(MAX_CLIENTS - 2));
        region.send(sender, targets(&[last]), 100, "/sit").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, "/sit")]);
        assert_eq!(others.receive(region, 100), []);
    }

    #[test]
    fn a_slot_taken_over_keeps_its_place_in_the_log() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        region.send(sender, targets(&[bob]), 100, "/before").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, "/before")]);

        let mut dead = Dead::default();
        dead.0.insert(owner(1));
        assert_eq!(region.claim(owner(3), &mut dead).unwrap(), sender);
        assert_eq!(inbox.receive(region, 100), [], "what the last owner sent must not run twice");
        region.send(sender, targets(&[bob]), 100, "/after").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, "/after")]);
    }

    #[test]
    fn a_sender_that_died_mid_write_leaves_nothing_to_read_and_the_next_owner_can_send() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        region.log(sender).entries[0].stamp.store(0, Ordering::Relaxed);
        region.log(sender).entries[0].len.store(3, Ordering::Relaxed);
        assert_eq!(inbox.receive(region, 100), []);
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, "/sit")]);
    }

    /// A command whose text can be checked as all from one write.
    fn patterned(n: u32) -> String {
        let fill = char::from(b'a' + (n % 26) as u8);
        format!("/{}", fill.to_string().repeat(100 + (n as usize % 26) * 40))
    }

    fn is_torn(text: &str) -> bool {
        let fill = text.as_bytes()[1];
        text.len() != 101 + usize::from(fill - b'a') * 40 || text.bytes().skip(1).any(|byte| byte != fill)
    }

    /// Runs `read` against a thread that sends as fast as it can, until `read`
    /// sees a torn command or the time is up. Returns whether it did.
    fn saw_torn_command_while_sending(region: &Region, sender: SlotId, to: SlotId, limit: Duration, mut read: impl FnMut() -> Vec<String>) -> bool {
        let stop = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut n = 0;
                while !stop.load(Ordering::Relaxed) {
                    region.send(sender, targets(&[to]), 100, &patterned(n)).unwrap();
                    n = n.wrapping_add(1);
                }
            });
            let started = Instant::now();
            let mut torn = false;
            while !torn && started.elapsed() < limit {
                torn = read().iter().any(|text| is_torn(text));
            }
            stop.store(true, Ordering::Relaxed);
            torn
        })
    }

    #[test]
    fn reads_never_see_a_torn_command() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        let read = || {
            let deliveries = inbox.receive(region, 100);
            deliveries.into_iter().filter_map(|delivery| if let Delivery::Run { text, .. } = delivery { Some(text) } else { None }).collect()
        };
        assert!(!saw_torn_command_while_sending(region, sender, bob, Duration::from_secs(1), read));
    }

    /// The negative for `reads_never_see_a_torn_command`: without the stamp
    /// check the same writer does tear a read, so that test can catch it.
    #[test]
    fn a_read_without_the_stamp_check_does_see_a_torn_command() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        region.send(sender, targets(&[bob]), 100, &patterned(0)).unwrap();
        let log = region.log(sender);
        let unchecked = || {
            let entry = &log.entries[log.sent.load(Ordering::Relaxed) as usize % LOG_LEN];
            let len = (entry.len.load(Ordering::Relaxed) as usize).min(MAX_COMMAND_LEN);
            let bytes: Vec<u8> = entry.text[..len.div_ceil(4)].iter().flat_map(|word| word.load(Ordering::Relaxed).to_le_bytes()).take(len).collect();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if text.len() > 1 && text.as_bytes()[1].is_ascii_lowercase() { vec![text] } else { Vec::new() }
        };
        assert!(saw_torn_command_while_sending(region, sender, bob, Duration::from_secs(30), unchecked));
    }

    #[test]
    fn the_audience_for_a_zone_leaves_out_other_zones_instances_and_zoning_members() {
        let audience = Audience::Zone { zone: "qeynos2".to_owned(), instance: 3 };
        let mut status = Status::default();
        status.set_zone("QEYNOS2");
        status.zone_instance = 3;
        assert!(audience.includes(&status));
        status.zone_instance = 4;
        assert!(!audience.includes(&status), "another instance");
        status.zone_instance = 3;
        status.set_state(State::Zoning);
        assert!(!audience.includes(&status), "zoning");
        status.set_state(State::InGame);
        status.set_zone("qeynos");
        assert!(!audience.includes(&status), "another zone");
    }

    #[test]
    fn the_audience_of_names_matches_in_any_case() {
        let audience = Audience::Names(vec!["Bob".to_owned()]);
        let mut status = Status::default();
        status.set_name("BOB");
        assert!(audience.includes(&status));
        status.set_name("Bobby");
        assert!(!audience.includes(&status));
    }

    #[test]
    fn every_command_in_help_is_in_the_readme_with_the_same_description() {
        let readme = include_str!("../../../README.md");
        let aliases = ALIASES.iter().map(|alias| (format!("{} <command>", alias.name), alias.description));
        let subcommands = SUBCOMMANDS.iter().map(|(usage, description)| ((*usage).to_owned(), *description));
        for (usage, description) in aliases.chain(subcommands) {
            let row = format!("| `{usage}` | {description} |");
            assert!(readme.contains(&row), "the README needs the row: {row}");
        }
    }

    #[test]
    fn the_shorthand_in_each_alias_name_matches_what_it_does() {
        for alias in &ALIASES {
            let letters = alias.name.strip_prefix("/p").unwrap();
            let (scope_letter, rest) = letters.split_once('c').unwrap();
            let scope = match scope_letter {
                "" => Scope::All,
                "g" => Scope::Group,
                "r" => Scope::Raid,
                "z" => Scope::Zone,
                other => panic!("unknown scope letter {other}"),
            };
            assert_eq!((alias.scope, alias.includes_sender), (scope, rest == "a"), "{}", alias.name);
        }
    }
}
