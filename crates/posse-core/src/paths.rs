//! `Me` paths one client reads from another: `${Posse.Member[Bob].Inventory[mainhand].Name}`
//! is `${Me.Inventory[mainhand].Name}` on Bob's client.
//!
//! A reader lists the paths it wants, and from which slots, in its own slot.
//! Each client evaluates the paths wanted from it and writes the values in
//! its own slot, so every slot still has one writer. A reader copies values
//! into a cache of its own, which keeps the last value of a path it stopped
//! asking for.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering, fence};
use std::time::Duration;

use crate::board::{Liveness, Region, SlotId};
use crate::commands::Targets;

/// The longest path, in bytes.
pub const MAX_PATH_LEN: usize = PATH_WORDS * 4;
/// The longest value text kept, in bytes: a longer one is cut short.
pub const MAX_VALUE_LEN: usize = VALUE_WORDS * 4;
/// Paths a client can ask for at once, from any number of clients.
pub const MAX_REQUESTS: usize = 64;
/// Paths a client evaluates at once, for any number of clients.
pub const MAX_RESULTS: usize = 128;
/// A path not read for this long stops being asked for.
pub const UNREAD_AFTER_MS: u64 = 30_000;
/// A path is evaluated no more often than this...
pub const MIN_INTERVAL_MS: u64 = 100;
/// ...or than this many times what it took to evaluate it last time.
pub const COST_FACTOR: u64 = 10;

const PATH_WORDS: usize = 32;
const VALUE_WORDS: usize = 64;
/// Targets (2 words), the path's length and the path.
const REQUEST_WORDS: usize = 3 + PATH_WORDS;
/// Kind, path length, value length, when evaluated (2), a number (2), path and value.
const RESULT_WORDS: usize = 7 + PATH_WORDS + VALUE_WORDS;
const READ_TRIES: u32 = 32;

/// One step of a path: `Buff[3]` is `Buff` and `3`, `Target` is `Target` and `""`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub index: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidPath {
    Empty,
    TooLong,
    /// `${` inside: it would be parsed again on the client evaluating it.
    Substitution,
    /// A step's name isn't letters, digits and `_` starting with a letter.
    BadName(String),
    /// A step's index is empty, unclosed, or has brackets or control characters.
    BadIndex(String),
}

impl fmt::Display for InvalidPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidPath::Empty => write!(f, "the path is empty"),
            InvalidPath::TooLong => write!(f, "the path is longer than {MAX_PATH_LEN} bytes"),
            InvalidPath::Substitution => write!(f, "the path has ${{ in it"),
            InvalidPath::BadName(name) => write!(f, "{name:?} isn't a member name"),
            InvalidPath::BadIndex(index) => write!(f, "{index:?} isn't an index"),
        }
    }
}

/// Splits a path relative to `Me` (`Inventory[mainhand].Name`, `Buff[Spirit of Wolf].Duration`)
/// into its steps.
pub fn parse_path(path: &str) -> Result<Vec<Step>, InvalidPath> {
    if path.is_empty() {
        return Err(InvalidPath::Empty);
    }
    if path.len() > MAX_PATH_LEN {
        return Err(InvalidPath::TooLong);
    }
    if path.contains("${") {
        return Err(InvalidPath::Substitution);
    }
    let mut steps = Vec::new();
    let mut rest = path;
    loop {
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        let name = &rest[..end];
        if !valid_name(name) {
            return Err(InvalidPath::BadName(name.to_owned()));
        }
        rest = &rest[end..];
        let mut index = "";
        if let Some(open) = rest.strip_prefix('[') {
            let close = open.find(']').ok_or_else(|| InvalidPath::BadIndex(open.to_owned()))?;
            index = &open[..close];
            if index.is_empty() || index.contains('[') || index.chars().any(char::is_control) {
                return Err(InvalidPath::BadIndex(index.to_owned()));
            }
            rest = &open[close + 1..];
        }
        steps.push(Step { name: name.to_owned(), index: index.to_owned() });
        match rest.strip_prefix('.') {
            Some(next) => rest = next,
            None if rest.is_empty() => return Ok(steps),
            None => return Err(InvalidPath::BadName(rest.to_owned())),
        }
    }
}

fn valid_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A path's value as its client evaluated it.
#[derive(Debug, Clone, PartialEq)]
pub enum PathValue {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    /// Any other type (`spawn`, `buff`...), as `${...}` shows it.
    Object(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathResult {
    pub path: String,
    pub value: PathValue,
    pub evaluated_ms: u64,
}

impl PathResult {
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.evaluated_ms)
    }
}

/// Words written under a sequence counter that's odd during a write.
#[repr(C)]
struct Record<const W: usize> {
    sequence: AtomicU32,
    words: [AtomicU32; W],
}

impl<const W: usize> Record<W> {
    fn write(&self, words: &[u32; W]) {
        let sequence = self.sequence.load(Ordering::Relaxed) | 1;
        self.sequence.store(sequence, Ordering::Relaxed);
        fence(Ordering::Release);
        for (to, from) in self.words.iter().zip(words) {
            to.store(*from, Ordering::Relaxed);
        }
        self.sequence.store(sequence.wrapping_add(1), Ordering::Release);
    }

    /// None if a writer is holding it up.
    fn read(&self) -> Option<[u32; W]> {
        for _ in 0..READ_TRIES {
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let words = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return Some(words);
            }
        }
        None
    }
}

/// A slot's requests and results. Each table's counter moves after every
/// change, so a client reading another's can skip it when it hasn't.
#[repr(C)]
pub(crate) struct PathTables {
    requests_changed: AtomicU32,
    results_changed: AtomicU32,
    requests: [Record<REQUEST_WORDS>; MAX_REQUESTS],
    results: [Record<RESULT_WORDS>; MAX_RESULTS],
}

impl PathTables {
    /// Empties both tables, for a client taking the slot.
    pub(crate) fn clear(&self) {
        for request in &self.requests {
            request.write(&[0; REQUEST_WORDS]);
        }
        for result in &self.results {
            result.write(&[0; RESULT_WORDS]);
        }
        self.requests_changed.fetch_add(1, Ordering::Release);
        self.results_changed.fetch_add(1, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn stop_first_result_mid_write(&self) {
        let sequence = &self.results[0].sequence;
        sequence.store(sequence.load(Ordering::Relaxed) | 1, Ordering::Relaxed);
    }
}

fn put_text(words: &mut [u32], text: &str) {
    for (word, chunk) in words.iter_mut().zip(text.as_bytes().chunks(4)) {
        let mut bytes = [0; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        *word = u32::from_le_bytes(bytes);
    }
}

fn get_text(words: &[u32], len: usize) -> String {
    let mut bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    bytes.truncate(len.min(words.len() * 4));
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn split64(n: u64) -> [u32; 2] {
    [n as u32, (n >> 32) as u32]
}

fn join64(low: u32, high: u32) -> u64 {
    u64::from(high) << 32 | u64::from(low)
}

fn encode_request(path: &str, targets: Targets) -> [u32; REQUEST_WORDS] {
    let mut words = [0; REQUEST_WORDS];
    [words[0], words[1]] = split64(targets.0);
    words[2] = path.len() as u32;
    put_text(&mut words[3..], path);
    words
}

fn decode_request(words: &[u32; REQUEST_WORDS]) -> Option<(String, Targets)> {
    let targets = Targets(join64(words[0], words[1]));
    (!targets.is_empty()).then(|| (get_text(&words[3..], words[2] as usize), targets))
}

const EMPTY: u32 = 0;
const NULL: u32 = 1;
const INT: u32 = 2;
const FLOAT: u32 = 3;
const BOOL: u32 = 4;
const STRING: u32 = 5;
const OBJECT: u32 = 6;

fn encode_result(result: &PathResult) -> [u32; RESULT_WORDS] {
    let mut words = [0; RESULT_WORDS];
    let (kind, number, text) = match &result.value {
        PathValue::Null => (NULL, 0, ""),
        PathValue::Int(n) => (INT, *n as u64, ""),
        PathValue::Float(x) => (FLOAT, x.to_bits(), ""),
        PathValue::Bool(b) => (BOOL, u64::from(*b), ""),
        PathValue::String(text) => (STRING, 0, cut(text, MAX_VALUE_LEN)),
        PathValue::Object(text) => (OBJECT, 0, cut(text, MAX_VALUE_LEN)),
    };
    words[0] = kind;
    words[1] = result.path.len() as u32;
    words[2] = text.len() as u32;
    [words[3], words[4]] = split64(result.evaluated_ms);
    [words[5], words[6]] = split64(number);
    put_text(&mut words[7..7 + PATH_WORDS], &result.path);
    put_text(&mut words[7 + PATH_WORDS..], text);
    words
}

fn decode_result(words: &[u32; RESULT_WORDS]) -> Option<PathResult> {
    let number = join64(words[5], words[6]);
    let text = || get_text(&words[7 + PATH_WORDS..], words[2] as usize);
    let value = match words[0] {
        NULL => PathValue::Null,
        INT => PathValue::Int(number as i64),
        FLOAT => PathValue::Float(f64::from_bits(number)),
        BOOL => PathValue::Bool(number != 0),
        STRING => PathValue::String(text()),
        OBJECT => PathValue::Object(text()),
        _ => return None,
    };
    let path = get_text(&words[7..7 + PATH_WORDS], words[1] as usize);
    Some(PathResult { path, value, evaluated_ms: join64(words[3], words[4]) })
}

impl Region {
    /// The paths the client in `slot` asks for, with the slots it asks.
    pub fn requests(&self, slot: SlotId) -> Vec<(String, Targets)> {
        self.paths(slot).requests.iter().filter_map(|request| decode_request(&request.read()?)).collect()
    }

    /// The values the client in `slot` has evaluated.
    pub fn results(&self, slot: SlotId) -> Vec<PathResult> {
        self.paths(slot).results.iter().filter_map(|result| decode_result(&result.read()?)).collect()
    }

    fn write_requests(&self, me: SlotId, requests: &[(String, Targets)]) {
        let tables = self.paths(me);
        for (i, record) in tables.requests.iter().enumerate() {
            match requests.get(i) {
                Some((path, targets)) => record.write(&encode_request(path, *targets)),
                None => record.write(&[0; REQUEST_WORDS]),
            }
        }
        tables.requests_changed.fetch_add(1, Ordering::Release);
    }

    fn write_result(&self, me: SlotId, index: usize, result: Option<&PathResult>) {
        let tables = self.paths(me);
        tables.results[index].write(&result.map_or([EMPTY; RESULT_WORDS], encode_result));
        tables.results_changed.fetch_add(1, Ordering::Release);
    }
}

/// How often a path that took `cost` to evaluate may be evaluated, in ms.
pub fn interval_ms(cost: Duration) -> u64 {
    (COST_FACTOR * cost.as_micros() as u64 / 1000).max(MIN_INTERVAL_MS)
}

struct Tracked {
    index: usize,
    evaluated_ms: Option<u64>,
    interval_ms: u64,
}

/// The paths other clients want from this one, and when each is next due.
#[derive(Default)]
pub struct Evaluator {
    /// Each slot's requests, as of its counter.
    seen: HashMap<usize, (u32, Vec<(String, Targets)>)>,
    tracked: BTreeMap<String, Tracked>,
}

impl Evaluator {
    /// Catches up with what's asked of `me` by live clients: a path nobody
    /// asks for any more is emptied from the results, and a new one takes a
    /// free result until there are [`MAX_RESULTS`].
    pub fn refresh(&mut self, region: &Region, me: SlotId, liveness: &mut impl Liveness) {
        let mut wanted = BTreeSet::new();
        let live: Vec<SlotId> = region.owners().filter(|(_, owner)| liveness.is_alive(*owner)).map(|(slot, _)| slot).collect();
        self.seen.retain(|slot, _| live.iter().any(|live| live.0 == *slot));
        for slot in live {
            let changed = region.paths(slot).requests_changed.load(Ordering::Acquire);
            let seen = self.seen.entry(slot.0).or_insert((changed.wrapping_sub(1), Vec::new()));
            if seen.0 != changed {
                *seen = (changed, region.requests(slot));
            }
            wanted.extend(seen.1.iter().filter(|(_, targets)| targets.contains(me)).map(|(path, _)| path.clone()));
        }
        let gone: Vec<String> = self.tracked.keys().filter(|path| !wanted.contains(*path)).cloned().collect();
        for path in gone {
            let tracked = self.tracked.remove(&path).expect("listed above");
            region.write_result(me, tracked.index, None);
        }
        for path in wanted {
            if self.tracked.len() == MAX_RESULTS {
                break;
            }
            if !self.tracked.contains_key(&path) {
                let index = (0..MAX_RESULTS).find(|i| self.tracked.values().all(|tracked| tracked.index != *i)).expect("fewer than MAX_RESULTS");
                self.tracked.insert(path, Tracked { index, evaluated_ms: None, interval_ms: MIN_INTERVAL_MS });
            }
        }
    }

    /// The paths due to be evaluated, those never evaluated first and then
    /// the longest overdue.
    pub fn due(&self, now_ms: u64) -> Vec<String> {
        let mut due: Vec<(u64, &String)> = self
            .tracked
            .iter()
            .filter_map(|(path, tracked)| match tracked.evaluated_ms {
                None => Some((u64::MAX, path)),
                Some(at) => now_ms.checked_sub(at + tracked.interval_ms).map(|late| (late, path)),
            })
            .collect();
        due.sort_by_key(|(late, _)| std::cmp::Reverse(*late));
        due.into_iter().map(|(_, path)| path.clone()).collect()
    }

    /// Writes a path's value, and when it may next be evaluated.
    pub fn record(&mut self, region: &Region, me: SlotId, path: &str, value: PathValue, now_ms: u64, cost: Duration) {
        let Some(tracked) = self.tracked.get_mut(path) else { return };
        tracked.evaluated_ms = Some(now_ms);
        tracked.interval_ms = interval_ms(cost);
        region.write_result(me, tracked.index, Some(&PathResult { path: path.to_owned(), value, evaluated_ms: now_ms }));
    }

    pub fn len(&self) -> usize {
        self.tracked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracked.is_empty()
    }
}

type Key = (String, String);

/// The paths this client reads from others, and their last values.
#[derive(Default)]
pub struct Reader {
    /// Member name and path, and when last read.
    wants: BTreeMap<Key, u64>,
    cache: HashMap<Key, PathResult>,
    written: Vec<(String, Targets)>,
    /// Each slot's results, as of its counter.
    seen: HashMap<usize, (u32, Vec<PathResult>)>,
}

impl Reader {
    /// A read of `path` on `member`: its last value, if one has arrived. The
    /// path is asked for until it goes unread for [`UNREAD_AFTER_MS`].
    pub fn read(&mut self, member: &str, path: &str, now_ms: u64) -> Option<&PathResult> {
        let key = (member.to_owned(), path.to_owned());
        let result = self.cache.get(&key);
        self.wants.insert(key, now_ms);
        result
    }

    /// The last value without reading it, so asking for nothing.
    pub fn peek(&self, member: &str, path: &str) -> Option<&PathResult> {
        self.cache.get(&(member.to_owned(), path.to_owned()))
    }

    /// Paths asked for now.
    pub fn wanted(&self) -> usize {
        self.written.len()
    }

    /// Stops asking for paths unread too long, writes what's asked for to
    /// `me`'s slot (the most recently read [`MAX_REQUESTS`] paths) if that has
    /// changed, and copies in the values that have arrived.
    pub fn refresh(&mut self, region: &Region, me: SlotId, slot_of: impl Fn(&str) -> Option<SlotId>, now_ms: u64) {
        self.wants.retain(|_, read_ms| now_ms.saturating_sub(*read_ms) < UNREAD_AFTER_MS);
        let mut latest: BTreeMap<&str, (u64, Targets)> = BTreeMap::new();
        let mut slots = Vec::new();
        for ((member, path), read_ms) in &self.wants {
            let Some(slot) = slot_of(member) else { continue };
            let entry = latest.entry(path).or_default();
            entry.0 = entry.0.max(*read_ms);
            entry.1.add(slot);
            slots.push((member, path, slot));
        }
        let mut requests: Vec<(u64, &str, Targets)> = latest.into_iter().map(|(path, (read_ms, targets))| (read_ms, path, targets)).collect();
        requests.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        requests.truncate(MAX_REQUESTS);
        let requests: Vec<(String, Targets)> = requests.into_iter().map(|(_, path, targets)| (path.to_owned(), targets)).collect();
        if requests != self.written {
            region.write_requests(me, &requests);
            self.written = requests;
        }
        for (member, path, slot) in slots {
            let changed = region.paths(slot).results_changed.load(Ordering::Acquire);
            let seen = self.seen.entry(slot.0).or_insert((changed.wrapping_sub(1), Vec::new()));
            if seen.0 != changed {
                *seen = (changed, region.results(slot));
            }
            if let Some(result) = seen.1.iter().find(|result| result.path == *path) {
                self.cache.insert((member.clone(), path.clone()), result.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::tests::{Dead, owner, region, stop_mid_write_paths, zeroed_memory};

    fn steps(path: &str) -> Vec<(String, String)> {
        parse_path(path).unwrap().into_iter().map(|step| (step.name, step.index)).collect()
    }

    #[test]
    fn paths_split_into_names_and_indexes() {
        assert_eq!(steps("PctHPs"), [("PctHPs".into(), "".into())]);
        assert_eq!(steps("Buff[Spirit of Wolf].Duration.TotalSeconds"), [
            ("Buff".into(), "Spirit of Wolf".into()),
            ("Duration".into(), "".into()),
            ("TotalSeconds".into(), "".into()),
        ]);
        assert_eq!(steps("Inventory[mainhand].Item[2]"), [("Inventory".into(), "mainhand".into()), ("Item".into(), "2".into())]);
    }

    /// Negative: each of these would reach the other client as something
    /// other than one `Me` path.
    #[test]
    fn invalid_paths_are_refused() {
        assert_eq!(parse_path(""), Err(InvalidPath::Empty));
        assert_eq!(parse_path(&"A".repeat(MAX_PATH_LEN + 1)), Err(InvalidPath::TooLong));
        assert!(parse_path(&"A".repeat(MAX_PATH_LEN)).is_ok());
        assert_eq!(parse_path("Buff[${Me.Level}]"), Err(InvalidPath::Substitution));
        assert_eq!(parse_path("${Me.Name}"), Err(InvalidPath::Substitution));
        assert_eq!(parse_path("Target..Name"), Err(InvalidPath::BadName("".into())));
        assert_eq!(parse_path("Target."), Err(InvalidPath::BadName("".into())));
        assert_eq!(parse_path(".Target"), Err(InvalidPath::BadName("".into())));
        assert_eq!(parse_path("2Target"), Err(InvalidPath::BadName("2Target".into())));
        assert_eq!(parse_path("Target Name"), Err(InvalidPath::BadName("Target Name".into())));
        assert_eq!(parse_path("Buff[]"), Err(InvalidPath::BadIndex("".into())));
        assert_eq!(parse_path("Buff[1"), Err(InvalidPath::BadIndex("1".into())));
        assert_eq!(parse_path("Buff[[1]]"), Err(InvalidPath::BadIndex("[1".into())));
        assert_eq!(parse_path("Buff[1\n]"), Err(InvalidPath::BadIndex("1\n".into())));
        assert_eq!(parse_path("Buff[1]Name"), Err(InvalidPath::BadName("Name".into())));
    }

    fn result(path: &str, value: PathValue) -> PathResult {
        PathResult { path: path.into(), value, evaluated_ms: 1234 }
    }

    #[test]
    fn every_kind_of_value_comes_back_as_written() {
        for value in [
            PathValue::Null,
            PathValue::Int(-5_000_000_000),
            PathValue::Float(-1.25),
            PathValue::Bool(true),
            PathValue::Bool(false),
            PathValue::String("Spirit of Wolf".into()),
            PathValue::Object("a gnoll".into()),
        ] {
            let result = result("Inventory[mainhand].Name", value);
            assert_eq!(decode_result(&encode_result(&result)), Some(result));
        }
    }

    #[test]
    fn a_long_value_is_cut_at_a_character_boundary() {
        let text = format!("{}é", "a".repeat(MAX_VALUE_LEN - 1));
        let decoded = decode_result(&encode_result(&result("Name", PathValue::String(text)))).unwrap();
        assert_eq!(decoded.value, PathValue::String("a".repeat(MAX_VALUE_LEN - 1)));
    }

    #[test]
    fn the_interval_is_ten_times_the_cost_and_at_least_100_ms() {
        assert_eq!(interval_ms(Duration::from_micros(50)), MIN_INTERVAL_MS);
        assert_eq!(interval_ms(Duration::from_millis(10)), 100);
        assert_eq!(interval_ms(Duration::from_millis(25)), 250);
    }

    /// Two clients in a block: `a` reads from `b`.
    fn pair(memory: &mut [u32]) -> (&Region, SlotId, SlotId) {
        let region = region(memory);
        let a = region.claim(owner(1), &mut Dead::default()).unwrap();
        let b = region.claim(owner(2), &mut Dead::default()).unwrap();
        (region, a, b)
    }

    fn slot_of(slots: &[(&str, SlotId)]) -> impl Fn(&str) -> Option<SlotId> {
        let slots: Vec<(String, SlotId)> = slots.iter().map(|(name, slot)| ((*name).to_owned(), *slot)).collect();
        move |name| slots.iter().find(|(n, _)| n == name).map(|(_, slot)| *slot)
    }

    #[test]
    fn a_read_is_null_until_the_value_arrives_then_the_value() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let (mut reader, mut evaluator) = (Reader::default(), Evaluator::default());
        assert_eq!(reader.read("Bob", "PctHPs", 0), None);
        reader.refresh(region, a, slot_of(&[("Bob", b)]), 0);
        evaluator.refresh(region, b, &mut Dead::default());
        assert_eq!(evaluator.due(0), ["PctHPs"]);
        evaluator.record(region, b, "PctHPs", PathValue::Int(97), 10, Duration::from_micros(30));
        assert_eq!(reader.read("Bob", "PctHPs", 20), None, "not copied in until the next refresh");
        reader.refresh(region, a, slot_of(&[("Bob", b)]), 20);
        assert_eq!(reader.read("Bob", "PctHPs", 30).map(|result| &result.value), Some(&PathValue::Int(97)));
        assert_eq!(reader.peek("Bob", "PctHPs").unwrap().age_ms(40), 30);
    }

    #[test]
    fn a_path_is_evaluated_only_for_the_slots_that_ask() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let c = region.claim(owner(3), &mut Dead::default()).unwrap();
        let mut reader = Reader::default();
        reader.read("Bob", "PctHPs", 0);
        reader.refresh(region, a, slot_of(&[("Bob", b), ("Cat", c)]), 0);
        let (mut on_b, mut on_c) = (Evaluator::default(), Evaluator::default());
        on_b.refresh(region, b, &mut Dead::default());
        on_c.refresh(region, c, &mut Dead::default());
        assert_eq!((on_b.len(), on_c.len()), (1, 0));
    }

    #[test]
    fn a_path_is_due_again_after_its_interval_and_the_longest_overdue_first() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let mut reader = Reader::default();
        reader.read("Bob", "Fast", 0);
        reader.read("Bob", "Slow", 0);
        reader.read("Bob", "New", 0);
        reader.refresh(region, a, slot_of(&[("Bob", b)]), 0);
        let mut evaluator = Evaluator::default();
        evaluator.refresh(region, b, &mut Dead::default());
        evaluator.record(region, b, "Fast", PathValue::Int(1), 0, Duration::from_micros(10));
        evaluator.record(region, b, "Slow", PathValue::Int(1), 0, Duration::from_millis(50));
        assert_eq!(evaluator.due(99), ["New"]);
        assert_eq!(evaluator.due(100), ["New", "Fast"]);
        assert_eq!(evaluator.due(600), ["New", "Fast", "Slow"], "Fast is 500 ms late, Slow 100 ms");
    }

    #[test]
    fn an_unread_path_stops_being_asked_for_and_keeps_its_last_value() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let (mut reader, mut evaluator) = (Reader::default(), Evaluator::default());
        reader.read("Bob", "PctHPs", 0);
        reader.refresh(region, a, slot_of(&[("Bob", b)]), 0);
        evaluator.refresh(region, b, &mut Dead::default());
        evaluator.record(region, b, "PctHPs", PathValue::Int(50), 0, Duration::ZERO);
        reader.refresh(region, a, slot_of(&[("Bob", b)]), UNREAD_AFTER_MS - 1);
        assert_eq!(reader.wanted(), 1, "read just under 30 s ago");
        reader.refresh(region, a, slot_of(&[("Bob", b)]), UNREAD_AFTER_MS);
        assert_eq!(reader.wanted(), 0);
        evaluator.refresh(region, b, &mut Dead::default());
        assert!(evaluator.is_empty());
        assert!(region.results(b).is_empty(), "its result is emptied");

        let kept = reader.read("Bob", "PctHPs", UNREAD_AFTER_MS + 1).map(|result| result.value.clone());
        assert_eq!(kept, Some(PathValue::Int(50)), "the last value, which starts it again");
        reader.refresh(region, a, slot_of(&[("Bob", b)]), UNREAD_AFTER_MS + 1);
        evaluator.refresh(region, b, &mut Dead::default());
        assert_eq!(evaluator.len(), 1);
    }

    #[test]
    fn a_dead_readers_requests_are_ignored() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let mut reader = Reader::default();
        reader.read("Bob", "PctHPs", 0);
        reader.refresh(region, a, slot_of(&[("Bob", b)]), 0);
        let mut evaluator = Evaluator::default();
        evaluator.refresh(region, b, &mut Dead([owner(1)].into()));
        assert!(evaluator.is_empty());
    }

    #[test]
    fn a_client_taking_a_slot_starts_with_empty_tables() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let mut reader = Reader::default();
        reader.read("Bob", "PctHPs", 0);
        reader.refresh(region, a, slot_of(&[("Bob", b)]), 0);
        let mut evaluator = Evaluator::default();
        evaluator.refresh(region, b, &mut Dead::default());
        evaluator.record(region, b, "PctHPs", PathValue::Int(50), 0, Duration::ZERO);
        assert_eq!(region.claim(owner(9), &mut Dead([owner(1), owner(2)].into())).unwrap(), a);
        assert_eq!(region.claim(owner(10), &mut Dead([owner(1), owner(2)].into())).unwrap(), b);
        assert!(region.requests(a).is_empty());
        assert!(region.results(b).is_empty());
    }

    #[test]
    fn only_the_most_recently_read_paths_are_asked_for() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let mut reader = Reader::default();
        for i in 0..=MAX_REQUESTS as u64 {
            reader.read("Bob", &format!("Buff[{i}]"), i);
        }
        reader.refresh(region, a, slot_of(&[("Bob", b)]), MAX_REQUESTS as u64);
        let asked = region.requests(a);
        assert_eq!(asked.len(), MAX_REQUESTS);
        assert!(!asked.iter().any(|(path, _)| path == "Buff[0]"), "the oldest read is left out");
    }

    #[test]
    fn an_owner_evaluates_at_most_max_results_paths() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let b = region.claim(owner(2), &mut Dead::default()).unwrap();
        for pid in 10..13 {
            let slot = region.claim(owner(pid), &mut Dead::default()).unwrap();
            let mut reader = Reader::default();
            for i in 0..MAX_REQUESTS {
                reader.read("Bob", &format!("Buff[{pid}x{i}]"), 0);
            }
            reader.refresh(region, slot, slot_of(&[("Bob", b)]), 0);
        }
        let mut evaluator = Evaluator::default();
        evaluator.refresh(region, b, &mut Dead::default());
        assert_eq!(evaluator.len(), MAX_RESULTS);
    }

    /// Negative: a writer stopped partway leaves a result nobody can read
    /// whole, which is left out rather than read torn.
    #[test]
    fn a_result_held_up_mid_write_is_left_out() {
        let mut memory = zeroed_memory();
        let (region, a, b) = pair(&mut memory);
        let mut reader = Reader::default();
        reader.read("Bob", "PctHPs", 0);
        reader.refresh(region, a, slot_of(&[("Bob", b)]), 0);
        let mut evaluator = Evaluator::default();
        evaluator.refresh(region, b, &mut Dead::default());
        evaluator.record(region, b, "PctHPs", PathValue::Int(50), 0, Duration::ZERO);
        assert_eq!(region.results(b).len(), 1);
        stop_mid_write_paths(region, b);
        assert!(region.results(b).is_empty());
    }
}
