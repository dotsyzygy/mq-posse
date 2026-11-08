//! Reads this client's status from the game.

use mq_emu::{GameState, Mq};
use posse_core::{BuffEntry, MAX_BUFFS, MAX_PET_BUFFS, MAX_SONGS, State, Status, flags};

use crate::Posse;

const TICK_MS: i64 = 6000;

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
    status.hp_current = mq.hp_current().unwrap_or(0);
    status.hp_max = mq.hp_max().unwrap_or(0);
    status.hp_percent = mq.hp_percent().unwrap_or(0);
    status.mana_current = mq.mana_current().unwrap_or(0);
    status.mana_max = mq.mana_max().unwrap_or(0);
    status.endurance_current = mq.endurance_current().unwrap_or(0);
    status.endurance_max = mq.endurance_max().unwrap_or(0);
    let position = me.position();
    (status.x, status.y, status.z) = (position.x, position.y, position.z);
    status.heading_degrees = me.heading().degrees();
    status.casting = me.casting().map_or(0, |spell| spell.0);
    status.target_id = mq.target().map_or(0, |target| target.id().0);
    status.target_hp_percent = mq.target().map_or(0, |target| target.hp_percent());
    if let Some(zone) = mq.zone() {
        status.set_zone(&zone.short_name());
        status.zone_instance = u32::from(zone.instance());
    }

    let states = [
        (mq.is_auto_attacking(), flags::COMBAT),
        (me.is_sitting(), flags::SITTING),
        (me.is_moving(), flags::MOVING),
        (me.is_invisible(), flags::INVIS),
        (mq.is_stunned(), flags::STUNNED),
        (me.is_feigning(), flags::FEIGNING),
        (me.is_hovering(), flags::HOVERING),
    ];
    status.flags = states.iter().filter(|(on, _)| *on).fold(0, |all, (_, flag)| all | flag);

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
    status.pet_buffs = [BuffEntry { spell_id: 0, ms_left: 0 }; MAX_PET_BUFFS];
    for buff in mq.pet_buffs() {
        if let Some(entry) = status.pet_buffs.get_mut(buff.slot()) {
            let ms_left = buff.remaining().map_or(-1, |left| left.as_millis().min(i32::MAX as u128) as i32);
            *entry = BuffEntry { spell_id: buff.spell_id().0, ms_left };
        }
    }
}

fn ms_left(ticks: Option<u32>) -> i32 {
    ticks.map_or(-1, |ticks| (i64::from(ticks) * TICK_MS).min(i64::from(i32::MAX)) as i32)
}
