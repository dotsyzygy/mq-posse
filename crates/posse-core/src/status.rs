//! The status a client publishes in its slot.

pub const MAX_BUFFS: usize = 42;
pub const MAX_SONGS: usize = 55;
/// A client that hasn't published for this long is stale.
pub const STALE_AFTER_MS: u64 = 3000;

const NAME_LEN: usize = 64;
const ZONE_LEN: usize = 32;

/// Bits of [`Status::flags`].
pub mod flags {
    pub const COMBAT: u32 = 1 << 0;
    pub const SITTING: u32 = 1 << 1;
    pub const MOVING: u32 = 1 << 2;
    pub const INVIS: u32 = 1 << 3;
    pub const STUNNED: u32 = 1 << 4;
    pub const FEIGNING: u32 = 1 << 5;
    pub const HOVERING: u32 = 1 << 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    InGame,
    Zoning,
}

impl State {
    /// As `.GameState` shows it.
    pub fn as_str(self) -> &'static str {
        match self {
            State::InGame => "InGame",
            State::Zoning => "Zoning",
        }
    }
}

/// A buff or song. An empty slot has a `spell_id` of 0 or less.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuffEntry {
    pub spell_id: i32,
    /// When published. Negative for a buff that doesn't run out.
    pub ms_left: i32,
}

/// One client's published status. Every field is 4 bytes or a multiple of it,
/// so there's no padding and any bits read from another process are valid.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Status {
    published_ms_low: u32,
    published_ms_high: u32,
    state: u32,
    pub id: u32,
    pub level: u32,
    pub class: u32,
    pub race: i32,
    pub hp_percent: i32,
    pub hp_current: i32,
    pub hp_max: i32,
    pub mana_current: i32,
    pub mana_max: i32,
    pub endurance_current: i32,
    pub endurance_max: i32,
    pub flags: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub heading_degrees: f32,
    pub casting: i32,
    pub target_id: u32,
    pub pet_id: u32,
    pub pet_hp_percent: i32,
    name: [u8; NAME_LEN],
    pet_name: [u8; NAME_LEN],
    zone: [u8; ZONE_LEN],
    pub buffs: [BuffEntry; MAX_BUFFS],
    pub songs: [BuffEntry; MAX_SONGS],
}

const _: () = assert!(align_of::<Status>() == 4 && size_of::<Status>().is_multiple_of(4));

/// The size of [`Status`] in the words it's copied by.
pub(crate) const STATUS_WORDS: usize = size_of::<Status>() / 4;

impl Default for Status {
    fn default() -> Status {
        Status {
            published_ms_low: 0,
            published_ms_high: 0,
            state: 0,
            id: 0,
            level: 0,
            class: 0,
            race: 0,
            hp_percent: 0,
            hp_current: 0,
            hp_max: 0,
            mana_current: 0,
            mana_max: 0,
            endurance_current: 0,
            endurance_max: 0,
            flags: 0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            heading_degrees: 0.0,
            casting: 0,
            target_id: 0,
            pet_id: 0,
            pet_hp_percent: 0,
            name: [0; NAME_LEN],
            pet_name: [0; NAME_LEN],
            zone: [0; ZONE_LEN],
            buffs: [BuffEntry { spell_id: 0, ms_left: 0 }; MAX_BUFFS],
            songs: [BuffEntry { spell_id: 0, ms_left: 0 }; MAX_SONGS],
        }
    }
}

impl Status {
    pub fn state(&self) -> State {
        match self.state {
            0 => State::InGame,
            _ => State::Zoning,
        }
    }

    pub fn set_state(&mut self, state: State) {
        self.state = match state {
            State::InGame => 0,
            State::Zoning => 1,
        };
    }

    pub fn published_ms(&self) -> u64 {
        u64::from(self.published_ms_high) << 32 | u64::from(self.published_ms_low)
    }

    pub fn set_published_ms(&mut self, ms: u64) {
        self.published_ms_low = ms as u32;
        self.published_ms_high = (ms >> 32) as u32;
    }

    /// Milliseconds between publishing and `now_ms`, on the clock all clients
    /// share.
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.published_ms())
    }

    pub fn is_stale(&self, now_ms: u64) -> bool {
        self.age_ms(now_ms) > STALE_AFTER_MS
    }

    pub fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    pub fn name(&self) -> String {
        from_text(&self.name)
    }

    pub fn set_name(&mut self, name: &str) {
        to_text(&mut self.name, name);
    }

    pub fn pet_name(&self) -> String {
        from_text(&self.pet_name)
    }

    pub fn set_pet_name(&mut self, name: &str) {
        to_text(&mut self.pet_name, name);
    }

    pub fn zone(&self) -> String {
        from_text(&self.zone)
    }

    pub fn set_zone(&mut self, zone: &str) {
        to_text(&mut self.zone, zone);
    }

    pub fn mana_percent(&self) -> i32 {
        percent(self.mana_current, self.mana_max)
    }

    pub fn endurance_percent(&self) -> i32 {
        percent(self.endurance_current, self.endurance_max)
    }
}

impl BuffEntry {
    pub fn is_empty(&self) -> bool {
        self.spell_id <= 0
    }

    /// Milliseconds left `age_ms` after publishing, or None for a buff that
    /// doesn't run out.
    pub fn ms_left_after(&self, age_ms: u64) -> Option<i64> {
        (self.ms_left >= 0).then(|| (i64::from(self.ms_left) - i64::try_from(age_ms).unwrap_or(i64::MAX)).max(0))
    }
}

fn percent(current: i32, max: i32) -> i32 {
    if max <= 0 { 0 } else { (i64::from(current) * 100 / i64::from(max)).clamp(0, 100) as i32 }
}

fn to_text(field: &mut [u8], text: &str) {
    field.fill(0);
    // Keep the last byte a terminator, and don't cut a character in half.
    let mut len = text.len().min(field.len() - 1);
    while !text.is_char_boundary(len) {
        len -= 1;
    }
    field[..len].copy_from_slice(&text.as_bytes()[..len]);
}

fn from_text(field: &[u8]) -> String {
    let len = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_no_padding() {
        let fields = 23 * 4 + NAME_LEN * 2 + ZONE_LEN + (MAX_BUFFS + MAX_SONGS) * size_of::<BuffEntry>();
        assert_eq!(size_of::<Status>(), fields);
    }

    #[test]
    fn text_round_trips_and_truncates_on_a_character() {
        let mut status = Status::default();
        status.set_name("Bob");
        assert_eq!(status.name(), "Bob");
        status.set_zone(&"é".repeat(40));
        assert_eq!(status.zone(), "é".repeat(15));
        status.set_name("Al");
        assert_eq!(status.name(), "Al", "a shorter name leaves nothing of the longer one");
    }

    #[test]
    fn age_and_staleness() {
        let mut status = Status::default();
        status.set_published_ms(u64::from(u32::MAX) + 10);
        assert_eq!(status.published_ms(), u64::from(u32::MAX) + 10);
        let published = status.published_ms();
        assert_eq!(status.age_ms(published + 3000), 3000);
        assert!(!status.is_stale(published + STALE_AFTER_MS));
        assert!(status.is_stale(published + STALE_AFTER_MS + 1));
        assert_eq!(status.age_ms(published - 5), 0, "a read before publishing isn't a negative age");
    }

    #[test]
    fn states_round_trip() {
        let mut status = Status::default();
        for state in [State::Zoning, State::InGame] {
            status.set_state(state);
            assert_eq!(status.state(), state);
        }
    }

    #[test]
    fn percentages_handle_no_pool() {
        let mut status = Status::default();
        assert_eq!(status.mana_percent(), 0, "a class with no mana");
        (status.mana_current, status.mana_max) = (50, 200);
        assert_eq!(status.mana_percent(), 25);
        (status.endurance_current, status.endurance_max) = (300, 200);
        assert_eq!(status.endurance_percent(), 100);
    }

    #[test]
    fn buff_time_counts_down_from_publishing() {
        let buff = BuffEntry { spell_id: 1, ms_left: 6000 };
        assert_eq!(buff.ms_left_after(1000), Some(5000));
        assert_eq!(buff.ms_left_after(9000), Some(0));
        assert_eq!(BuffEntry { spell_id: 1, ms_left: -1 }.ms_left_after(1000), None);
        assert!(BuffEntry { spell_id: 0, ms_left: 0 }.is_empty());
    }
}
