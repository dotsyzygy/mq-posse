//! Reads this client's status from the game.

use mq_emu::{GameState, Mq, Value};
use posse_core::{BuffEntry, MAX_BUFFS, MAX_SONGS, State, Status, flags};

use crate::Posse;

const TICK_MS: i64 = 6000;

/// `Me` members with no typed getter, read through MacroQuest.
const FLAGS: [(&str, u32); 7] = [
    ("Me.Combat", flags::COMBAT),
    ("Me.Sitting", flags::SITTING),
    ("Me.Moving", flags::MOVING),
    ("Me.Invis", flags::INVIS),
    ("Me.Stunned", flags::STUNNED),
    ("Me.Feigning", flags::FEIGNING),
    ("Me.Hovering", flags::HOVERING),
];

/// Brings `status` up to date. Out of game only the state changes, so other
/// clients go on seeing the character's last known name and zone.
pub fn collect(mq: &Mq<Posse>, status: &mut Status, zoning: bool) {
    let state = if mq.game_state() == GameState::InGame && !zoning && mq.me().is_some() { State::InGame } else { State::Zoning };
    status.set_state(state);
    if state != State::InGame {
        return;
    }
    let Some(me) = mq.me() else { return };

    status.set_name(&me.name());
    status.id = me.id().0;
    status.level = u32::from(me.level());
    status.class = u32::from(me.class());
    status.race = me.race();
    // Not the spawn's fields: in game its mana read 96 where ${Me.CurrentMana}
    // read 10277. These are what ${Me} shows.
    status.hp_current = int(mq, "Me.CurrentHPs");
    status.hp_max = int(mq, "Me.MaxHPs");
    status.hp_percent = if status.hp_max == 0 { 0 } else { (i64::from(status.hp_current) * 100 / i64::from(status.hp_max)) as i32 };
    status.mana_current = int(mq, "Me.CurrentMana");
    status.mana_max = int(mq, "Me.MaxMana");
    status.endurance_current = mq.endurance_current().unwrap_or(0);
    status.endurance_max = mq.endurance_max().unwrap_or(0);
    let position = me.position();
    (status.x, status.y, status.z) = (position.x, position.y, position.z);
    status.heading_degrees = me.heading().degrees();
    status.casting = me.casting().map_or(0, |spell| spell.0);
    status.target_id = mq.target().map_or(0, |target| target.id().0);
    if let Some(zone) = mq.zone() {
        status.set_zone(&zone.short_name());
        status.zone_instance = u32::from(zone.instance());
    }

    status.flags = FLAGS.iter().filter(|(path, _)| is_true(mq, path)).fold(0, |all, (_, flag)| all | flag);

    if let Some(pet) = mq.pet() {
        status.pet_id = pet.id().0;
        status.pet_hp_percent = pet.hp_percent();
        // The internal name, as ${Me.Pet.Name} shows it: "Zabaner000", not "Zabaner".
        status.set_pet_name(&pet.raw_name());
    } else {
        status.pet_id = 0;
        status.pet_hp_percent = 0;
        status.set_pet_name("");
    }

    status.buffs = [BuffEntry { spell_id: 0, ms_left: 0 }; MAX_BUFFS];
    for buff in mq.buffs() {
        if let Some(entry) = status.buffs.get_mut(buff.slot()) {
            *entry = BuffEntry { spell_id: buff.spell_id().0, ms_left: ms_left(buff.ticks()) };
        }
    }
    status.songs = [BuffEntry { spell_id: 0, ms_left: 0 }; MAX_SONGS];
    for song in mq.songs() {
        if let Some(entry) = status.songs.get_mut(song.slot()) {
            *entry = BuffEntry { spell_id: song.spell_id().0, ms_left: ms_left(song.ticks()) };
        }
    }
}

fn is_true(mq: &Mq<Posse>, path: &str) -> bool {
    matches!(mq.data(path), Ok(Some(data)) if data.value() == Value::Bool(true))
}

fn int(mq: &Mq<Posse>, path: &str) -> i32 {
    match mq.data(path) {
        Ok(Some(data)) => match data.value() {
            Value::Int(value) => value as i32,
            _ => 0,
        },
        _ => 0,
    }
}

fn ms_left(ticks: Option<u32>) -> i32 {
    ticks.map_or(-1, |ticks| (i64::from(ticks) * TICK_MS).min(i64::from(i32::MAX)) as i32)
}
