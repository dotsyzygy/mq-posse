//! Commands sent between clients, and the slash commands that send them.
//!
//! A client writes commands only to the log in its own slot, each addressed to
//! the slots it's for, and every reader keeps its own place in each log. So one
//! write reaches any number of clients, and a client that stops can't hold up
//! another. A log's oldest commands are overwritten, and a reader that falls
//! that far behind can tell.
//!
//! A target answers in its own slot, one reply per sender, so the sender can see
//! what happened to the last command it sent, and how many earlier ones were
//! dropped or lost.

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
    /// One character, named before the command.
    Name,
    All,
    Group,
    Raid,
    Zone,
    /// A named channel, given before the command.
    Channel,
}

/// A slash command that sends a command to other clients.
#[derive(Debug)]
pub struct Alias {
    pub name: &'static str,
    pub scope: Scope,
    /// Whether the sender runs it too. For [`Scope::Name`] and [`Scope::Channel`],
    /// only if it's that character or in that channel.
    pub includes_sender: bool,
    pub description: &'static str,
}

impl Alias {
    /// As `/posse help` shows it.
    pub fn usage(&self) -> String {
        match self.scope {
            Scope::Name => format!("{} <name> <command>", self.name),
            Scope::Channel => format!("{} <channel> <command>", self.name),
            _ => format!("{} <command>", self.name),
        }
    }
}

pub static ALIASES: [Alias; 11] = [
    Alias {
        name: "/pc",
        scope: Scope::Name,
        includes_sender: true,
        description: "Runs a command on that character, which can be you",
    },
    Alias {
        name: "/pca",
        scope: Scope::All,
        includes_sender: false,
        description: "Runs a command on every connected client except you",
    },
    Alias {
        name: "/pcaa",
        scope: Scope::All,
        includes_sender: true,
        description: "Runs a command on every connected client, including you",
    },
    Alias {
        name: "/pgc",
        scope: Scope::Group,
        includes_sender: false,
        description: "Runs a command on every client in your group except you",
    },
    Alias {
        name: "/pgca",
        scope: Scope::Group,
        includes_sender: true,
        description: "Runs a command on every client in your group, including you",
    },
    Alias {
        name: "/prc",
        scope: Scope::Raid,
        includes_sender: false,
        description: "Runs a command on every client in your raid except you, your group included",
    },
    Alias {
        name: "/prca",
        scope: Scope::Raid,
        includes_sender: true,
        description: "Runs a command on every client in your raid, including you",
    },
    Alias {
        name: "/pzc",
        scope: Scope::Zone,
        includes_sender: false,
        description: "Runs a command on every client in your zone except you",
    },
    Alias {
        name: "/pzca",
        scope: Scope::Zone,
        includes_sender: true,
        description: "Runs a command on every client in your zone, including you",
    },
    Alias {
        name: "/pnc",
        scope: Scope::Channel,
        includes_sender: false,
        description: "Runs a command on every client in that channel except you",
    },
    Alias {
        name: "/pnca",
        scope: Scope::Channel,
        includes_sender: true,
        description: "Runs a command on every client in that channel, including you if you're in it",
    },
];

/// A group of commands that `/posse help <topic>` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpTopic {
    Posse,
    Channel,
    Settings,
    /// The aliases that send commands to other clients, from [`ALIASES`].
    Command,
}

impl HelpTopic {
    pub const ALL: [HelpTopic; 4] = [HelpTopic::Posse, HelpTopic::Channel, HelpTopic::Settings, HelpTopic::Command];

    /// As typed after `/posse help`.
    pub fn name(self) -> &'static str {
        match self {
            HelpTopic::Posse => "posse",
            HelpTopic::Channel => "channel",
            HelpTopic::Settings => "settings",
            HelpTopic::Command => "command",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            HelpTopic::Posse => "Listing members, and joining and leaving posses",
            HelpTopic::Channel => "Creating, deleting, joining and leaving channels",
            HelpTopic::Settings => "Debugging, allowing commands, and stats",
            HelpTopic::Command => "Running commands on other clients",
        }
    }

    pub fn parse(name: &str) -> Option<HelpTopic> {
        HelpTopic::ALL.into_iter().find(|topic| topic.name().eq_ignore_ascii_case(name))
    }
}

/// `/posse help` itself, which lists the topics rather than belonging to one.
pub const HELP: [(&str, &str); 3] = [
    ("/posse help", "Lists the help topics"),
    ("/posse help all", "Lists every command"),
    (
        "/posse help <topic>",
        "Lists the commands for that topic: posse, channel, settings or command",
    ),
];

/// The `/posse` subcommands that work so far, by the topic `/posse help` lists them under.
pub const SUBCOMMANDS: [(HelpTopic, &str, &str); 14] = [
    (
        HelpTopic::Posse,
        "/posse list",
        "Lists members with their state and data age, as does /posse alone",
    ),
    (
        HelpTopic::Posse,
        "/posse join <name>",
        "Leaves the current posse and joins that one, until you next go to character select",
    ),
    (
        HelpTopic::Posse,
        "/posse leave",
        "Leaves the posse until the next /posse join",
    ),
    (
        HelpTopic::Channel,
        "/posse channel list",
        "Lists every channel and who's in it, as does /posse channel alone",
    ),
    (
        HelpTopic::Channel,
        "/posse channel create <name>",
        "Creates a channel, which stays until it's deleted",
    ),
    (
        HelpTopic::Channel,
        "/posse channel delete <name>",
        "Deletes a channel once nobody online is in it",
    ),
    (
        HelpTopic::Channel,
        "/posse channel join <name>",
        "Puts this character in a channel, and keeps it there",
    ),
    (
        HelpTopic::Channel,
        "/posse channel leave <name>",
        "Takes this character out of a channel, and keeps it out",
    ),
    (
        HelpTopic::Settings,
        "/posse debug on",
        "Logs debugging information to the MQ console for this character",
    ),
    (
        HelpTopic::Settings,
        "/posse debug off",
        "Turns off debugging",
    ),
    (
        HelpTopic::Settings,
        "/posse allowcommands true",
        "Runs commands other clients send, and keeps doing so for this character",
    ),
    (
        HelpTopic::Settings,
        "/posse allowcommands false",
        "Refuses commands other clients send, and keeps doing so for this character",
    ),
    (
        HelpTopic::Settings,
        "/posse stats",
        "Shows publishing costs for client per pulse, member status age, and how long commands took to reach this client",
    ),
    (
        HelpTopic::Settings,
        "/posse stats reset",
        "Reset the stats",
    ),
];

/// Which members a command is for, as the sender sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    All,
    /// The sender's group or raid.
    Names(Vec<String>),
    /// In game in this zone. Zoning members have left it, wherever they're headed.
    Zone {
        zone: String,
        instance: u32,
    },
    Channel(String),
}

impl Audience {
    pub fn includes(&self, status: &Status) -> bool {
        match self {
            Audience::All => true,
            Audience::Names(names) => {
                let name = status.name();
                names.iter().any(|other| other.eq_ignore_ascii_case(&name))
            }
            Audience::Zone { zone, instance } => {
                status.state() == State::InGame
                    && status.zone_instance == *instance
                    && status.zone().eq_ignore_ascii_case(zone)
            }
            Audience::Channel(channel) => status.in_channel(channel),
        }
    }
}

/// The slots a command is addressed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Targets(pub(crate) u64);

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

    fn slots(self) -> impl Iterator<Item = SlotId> {
        (0..MAX_CLIENTS)
            .map(SlotId)
            .filter(move |slot| self.contains(*slot))
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
        let targets = u64::from(self.targets_high.load(Ordering::Relaxed)) << 32
            | u64::from(self.targets_low.load(Ordering::Relaxed));
        let sent_ms = u64::from(self.sent_ms_high.load(Ordering::Relaxed)) << 32
            | u64::from(self.sent_ms_low.load(Ordering::Relaxed));
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
        Some(Sent {
            targets: Targets(targets),
            sent_ms,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

impl Region {
    /// Writes a command to the log of a slot this client holds, returning its
    /// sequence number.
    pub fn send(
        &self,
        from: SlotId,
        targets: Targets,
        sent_ms: u64,
        text: &str,
    ) -> Result<u32, CommandTooLong> {
        if text.len() > MAX_COMMAND_LEN {
            return Err(CommandTooLong);
        }
        let log = self.log(from);
        let sequence = log.sent.load(Ordering::Relaxed);
        let entry = &log.entries[sequence as usize % LOG_LEN];
        entry.stamp.store(0, Ordering::Relaxed);
        fence(Ordering::Release);
        entry.targets_low.store(targets.0 as u32, Ordering::Relaxed);
        entry
            .targets_high
            .store((targets.0 >> 32) as u32, Ordering::Relaxed);
        entry.sent_ms_low.store(sent_ms as u32, Ordering::Relaxed);
        entry
            .sent_ms_high
            .store((sent_ms >> 32) as u32, Ordering::Relaxed);
        entry.len.store(text.len() as u32, Ordering::Relaxed);
        for (word, chunk) in entry.text.iter().zip(text.as_bytes().chunks(4)) {
            let mut bytes = [0; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            word.store(u32::from_le_bytes(bytes), Ordering::Relaxed);
        }
        entry
            .stamp
            .store(sequence.wrapping_add(1), Ordering::Release);
        log.sent.store(sequence.wrapping_add(1), Ordering::Release);
        Ok(sequence)
    }

    /// Tells the client in `from` what the client in `me` did with a command.
    pub fn answer(&self, me: SlotId, from: SlotId, sequence: u32, answer: Answer) {
        let reply = self.reply(me, from);
        reply
            .last
            .store(packed_answer(sequence, answer), Ordering::Release);
        if answer == Answer::Dropped {
            reply.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// What a target did with a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Ran,
    /// Refused because of `AllowCommands`, or MacroQuest wouldn't take it.
    Refused,
    /// It arrived too late to run.
    Dropped,
}

const ANSWER_BITS: u32 = 2;

/// An answer and which command it's for, in one word so they can't be read
/// apart. The sequence number loses its top bits, which only matters if another
/// answer is 2^30 commands out of date.
fn packed_answer(sequence: u32, answer: Answer) -> u32 {
    let code = match answer {
        Answer::Ran => 1,
        Answer::Refused => 2,
        Answer::Dropped => 3,
    };
    sequence.wrapping_add(1) << ANSWER_BITS | code
}

fn unpacked_answer(word: u32, sequence: u32) -> Option<Answer> {
    if word >> ANSWER_BITS != sequence.wrapping_add(1) << ANSWER_BITS >> ANSWER_BITS {
        return None;
    }
    match word & ((1 << ANSWER_BITS) - 1) {
        1 => Some(Answer::Ran),
        2 => Some(Answer::Refused),
        3 => Some(Answer::Dropped),
        _ => None,
    }
}

/// One target's report to one sender.
#[repr(C)]
pub(crate) struct Reply {
    /// The answer to the latest command from that sender addressed to this
    /// client, or 0 for none yet.
    last: AtomicU32,
    /// Running totals of that sender's commands dropped, and of commands
    /// overwritten before this client read them, addressed to it or not.
    dropped: AtomicU32,
    lost: AtomicU32,
}

/// The command this client last sent, for `${Posse.LastCommand}`.
#[derive(Debug)]
pub struct LastCommand {
    text: String,
    sequence: u32,
    targets: Targets,
    sent_ms: u64,
    /// How the sender's own copy went, if it ran one.
    here: Option<Answer>,
    lost: u32,
}

/// What's become of [`LastCommand`], with each target counted once in one of
/// `pending`, `done`, `refused`, `dropped` or `timed_out`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub targets: u32,
    pub pending: u32,
    pub done: u32,
    pub refused: u32,
    pub dropped: u32,
    /// Past [`COMMAND_TIMEOUT_MS`] with no answer. A target can still move from
    /// here to `dropped` when it gets to the command.
    pub timed_out: u32,
    /// Commands reported lost since this one was sent. That's a count of commands
    /// and not of targets, since a log only overwrites older commands than this.
    pub lost: u32,
}

impl Tally {
    fn add(&mut self, answer: Answer) {
        match answer {
            Answer::Ran => self.done += 1,
            Answer::Refused => self.refused += 1,
            Answer::Dropped => self.dropped += 1,
        }
    }
}

impl LastCommand {
    pub fn new(
        text: &str,
        sequence: u32,
        targets: Targets,
        sent_ms: u64,
        here: Option<Answer>,
    ) -> LastCommand {
        LastCommand {
            text: text.to_owned(),
            sequence,
            targets,
            sent_ms,
            here,
            lost: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Every target that lost commands lost the oldest ones in the same log, so
    /// the commands lost are those of the target that lost the most, and not the
    /// sum of what each reports.
    pub fn add_lost(&mut self, count: u32) {
        self.lost = self.lost.max(count);
    }

    /// `me` is the slot it was sent from.
    pub fn tally(&self, region: &Region, me: SlotId, now_ms: u64) -> Tally {
        let mut tally = Tally {
            lost: self.lost,
            ..Tally::default()
        };
        if let Some(answer) = self.here {
            tally.targets += 1;
            tally.add(answer);
        }
        for slot in self.targets.slots() {
            tally.targets += 1;
            match unpacked_answer(
                region.reply(slot, me).last.load(Ordering::Acquire),
                self.sequence,
            ) {
                Some(answer) => tally.add(answer),
                None if now_ms.saturating_sub(self.sent_ms) > COMMAND_TIMEOUT_MS => {
                    tally.timed_out += 1
                }
                None => tally.pending += 1,
            }
        }
        tally
    }
}

/// A client's report of commands from this one it didn't run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Complaint {
    pub from: SlotId,
    pub dropped: u32,
    pub lost: u32,
}

/// How much of what other clients have reported this one has seen.
#[derive(Debug)]
pub struct Complaints {
    me: SlotId,
    seen: [(u32, u32); MAX_CLIENTS],
}

impl Complaints {
    /// Starts after everything reported so far, which was about whoever was here
    /// then.
    pub fn new(region: &Region, me: SlotId) -> Complaints {
        Complaints {
            me,
            seen: std::array::from_fn(|i| {
                let reply = region.reply(SlotId(i), me);
                (
                    reply.dropped.load(Ordering::Relaxed),
                    reply.lost.load(Ordering::Relaxed),
                )
            }),
        }
    }

    /// What's been reported since the last call.
    pub fn check(&mut self, region: &Region) -> Vec<Complaint> {
        let mut complaints = Vec::new();
        for (i, seen) in self.seen.iter_mut().enumerate() {
            let from = SlotId(i);
            let reply = region.reply(from, self.me);
            let now = (
                reply.dropped.load(Ordering::Relaxed),
                reply.lost.load(Ordering::Relaxed),
            );
            if now != *seen {
                complaints.push(Complaint {
                    from,
                    dropped: now.0.wrapping_sub(seen.0),
                    lost: now.1.wrapping_sub(seen.1),
                });
                *seen = now;
            }
        }
        complaints
    }
}

/// What a client finds addressed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `age_ms` is how long it took to get here. Tell the sender what became of
    /// it with [`Region::answer`] and `sequence`.
    Run {
        from: SlotId,
        sequence: u32,
        text: String,
        age_ms: u64,
    },
    /// Sent over [`COMMAND_TIMEOUT_MS`] ago, so it shouldn't run. The sender has
    /// been told.
    TooLate {
        from: SlotId,
        text: String,
        age_ms: u64,
    },
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
        Inbox {
            me,
            next: std::array::from_fn(|i| region.log(SlotId(i)).sent.load(Ordering::Acquire)),
        }
    }

    /// What's arrived since the last call, oldest first from each sender. This
    /// also tells each sender what was dropped or lost.
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
                        if age_ms > COMMAND_TIMEOUT_MS {
                            region.answer(self.me, from, sequence, Answer::Dropped);
                            deliveries.push(Delivery::TooLate { from, text, age_ms });
                        } else {
                            deliveries.push(Delivery::Run {
                                from,
                                sequence,
                                text,
                                age_ms,
                            });
                        }
                    }
                }
            }
            if lost > 0 {
                region
                    .reply(self.me, from)
                    .lost
                    .fetch_add(lost, Ordering::Relaxed);
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

    fn run(from: SlotId, sequence: u32, text: &str) -> Delivery {
        Delivery::Run {
            from,
            sequence,
            text: text.to_owned(),
            age_ms: 0,
        }
    }

    #[test]
    fn a_command_reaches_only_the_slots_addressed_and_only_once() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob, carol) = (claim(region, 1), claim(region, 2), claim(region, 3));
        let (mut bobs, mut carols) = (Inbox::new(region, bob), Inbox::new(region, carol));
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(carols.receive(region, 100), []);
        assert_eq!(bobs.receive(region, 100), [run(sender, 0, "/sit")]);
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
        assert_eq!(
            inbox.receive(region, 100),
            [
                run(sender, 0, "/first"),
                run(sender, 1, "/second"),
                run(sender, 2, "/third")
            ]
        );
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
        assert_eq!(inbox.receive(region, 100), [run(sender, 1, "/new")]);
    }

    #[test]
    fn a_command_past_the_timeout_is_too_late_and_at_it_is_not() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(
            inbox.receive(region, 100 + COMMAND_TIMEOUT_MS),
            [Delivery::Run {
                from: sender,
                sequence: 0,
                text: "/sit".to_owned(),
                age_ms: COMMAND_TIMEOUT_MS
            }]
        );
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        let age_ms = COMMAND_TIMEOUT_MS + 1;
        assert_eq!(
            inbox.receive(region, 100 + age_ms),
            [Delivery::TooLate {
                from: sender,
                text: "/sit".to_owned(),
                age_ms
            }]
        );
    }

    #[test]
    fn a_reader_that_falls_behind_is_told_and_gets_what_remains() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        for n in 0..LOG_LEN + 3 {
            region
                .send(sender, targets(&[bob]), 100, &format!("/cmd {n}"))
                .unwrap();
        }
        let deliveries = inbox.receive(region, 100);
        assert_eq!(deliveries.len(), LOG_LEN + 1);
        assert_eq!(deliveries[0], run(sender, 3, "/cmd 3"));
        assert_eq!(
            deliveries[LOG_LEN - 1],
            run(sender, LOG_LEN as u32 + 2, &format!("/cmd {}", LOG_LEN + 2))
        );
        assert_eq!(
            deliveries[LOG_LEN],
            Delivery::Lost {
                from: sender,
                count: 3
            }
        );
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
        assert_eq!(inbox.receive(region, 100), [run(sender, 0, &longest)]);
        assert_eq!(
            region.send(sender, targets(&[bob]), 100, &format!("{longest}x")),
            Err(CommandTooLong)
        );
        assert_eq!(inbox.receive(region, 100), []);
    }

    #[test]
    fn text_shorter_than_an_earlier_command_in_the_entry_shows_none_of_it() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        for _ in 0..LOG_LEN {
            region
                .send(sender, targets(&[bob]), 100, "/a longer command")
                .unwrap();
        }
        inbox.receive(region, 100);
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(
            inbox.receive(region, 100),
            [run(sender, LOG_LEN as u32, "/sit")]
        );
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
        assert_eq!(inbox.receive(region, 100), [run(sender, 0, "/sit")]);
        assert_eq!(others.receive(region, 100), []);
    }

    #[test]
    fn a_slot_taken_over_keeps_its_place_in_the_log() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        region
            .send(sender, targets(&[bob]), 100, "/before")
            .unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, 0, "/before")]);

        let mut dead = Dead::default();
        dead.0.insert(owner(1));
        assert_eq!(region.claim(owner(3), &mut dead).unwrap(), sender);
        assert_eq!(
            inbox.receive(region, 100),
            [],
            "what the last owner sent must not run twice"
        );
        region.send(sender, targets(&[bob]), 100, "/after").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, 1, "/after")]);
    }

    #[test]
    fn a_sender_that_died_mid_write_leaves_nothing_to_read_and_the_next_owner_can_send() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        region.log(sender).entries[0]
            .stamp
            .store(0, Ordering::Relaxed);
        region.log(sender).entries[0]
            .len
            .store(3, Ordering::Relaxed);
        assert_eq!(inbox.receive(region, 100), []);
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        assert_eq!(inbox.receive(region, 100), [run(sender, 0, "/sit")]);
    }

    /// Sends a command to `slots` at time 100 and tracks it.
    fn tracked(region: &Region, sender: SlotId, slots: &[SlotId]) -> LastCommand {
        let sequence = region.send(sender, targets(slots), 100, "/sit").unwrap();
        LastCommand::new("/sit", sequence, targets(slots), 100, None)
    }

    #[test]
    fn a_tally_starts_pending_and_counts_each_answer_once() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob, carol) = (claim(region, 1), claim(region, 2), claim(region, 3));
        let dave = claim(region, 4);
        let (mut bobs, mut carols) = (Inbox::new(region, bob), Inbox::new(region, carol));
        let last = tracked(region, sender, &[bob, carol, dave]);
        assert_eq!(
            last.tally(region, sender, 100),
            Tally {
                targets: 3,
                pending: 3,
                ..Tally::default()
            }
        );

        let [Delivery::Run { sequence, .. }] = bobs.receive(region, 100)[..] else {
            panic!("Bob should have it")
        };
        assert_eq!(
            last.tally(region, sender, 100).pending,
            3,
            "received isn't answered"
        );
        region.answer(bob, sender, sequence, Answer::Ran);
        let [Delivery::Run { sequence, .. }] = carols.receive(region, 100)[..] else {
            panic!("Carol should have it")
        };
        region.answer(carol, sender, sequence, Answer::Refused);
        assert_eq!(
            last.tally(region, sender, 100),
            Tally {
                targets: 3,
                pending: 1,
                done: 1,
                refused: 1,
                ..Tally::default()
            }
        );
    }

    #[test]
    fn a_target_silent_past_the_timeout_is_timed_out_and_at_it_is_not() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let last = tracked(region, sender, &[bob]);
        assert_eq!(
            last.tally(region, sender, 100 + COMMAND_TIMEOUT_MS).pending,
            1
        );
        assert_eq!(
            last.tally(region, sender, 101 + COMMAND_TIMEOUT_MS),
            Tally {
                targets: 1,
                timed_out: 1,
                ..Tally::default()
            }
        );
    }

    #[test]
    fn a_command_read_too_late_goes_from_timed_out_to_dropped_and_is_complained_about() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        let mut complaints = Complaints::new(region, sender);
        let last = tracked(region, sender, &[bob]);
        let late = 101 + COMMAND_TIMEOUT_MS;
        assert_eq!(last.tally(region, sender, late).timed_out, 1);
        inbox.receive(region, late);
        assert_eq!(
            last.tally(region, sender, late),
            Tally {
                targets: 1,
                dropped: 1,
                ..Tally::default()
            }
        );
        assert_eq!(
            complaints.check(region),
            [Complaint {
                from: bob,
                dropped: 1,
                lost: 0
            }]
        );
        assert_eq!(complaints.check(region), [], "told once");
    }

    #[test]
    fn a_command_in_time_is_not_complained_about() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        let mut complaints = Complaints::new(region, sender);
        tracked(region, sender, &[bob]);
        inbox.receive(region, 100 + COMMAND_TIMEOUT_MS);
        assert_eq!(complaints.check(region), []);
    }

    #[test]
    fn an_answer_to_an_older_command_does_not_count_for_the_newer_one() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let older = region.send(sender, targets(&[bob]), 100, "/stand").unwrap();
        let last = tracked(region, sender, &[bob]);
        region.answer(bob, sender, older, Answer::Ran);
        assert_eq!(last.tally(region, sender, 100).pending, 1);
    }

    #[test]
    fn an_answer_to_another_sender_does_not_count() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, other, bob) = (claim(region, 1), claim(region, 2), claim(region, 3));
        let last = tracked(region, sender, &[bob]);
        region.answer(bob, other, 0, Answer::Ran);
        assert_eq!(last.tally(region, sender, 100).pending, 1);
    }

    #[test]
    fn answers_match_across_sequence_numbers_wrapping_and_the_bits_they_lose() {
        for sequence in [u32::MAX - 1, u32::MAX, 0, 1 << 30] {
            for answer in [Answer::Ran, Answer::Refused, Answer::Dropped] {
                assert_eq!(
                    unpacked_answer(packed_answer(sequence, answer), sequence),
                    Some(answer),
                    "{sequence}"
                );
            }
            assert_eq!(
                unpacked_answer(
                    packed_answer(sequence, Answer::Ran),
                    sequence.wrapping_add(1)
                ),
                None,
                "{sequence}"
            );
        }
        assert_eq!(
            unpacked_answer(0, u32::MAX),
            None,
            "no answer yet isn't an answer to the command that wraps to 0"
        );
    }

    #[test]
    fn a_client_taking_over_a_targets_slot_does_not_answer_for_it() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let last = tracked(region, sender, &[bob]);
        let mut dead = Dead::default();
        dead.0.insert(owner(2));
        assert_eq!(region.claim(owner(3), &mut dead).unwrap(), bob);
        assert_eq!(Inbox::new(region, bob).receive(region, 100), []);
        assert_eq!(last.tally(region, sender, 100).pending, 1);
        assert_eq!(
            last.tally(region, sender, 101 + COMMAND_TIMEOUT_MS)
                .timed_out,
            1
        );
    }

    #[test]
    fn the_senders_own_copy_counts_as_a_target() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let sender = claim(region, 1);
        let last = LastCommand::new("/sit", 0, Targets::default(), 100, Some(Answer::Ran));
        assert_eq!(
            last.tally(region, sender, 100),
            Tally {
                targets: 1,
                done: 1,
                ..Tally::default()
            }
        );
        let last = LastCommand::new("/sit", 0, Targets::default(), 100, Some(Answer::Refused));
        assert_eq!(
            last.tally(region, sender, 100),
            Tally {
                targets: 1,
                refused: 1,
                ..Tally::default()
            }
        );
    }

    #[test]
    fn lost_commands_are_complained_about_with_the_last_commands_answer_still_good() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        let mut complaints = Complaints::new(region, sender);
        for _ in 0..LOG_LEN + 3 {
            region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        }
        let mut last = tracked(region, sender, &[bob]);
        let deliveries = inbox.receive(region, 100);
        let Some(Delivery::Run { sequence, .. }) = deliveries
            .iter()
            .rfind(|delivery| matches!(delivery, Delivery::Run { .. }))
        else {
            panic!("the last command should arrive")
        };
        region.answer(bob, sender, *sequence, Answer::Ran);

        let found = complaints.check(region);
        assert_eq!(
            found,
            [Complaint {
                from: bob,
                dropped: 0,
                lost: 4
            }]
        );
        last.add_lost(found[0].lost);
        assert_eq!(
            last.tally(region, sender, 100),
            Tally {
                targets: 1,
                done: 1,
                lost: 4,
                ..Tally::default()
            }
        );
    }

    #[test]
    fn commands_lost_by_two_targets_are_counted_once() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob, cal) = (claim(region, 1), claim(region, 2), claim(region, 3));
        let mut inboxes = [Inbox::new(region, bob), Inbox::new(region, cal)];
        let mut complaints = Complaints::new(region, sender);
        for _ in 0..LOG_LEN + 3 {
            region.send(sender, targets(&[bob, cal]), 100, "/sit").unwrap();
        }
        let mut last = tracked(region, sender, &[bob, cal]);
        for inbox in &mut inboxes {
            inbox.receive(region, 100);
        }

        let found = complaints.check(region);
        assert_eq!(found.iter().map(|complaint| complaint.lost).collect::<Vec<_>>(), [4, 4]);
        for complaint in found {
            last.add_lost(complaint.lost);
        }
        assert_eq!(last.tally(region, sender, 100).lost, 4);
    }

    #[test]
    fn new_complaints_start_after_what_was_reported_to_whoever_was_here_before() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        let mut inbox = Inbox::new(region, bob);
        for _ in 0..LOG_LEN + 1 {
            region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        }
        inbox.receive(region, 100);
        assert_eq!(Complaints::new(region, sender).check(region), []);
    }

    #[test]
    fn a_complaint_goes_only_to_the_sender_it_is_about() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, other, bob) = (claim(region, 1), claim(region, 2), claim(region, 3));
        let mut inbox = Inbox::new(region, bob);
        let (mut senders, mut others) = (
            Complaints::new(region, sender),
            Complaints::new(region, other),
        );
        region.send(sender, targets(&[bob]), 100, "/sit").unwrap();
        inbox.receive(region, 101 + COMMAND_TIMEOUT_MS);
        assert_eq!(others.check(region), []);
        assert_eq!(senders.check(region).len(), 1);
    }

    /// A command whose text can be checked as all from one write.
    fn patterned(n: u32) -> String {
        let fill = char::from(b'a' + (n % 26) as u8);
        format!("/{}", fill.to_string().repeat(100 + (n as usize % 26) * 40))
    }

    fn is_torn(text: &str) -> bool {
        let fill = text.as_bytes()[1];
        text.len() != 101 + usize::from(fill - b'a') * 40
            || text.bytes().skip(1).any(|byte| byte != fill)
    }

    /// Runs `read` against a thread that sends as fast as it can, until `read`
    /// sees a torn command or the time is up. Returns whether it did.
    fn saw_torn_command_while_sending(
        region: &Region,
        sender: SlotId,
        to: SlotId,
        limit: Duration,
        mut read: impl FnMut() -> Vec<String>,
    ) -> bool {
        let stop = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut n = 0;
                while !stop.load(Ordering::Relaxed) {
                    region
                        .send(sender, targets(&[to]), 100, &patterned(n))
                        .unwrap();
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
            deliveries
                .into_iter()
                .filter_map(|delivery| {
                    if let Delivery::Run { text, .. } = delivery {
                        Some(text)
                    } else {
                        None
                    }
                })
                .collect()
        };
        assert!(!saw_torn_command_while_sending(
            region,
            sender,
            bob,
            Duration::from_secs(1),
            read
        ));
    }

    /// The negative for `reads_never_see_a_torn_command`: without the stamp
    /// check the same writer does tear a read, so that test can catch it.
    #[test]
    fn a_read_without_the_stamp_check_does_see_a_torn_command() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (sender, bob) = (claim(region, 1), claim(region, 2));
        region
            .send(sender, targets(&[bob]), 100, &patterned(0))
            .unwrap();
        let log = region.log(sender);
        let unchecked = || {
            let entry = &log.entries[log.sent.load(Ordering::Relaxed) as usize % LOG_LEN];
            let len = (entry.len.load(Ordering::Relaxed) as usize).min(MAX_COMMAND_LEN);
            let bytes: Vec<u8> = entry.text[..len.div_ceil(4)]
                .iter()
                .flat_map(|word| word.load(Ordering::Relaxed).to_le_bytes())
                .take(len)
                .collect();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if text.len() > 1 && text.as_bytes()[1].is_ascii_lowercase() {
                vec![text]
            } else {
                Vec::new()
            }
        };
        assert!(saw_torn_command_while_sending(
            region,
            sender,
            bob,
            Duration::from_secs(30),
            unchecked
        ));
    }

    #[test]
    fn the_audience_for_a_zone_leaves_out_other_zones_instances_and_zoning_members() {
        let audience = Audience::Zone {
            zone: "qeynos2".to_owned(),
            instance: 3,
        };
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
    fn the_audience_of_a_channel_is_its_members_in_any_case() {
        let audience = Audience::Channel("healers".to_owned());
        let mut status = Status::default();
        assert!(!audience.includes(&status));
        status.join_channel("Healers").unwrap();
        assert!(audience.includes(&status));
        status.leave_channel("healers");
        status.join_channel("healers2").unwrap();
        assert!(!audience.includes(&status), "not a match on part of a name");
    }

    /// A Markdown table's rows, with the padding that lines up its columns
    /// taken out.
    fn table_rows(markdown: &str) -> Vec<String> {
        markdown
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.split('|').map(str::trim).collect::<Vec<_>>().join(" | ").trim().to_owned())
            .collect()
    }

    #[test]
    fn a_padded_row_reads_the_same_as_an_unpadded_one_and_a_different_one_does_not() {
        let rows = table_rows("text\n| `/posse help`   | Lists these commands    |\n| a |\n");
        assert_eq!(rows, ["| `/posse help` | Lists these commands |", "| a |"]);
        assert!(!rows.contains(&"| `/posse help` | Lists those commands |".to_owned()));
    }

    #[test]
    fn every_command_in_help_is_in_the_readme_with_the_same_description() {
        let readme_rows = table_rows(include_str!("../../../README.md"));
        let aliases = ALIASES
            .iter()
            .map(|alias| (alias.usage(), alias.description));
        let subcommands = HELP
            .iter()
            .copied()
            .chain(SUBCOMMANDS.iter().map(|&(_, usage, description)| (usage, description)))
            .map(|(usage, description)| (usage.to_owned(), description));
        for (usage, description) in aliases.chain(subcommands) {
            let row = format!("| `{usage}` | {description} |");
            assert!(readme_rows.contains(&row), "the README needs the row: {row}");
        }
    }

    #[test]
    fn every_help_topic_parses_from_its_name_in_any_case_and_others_do_not() {
        for topic in HelpTopic::ALL {
            assert_eq!(HelpTopic::parse(topic.name()), Some(topic));
            assert_eq!(HelpTopic::parse(&topic.name().to_ascii_uppercase()), Some(topic));
        }
        assert_eq!(HelpTopic::parse("all"), None);
        assert_eq!(HelpTopic::parse("chan"), None);
    }

    #[test]
    fn every_topic_but_command_has_subcommands_and_command_has_none() {
        for topic in HelpTopic::ALL {
            let count = SUBCOMMANDS.iter().filter(|(t, ..)| *t == topic).count();
            assert_eq!(count == 0, topic == HelpTopic::Command, "{topic:?} has {count}");
        }
    }

    /// `/p`, a scope letter, `c`, then `a` to include the sender. With no scope
    /// letter, the command is to a name, and an `a` makes it to all.
    #[test]
    fn the_shorthand_in_each_alias_name_matches_what_it_does() {
        for alias in &ALIASES {
            let letters = alias.name.strip_prefix("/p").unwrap();
            let (scope_letter, rest) = letters.split_once('c').unwrap();
            let (scope, rest) = match (scope_letter, rest) {
                ("", "") => (Scope::Name, "a"),
                ("", rest) => (Scope::All, rest.strip_prefix('a').unwrap()),
                ("g", rest) => (Scope::Group, rest),
                ("r", rest) => (Scope::Raid, rest),
                ("z", rest) => (Scope::Zone, rest),
                ("n", rest) => (Scope::Channel, rest),
                (other, _) => panic!("unknown scope letter {other}"),
            };
            assert_eq!(
                (alias.scope, alias.includes_sender),
                (scope, rest == "a"),
                "{}",
                alias.name
            );
        }
    }
}
