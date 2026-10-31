#![allow(non_snake_case)] // the crate is named after the DLL

mod publish;
mod tlo;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mq_emu::{GameState, Ini, Mq, Plugin, Value};
use posse_core::{Owner, Roster, SlotId, State, Status, mapping_name, validate_posse_name};

use posse_shm::{Mapping, Processes, now_ms, this_process};
use tlo::Types;

const COMMAND: &str = "/posse";
const DEFAULT_POSSE: &str = "default";
const DEFAULT_SECTION: &str = "Default";
const USAGE: &str = "usage: /posse [list | join <name> | leave | stats [reset]]";
/// How often to check who's alive. Each check asks Windows about every member,
/// so not every pulse.
const ROSTER_REFRESH: Duration = Duration::from_millis(100);

/// Membership of a posse: this client's slot in its shared memory.
struct Connection {
    name: String,
    mapping: Mapping,
    slot: SlotId,
    me: Owner,
    roster: Roster,
    processes: Processes,
    refreshed: Instant,
}

impl Connection {
    fn refresh(&mut self) {
        self.roster.refresh(self.mapping.region(), &mut self.processes);
        self.refreshed = Instant::now();
    }
}

/// How old a member's status was when looked at here, which is how far behind
/// a read of it can be.
#[derive(Default)]
struct AgeStats {
    samples: u64,
    total_ms: u64,
    worst_ms: u64,
}

/// What publishing costs and how fresh others' status is, for `/posse stats`.
#[derive(Default)]
struct Stats {
    pulses: u64,
    total: Duration,
    worst: Duration,
    ages: BTreeMap<String, AgeStats>,
}

struct Posse {
    types: Option<Types>,
    connection: Option<Connection>,
    /// The character the settings were last read for.
    settings_for: String,
    /// `/posse leave` was used, so settings don't rejoin.
    left: bool,
    zoning: bool,
    status: Status,
    stats: Stats,
}

impl Plugin for Posse {
    fn new(mq: &Mq<Self>) -> Self {
        let types = tlo::add(mq).inspect_err(|e| mq.chat(format!("[Posse] couldn't add the Posse TLO: {e}"))).ok();
        if let Err(e) = mq.add_command(COMMAND, Posse::command) {
            mq.chat(format!("[Posse] couldn't add {COMMAND}: {e}"));
        }
        Posse { types, connection: None, settings_for: String::new(), left: false, zoning: false, status: Status::default(), stats: Stats::default() }
    }

    fn shutdown(&mut self, _mq: &Mq<Self>) {
        self.leave();
    }

    fn on_pulse(&mut self, mq: &Mq<Self>) {
        if mq.game_state() == GameState::InGame
            && let Some(name) = mq.me().map(|me| me.name())
            && name != self.settings_for
        {
            self.apply_settings(mq, name);
        }
        self.publish(mq);
    }

    fn on_set_game_state(&mut self, mq: &Mq<Self>, state: GameState) {
        // Camping or dying out of a zone change never ends it.
        if state != GameState::InGame {
            self.zoning = false;
        }
        // There's no character here to be a member, and the next one may not be
        // this one. Entering the game joins again.
        if matches!(state, GameState::CharacterSelect | GameState::CharacterCreate | GameState::LoggingIn) {
            self.leave();
            self.settings_for.clear();
            self.status = Status::default();
            return;
        }
        // Pulses may not run through the change, so others hear of it now.
        self.publish(mq);
    }

    fn on_begin_zone(&mut self, mq: &Mq<Self>) {
        self.zoning = true;
        self.publish(mq);
    }

    fn on_end_zone(&mut self, _mq: &Mq<Self>) {
        self.zoning = false;
    }
}

impl Posse {
    /// The latest status of a member by name.
    fn status_of(&self, name: &str) -> Option<Status> {
        let connection = self.connection.as_ref()?;
        connection.roster.read(connection.mapping.region(), name)
    }

    /// `${Posse.Member[name]}`, under the name it's published with.
    fn member_value(&self, name: &str) -> Option<Value> {
        let member = self.connection.as_ref()?.roster.find(name)?;
        Some(self.types.as_ref()?.member_value(member.name.clone()))
    }

    fn publish(&mut self, mq: &Mq<Self>) {
        let Some(connection) = &mut self.connection else { return };
        let started = Instant::now();
        publish::collect(mq, &mut self.status, self.zoning);
        self.status.set_published_ms(now_ms());
        connection.mapping.region().publish(connection.slot, &self.status);
        let refreshed = connection.refreshed.elapsed() >= ROSTER_REFRESH;
        if refreshed {
            connection.refresh();
        }
        let elapsed = started.elapsed();
        self.stats.pulses += 1;
        self.stats.total += elapsed;
        self.stats.worst = self.stats.worst.max(elapsed);
        if refreshed {
            self.sample_ages();
        }
    }

    /// Notes how old each other member's status is. Only in game: a load
    /// screen isn't how far behind a read normally is.
    fn sample_ages(&mut self) {
        let Some(connection) = &self.connection else { return };
        let now = now_ms();
        for member in connection.roster.members().iter().filter(|member| member.owner != connection.me) {
            let Some(status) = connection.roster.read(connection.mapping.region(), &member.name) else { continue };
            if status.state() == State::InGame {
                let age = status.age_ms(now);
                let stats = self.stats.ages.entry(member.name.clone()).or_default();
                stats.samples += 1;
                stats.total_ms += age;
                stats.worst_ms = stats.worst_ms.max(age);
            }
        }
    }

    /// Reads a character's settings, each time it enters the game.
    fn apply_settings(&mut self, mq: &Mq<Self>, character: String) {
        self.settings_for = character.clone();
        if self.left {
            return;
        }
        let server = mq.parse("${EverQuest.Server}");
        let section = format!("{server}_{character}");
        let ini = Ini::new(mq.config_dir().join("MQ2Posse.ini")).ok();
        let read = |key: &str| ini.as_ref().and_then(|ini| ini.get(&section, key).or_else(|| ini.get(DEFAULT_SECTION, key)));
        let auto_join = read("AutoJoin").is_none_or(|value| value != "0");
        let posse = read("Posse").unwrap_or_else(|| DEFAULT_POSSE.to_owned());
        if !auto_join {
            self.leave();
        } else if self.connection.as_ref().is_none_or(|connection| !connection.name.eq_ignore_ascii_case(&posse)) {
            self.join(mq, &posse);
        }
    }

    /// Leaves any posse and joins `posse`, saying why in chat if it can't.
    fn join(&mut self, mq: &Mq<Self>, posse: &str) -> bool {
        match self.connect(mq, posse) {
            Ok(()) => {
                self.publish(mq);
                if let Some(connection) = &mut self.connection {
                    connection.refresh();
                }
                true
            }
            Err(e) => {
                mq.chat(format!("[Posse] couldn't join {posse}: {e}"));
                false
            }
        }
    }

    fn connect(&mut self, mq: &Mq<Self>, posse: &str) -> Result<(), String> {
        validate_posse_name(posse).map_err(|e| e.to_string())?;
        if mq.game_state() != GameState::InGame {
            return Err("not in game yet".to_owned());
        }
        self.leave();
        let me = this_process().ok_or("Windows wouldn't say when this process started")?;
        let mapping = Mapping::open(&mapping_name(&mq.parse("${EverQuest.Server}"), posse)).map_err(|e| e.to_string())?;
        let mut processes = Processes::default();
        let slot = mapping.region().claim(me, &mut processes).map_err(|_| "the posse is full")?;
        self.connection = Some(Connection { name: posse.to_owned(), mapping, slot, me, roster: Roster::default(), processes, refreshed: Instant::now() });
        Ok(())
    }

    fn leave(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.mapping.region().release(connection.slot, connection.me);
        }
    }

    fn command(&mut self, mq: &Mq<Self>, args: &str) {
        let args = args.trim();
        let (command, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        match command.to_ascii_lowercase().as_str() {
            "" | "list" => self.list(mq),
            "join" if !rest.trim().is_empty() => {
                let posse = rest.trim();
                self.left = false;
                if self.join(mq, posse) {
                    mq.chat(format!("[Posse] joined {posse}"));
                }
            }
            "leave" => {
                self.left = true;
                self.leave();
                mq.chat("[Posse] left");
            }
            "stats" if rest.trim().eq_ignore_ascii_case("reset") => {
                self.stats = Stats::default();
                mq.chat("[Posse] stats reset");
            }
            "stats" => self.print_stats(mq),
            _ => mq.chat(format!("[Posse] {USAGE}")),
        }
    }

    fn list(&mut self, mq: &Mq<Self>) {
        let Some(connection) = &mut self.connection else {
            mq.chat("[Posse] not in a posse");
            return;
        };
        connection.refresh();
        let region = connection.mapping.region();
        mq.chat(format!("[Posse] {}: {} online", connection.name, connection.roster.members().len()));
        let now = now_ms();
        for member in connection.roster.members() {
            if let Some(status) = connection.roster.read(region, &member.name) {
                let stale = if status.is_stale(now) { ", stale" } else { "" };
                mq.chat(format!("  {}: {}, {}, {} ms{stale}", member.name, status.state().as_str(), status.zone(), status.age_ms(now)));
            }
        }
    }

    fn print_stats(&self, mq: &Mq<Self>) {
        let average = self.stats.total.as_micros() / u128::from(self.stats.pulses.max(1));
        mq.chat(format!("[Posse] publishing: {} pulses, {average} us average, {} us worst", self.stats.pulses, self.stats.worst.as_micros()));
        for (name, age) in &self.stats.ages {
            mq.chat(format!("  {name}'s status: {} ms old on average, {} ms at worst, {} samples", age.total_ms / age.samples.max(1), age.worst_ms, age.samples));
        }
    }
}

mq_emu::plugin!(Posse);
