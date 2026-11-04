//! The `Posse` TLO and its datatypes. `docs/tlo.md` documents every member, and
//! a test in `posse-core` fails if the two list different ones.

use mq_emu::{DataType, DataTypeDef, DataTypeError, MACROQUEST_MEMBERS, Mq, SpawnId, SpellId, TloError, Value};
use posse_core::{BuffEntry, State, Status, compass, flags};

use crate::Posse;
use crate::paths::step;
use posse_shm::now_ms;

const NO_PET: &str = "NO PET";

/// The plugin's datatypes, for members to return values of.
pub struct Types {
    member: DataType<String>,
    pet: DataType<String>,
    buff: DataType<String>,
    path: DataType<String>,
    channel: DataType<String>,
    last_command: DataType<()>,
    posse: DataType<()>,
}

/// `posse_member`'s own members, which its catch-all doesn't list. A test in
/// `posse-core` fails if this and the members added below differ.
const MEMBER_OWN: &[&str] = &[
    "GameState", "Zone", "TargetID", "TargetPctHPs", "Age", "Stale", "PathAge", "Spawn", "Class", "Race", "Casting", "Pet", "Buff", "Song", "Name", "Channels", "ID",
    "Level", "PctHPs", "CurrentHPs", "MaxHPs", "PctMana", "CurrentMana", "MaxMana", "PctEndurance", "CurrentEndurance", "MaxEndurance", "X", "Y",
    "Z", "Heading", "CountBuffs", "CountSongs", "Combat", "Sitting", "Moving", "Invis", "Stunned", "Feigning", "Hovering",
];
const PET_OWN: &[&str] = &["ID", "Name", "PctHPs", "Buff", "CountBuffs"];
const BUFF_OWN: &[&str] = &["ID", "Name", "Spell", "Duration"];

/// MacroQuest's member names but `own`, for a catch-all to list for Lua.
fn catch_all_names(own: &[&str]) -> Vec<&'static str> {
    MACROQUEST_MEMBERS.iter().copied().filter(|name| !own.contains(name)).collect()
}

/// A `posse_path` value's key: the member and the path.
fn path_key(member: &str, path: &str) -> String {
    format!("{member}\n{path}")
}

/// Which of a member's buff lists a `posse_buff` value is in.
#[derive(Clone, Copy)]
enum List {
    Buffs,
    Songs,
    PetBuffs,
}

/// A `posse_buff` value's key: the character, the list and the slot.
fn buff_key(name: &str, list: List, slot: usize) -> String {
    let list = match list {
        List::Buffs => 'b',
        List::Songs => 's',
        List::PetBuffs => 'p',
    };
    format!("{name}\n{list}\n{slot}")
}

/// The buff a `posse_buff` key names, with how old its status is.
fn lookup_buff(plugin: &Posse, key: &str) -> Option<(BuffEntry, u64, usize)> {
    let (name, list, slot) = split_buff_key(key)?;
    let status = plugin.status_of(name)?;
    let entries: &[BuffEntry] = match list {
        "b" => &status.buffs,
        "s" => &status.songs,
        _ => &status.pet_buffs,
    };
    let entry = *entries.get(slot)?;
    (!entry.is_empty()).then(|| (entry, status.age_ms(now_ms()), slot))
}

/// A `posse_buff` key's character, list (`b`, `s` or `p`) and slot.
fn split_buff_key(key: &str) -> Option<(&str, &str, usize)> {
    let mut parts = key.split('\n');
    Some((parts.next()?, parts.next()?, parts.next()?.parse().ok()?))
}

/// `Buff[n or name]`: a slot counted from 1, or the first with that spell.
fn find_buff(mq: &Mq<Posse>, entries: &[BuffEntry], index: &str) -> Option<usize> {
    if let Ok(n) = index.parse::<usize>() {
        return n.checked_sub(1).filter(|slot| entries.get(*slot).is_some_and(|entry| !entry.is_empty()));
    }
    entries.iter().position(|entry| !entry.is_empty() && spell_name(mq, entry.spell_id).is_some_and(|name| name.eq_ignore_ascii_case(index)))
}

fn spell_name(mq: &Mq<Posse>, id: i32) -> Option<String> {
    Some(mq.spell(SpellId(id))?.name())
}

/// A spell ID as MacroQuest's own `spell` value.
fn spell_value(id: i32) -> Option<Value> {
    Value::path(&format!("Spell[{id}]")).ok()
}

/// Adds a member that only reads the status.
fn field<F>(def: DataTypeDef<Posse, String>, member: &str, read: F) -> DataTypeDef<Posse, String>
where
    F: Fn(&Status) -> Option<Value> + 'static,
{
    def.member(member, move |plugin: &mut Posse, _, name: &String, _| read(&plugin.status_of(name)?))
}

/// Adds a boolean member from a status flag.
fn flag(def: DataTypeDef<Posse, String>, member: &str, flag: u32) -> DataTypeDef<Posse, String> {
    field(def, member, move |status| Some(status.has_flag(flag).into()))
}

/// A member has the members of `${Me}`, under the same names, and a few of its
/// own that `${Me}` doesn't have. `GameState` isn't `State` because `${Me.State}`
/// means something else: STAND, SIT and so on.
fn add_member_type(mq: &Mq<Posse>) -> Result<DataType<String>, DataTypeError> {
    let mut def = DataTypeDef::new()
        .member("GameState", |plugin: &mut Posse, _, name: &String, _| Some(plugin.status_of(name)?.state().as_str().into()))
        .member("Zone", |plugin, _, name, _| Some(plugin.status_of(name)?.zone().into()))
        .member("TargetID", |plugin, _, name, _| Some(plugin.status_of(name)?.target_id.into()))
        .member("TargetPctHPs", |plugin, _, name, _| Some(plugin.status_of(name)?.target_hp_percent.into()))
        .member("Age", |plugin, _, name, _| Some(i64::try_from(plugin.status_of(name)?.age_ms(now_ms())).ok()?.into()))
        .member("Stale", |plugin, _, name, _| Some(plugin.status_of(name)?.is_stale(now_ms()).into()))
        .member("PathAge", |plugin, _, name, index| plugin.path_age(name, index))
        .member("Spawn", |plugin, mq, name, _| {
            let status = plugin.status_of(name)?;
            let here = mq.zone()?;
            let same_zone = status.zone().eq_ignore_ascii_case(&here.short_name()) && status.zone_instance == u32::from(here.instance());
            (status.state() == State::InGame && same_zone).then(|| Value::spawn(SpawnId(status.id)))
        })
        .member("Class", |plugin, mq, name, _| Some(mq.class_name(plugin.status_of(name)?.class as u8)?.into()))
        .member("Race", |plugin, mq, name, _| Some(mq.race_name(plugin.status_of(name)?.race)?.into()))
        .member("Casting", |plugin, _, name, _| Some(plugin.status_of(name)?.casting).filter(|id| *id > 0).and_then(spell_value))
        .member("Pet", |plugin, _, name, _| {
            plugin.status_of(name)?;
            Some(plugin.types.as_ref()?.pet.value(name.clone()))
        })
        .member("Buff", |plugin, mq, name, index| {
            let status = plugin.status_of(name)?;
            let slot = find_buff(mq, &status.buffs, index)?;
            Some(plugin.types.as_ref()?.buff.value(buff_key(name, List::Buffs, slot)))
        })
        .member("Song", |plugin, mq, name, index| {
            let status = plugin.status_of(name)?;
            let slot = find_buff(mq, &status.songs, index)?;
            Some(plugin.types.as_ref()?.buff.value(buff_key(name, List::Songs, slot)))
        })
        .any_member(&catch_all_names(MEMBER_OWN), |plugin, _, name, member, index| plugin.path_value(name, &step(member, index)))
        .text(|_, _, name| name.clone());
    def = field(def, "Name", |status| Some(status.name().into()));
    def = field(def, "Channels", |status| Some(status.channels().join(" ").into()));
    def = field(def, "ID", |status| Some(status.id.into()));
    def = field(def, "Level", |status| Some(status.level.into()));
    def = field(def, "PctHPs", |status| Some(status.hp_percent.into()));
    def = field(def, "CurrentHPs", |status| Some(status.hp_current.into()));
    def = field(def, "MaxHPs", |status| Some(status.hp_max.into()));
    def = field(def, "PctMana", |status| Some(status.mana_percent().into()));
    def = field(def, "CurrentMana", |status| Some(status.mana_current.into()));
    def = field(def, "MaxMana", |status| Some(status.mana_max.into()));
    def = field(def, "PctEndurance", |status| Some(status.endurance_percent().into()));
    def = field(def, "CurrentEndurance", |status| Some(status.endurance_current.into()));
    def = field(def, "MaxEndurance", |status| Some(status.endurance_max.into()));
    def = field(def, "X", |status| Some(status.x.into()));
    def = field(def, "Y", |status| Some(status.y.into()));
    def = field(def, "Z", |status| Some(status.z.into()));
    def = field(def, "Heading", |status| Some(compass(status.heading_degrees).into()));
    def = field(def, "CountBuffs", |status| Some((status.buffs.iter().filter(|entry| !entry.is_empty()).count() as u32).into()));
    def = field(def, "CountSongs", |status| Some((status.songs.iter().filter(|entry| !entry.is_empty()).count() as u32).into()));
    def = flag(def, "Combat", flags::COMBAT);
    def = flag(def, "Sitting", flags::SITTING);
    def = flag(def, "Moving", flags::MOVING);
    def = flag(def, "Invis", flags::INVIS);
    def = flag(def, "Stunned", flags::STUNNED);
    def = flag(def, "Feigning", flags::FEIGNING);
    def = flag(def, "Hovering", flags::HOVERING);
    mq.add_type("posse_member", def)
}

fn add_pet_type(mq: &Mq<Posse>) -> Result<DataType<String>, DataTypeError> {
    let def = DataTypeDef::new()
        .member("ID", |plugin: &mut Posse, _, name: &String, _| Some(plugin.status_of(name)?.pet_id.into()))
        .member("Name", |plugin, _, name, _| {
            let status = plugin.status_of(name)?;
            Some(if status.pet_id == 0 { NO_PET.into() } else { status.pet_name().into() })
        })
        .member("PctHPs", |plugin, _, name, _| Some(plugin.status_of(name)?.pet_hp_percent.into()))
        .member("Buff", |plugin, mq, name, index| {
            let status = plugin.status_of(name)?;
            let slot = find_buff(mq, &status.pet_buffs, index)?;
            Some(plugin.types.as_ref()?.buff.value(buff_key(name, List::PetBuffs, slot)))
        })
        .member("CountBuffs", |plugin, _, name, _| Some((plugin.status_of(name)?.pet_buffs.iter().filter(|entry| !entry.is_empty()).count() as u32).into()))
        .any_member(&catch_all_names(PET_OWN), |plugin, _, name, member, index| plugin.path_value(name, &format!("Pet.{}", step(member, index))))
        .text(|plugin, _, name| plugin.status_of(name).filter(|status| status.pet_id != 0).map_or(NO_PET.to_owned(), |status| status.pet_name()));
    mq.add_type("posse_pet", def)
}

fn add_buff_type(mq: &Mq<Posse>) -> Result<DataType<String>, DataTypeError> {
    let def = DataTypeDef::new()
        .member("ID", |plugin: &mut Posse, _, key: &String, _| Some((lookup_buff(plugin, key)?.2 as u32 + 1).into()))
        .member("Name", |plugin, mq, key, _| Some(spell_name(mq, lookup_buff(plugin, key)?.0.spell_id)?.into()))
        .member("Spell", |plugin, _, key, _| spell_value(lookup_buff(plugin, key)?.0.spell_id))
        .member("Duration", |plugin, _, key, _| {
            let (entry, age, _) = lookup_buff(plugin, key)?;
            Some(entry.ms_left_after(age)?.into())
        })
        .any_member(&catch_all_names(BUFF_OWN), |plugin, _, key, member, index| {
            let (name, list, slot) = split_buff_key(key)?;
            let list = match list {
                "b" => "Buff",
                "s" => "Song",
                _ => "Pet.Buff",
            };
            plugin.path_value(name, &format!("{list}[{}].{}", slot + 1, step(member, index)))
        })
        .text(|plugin, mq, key| lookup_buff(plugin, key).and_then(|(entry, _, _)| spell_name(mq, entry.spell_id)).unwrap_or_default());
    mq.add_type("posse_buff", def)
}

/// A value of one of MacroQuest's other types (`spawn`, `buff`...) read from
/// a member's `${Me}`: its text, and any member of it is the path gone on.
fn add_path_type(mq: &Mq<Posse>) -> Result<DataType<String>, DataTypeError> {
    let def = DataTypeDef::new()
        .any_member(MACROQUEST_MEMBERS, |plugin: &mut Posse, _, key: &String, member, index| {
            let (name, path) = key.split_once('\n')?;
            plugin.path_value(name, &format!("{path}.{}", step(member, index)))
        })
        .text(|plugin, _, key| key.split_once('\n').map_or(String::new(), |(name, path)| plugin.path_text(name, path)));
    mq.add_type("posse_path", def)
}

/// A channel, by the name it was created with.
fn add_channel_type(mq: &Mq<Posse>) -> Result<DataType<String>, DataTypeError> {
    let def = DataTypeDef::new()
        .member("Count", |plugin: &mut Posse, _, name: &String, _| Some((plugin.channel_members(name)?.len() as u32).into()))
        .member("Members", |plugin, _, name, _| Some(plugin.channel_members(name)?.join(" ").into()))
        .text(|_, _, name| name.clone());
    mq.add_type("posse_channel", def)
}

fn add_last_command_type(mq: &Mq<Posse>) -> Result<DataType<()>, DataTypeError> {
    let def = DataTypeDef::new()
        .member("Targets", |plugin: &mut Posse, _, _: &(), _| Some(plugin.last_command_tally()?.targets.into()))
        .member("Pending", |plugin, _, _, _| Some(plugin.last_command_tally()?.pending.into()))
        .member("Done", |plugin, _, _, _| Some(plugin.last_command_tally()?.done.into()))
        .member("Refused", |plugin, _, _, _| Some(plugin.last_command_tally()?.refused.into()))
        .member("Dropped", |plugin, _, _, _| Some(plugin.last_command_tally()?.dropped.into()))
        .member("TimedOut", |plugin, _, _, _| Some(plugin.last_command_tally()?.timed_out.into()))
        .member("Lost", |plugin, _, _, _| Some(plugin.last_command_tally()?.lost.into()))
        .text(|plugin, _, _| plugin.connection.as_ref().and_then(|connection| connection.last_command.as_ref()).map_or(String::new(), |last| last.text().to_owned()));
    mq.add_type("posse_last_command", def)
}

fn add_posse_type(mq: &Mq<Posse>) -> Result<DataType<()>, DataTypeError> {
    let def = DataTypeDef::new()
        .member("Count", |plugin: &mut Posse, _, _: &(), _| Some((plugin.connection.as_ref()?.roster.members().len() as u32).into()))
        .member("Members", |plugin, _, _, _| {
            let names: Vec<&str> = plugin.connection.as_ref()?.roster.members().iter().map(|member| member.name.as_str()).collect();
            Some(names.join(" ").into())
        })
        .member("Member", |plugin, _, _, index| plugin.member_value(index))
        .member("Channels", |plugin, _, _, _| Some(plugin.connection.as_ref()?.mapping.region().channel_names().join(" ").into()))
        .member("Channel", |plugin, _, _, index| {
            let name = plugin.connection.as_ref()?.mapping.region().channel(index)?;
            Some(plugin.types.as_ref()?.channel.value(name))
        })
        .member("LastCommand", |plugin, _, _, _| {
            plugin.connection.as_ref()?;
            Some(plugin.types.as_ref()?.last_command.value(()))
        })
        .text(|plugin, _, _| plugin.connection.as_ref().map_or(String::new(), |connection| connection.name.clone()));
    mq.add_type("posse", def)
}

/// Adds the datatypes, then the TLO.
pub fn add(mq: &Mq<Posse>) -> Result<Types, String> {
    let describe = |e: DataTypeError| e.to_string();
    let types = Types {
        member: add_member_type(mq).map_err(describe)?,
        pet: add_pet_type(mq).map_err(describe)?,
        buff: add_buff_type(mq).map_err(describe)?,
        path: add_path_type(mq).map_err(describe)?,
        channel: add_channel_type(mq).map_err(describe)?,
        last_command: add_last_command_type(mq).map_err(describe)?,
        posse: add_posse_type(mq).map_err(describe)?,
    };
    mq.add_tlo("Posse", |plugin: &mut Posse, _, index| {
        plugin.connection.as_ref()?;
        if index.is_empty() { Some(plugin.types.as_ref()?.posse.value(())) } else { plugin.member_value(index) }
    })
    .map_err(|e: TloError| e.to_string())?;
    Ok(types)
}

impl Types {
    pub fn member_value(&self, name: String) -> Value {
        self.member.value(name)
    }

    pub fn path_value(&self, member: &str, path: &str) -> Value {
        self.path.value(path_key(member, path))
    }
}
