#![allow(non_snake_case)] // the crate is named after the DLL

mod publish;
mod tlo;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mq_emu::{GameState, Ini, IniError, Mq, Plugin, Value};
use posse_core::{ALIASES, Alias, Audience, Delivery, Inbox, Owner, Roster, SUBCOMMANDS, Scope, SlotId, State, Status, mapping_name, validate_posse_name};

use posse_shm::{Mapping, Processes, now_ms, this_process};
use tlo::Types;

const COMMAND: &str = "/posse";
const DEFAULT_POSSE: &str = "default";
const DEFAULT_SECTION: &str = "Default";
const USAGE: &str = "usage: /posse [help | list | join <name> | leave | debug on|off | stats [reset]]";
const SETTINGS_FILE: &str = "MQ2Posse.ini";
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
    inbox: Inbox,
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

impl AgeStats {
    fn add(&mut self, age_ms: u64) {
        self.samples += 1;
        self.total_ms += age_ms;
        self.worst_ms = self.worst_ms.max(age_ms);
    }
}

/// What publishing costs and how fresh others' status is, for `/posse stats`.
#[derive(Default)]
struct Stats {
    pulses: u64,
    total: Duration,
    worst: Duration,
    ages: BTreeMap<String, AgeStats>,
    /// From being sent to being picked up here.
    commands: AgeStats,
}

struct Posse {
    types: Option<Types>,
    connection: Option<Connection>,
    /// The character the settings were last read for.
    settings_for: String,
    /// `/posse leave` was used, so settings don't rejoin.
    left: bool,
    /// `AllowCommands` in the settings: whether to run what others send.
    allow_commands: bool,
    /// `Debug` in the settings: whether to say in chat what's sent and received.
    debug: bool,
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
        for alias in &ALIASES {
            if let Err(e) = mq.add_command(alias.name, move |posse: &mut Posse, mq: &Mq<Posse>, args: &str| posse.send_command(mq, alias, args)) {
                mq.chat(format!("[Posse] couldn't add {}: {e}", alias.name));
            }
        }
        Posse { types, connection: None, settings_for: String::new(), left: false, allow_commands: true, debug: false, zoning: false, status: Status::default(), stats: Stats::default() }
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
        self.run_commands(mq);
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
                self.stats.ages.entry(member.name.clone()).or_default().add(age);
            }
        }
    }

    /// Reads a character's settings, each time it enters the game.
    fn apply_settings(&mut self, mq: &Mq<Self>, character: String) {
        self.settings_for = character.clone();
        let section = settings_section(mq, &character);
        let ini = settings_file(mq).inspect_err(|e| mq.chat(format!("[Posse] couldn't open {SETTINGS_FILE}, so using default settings: {e}"))).ok();
        let read = |key: &str| ini.as_ref().and_then(|ini| ini.get(&section, key).or_else(|| ini.get(DEFAULT_SECTION, key)));
        self.allow_commands = read("AllowCommands").is_none_or(|value| value != "0");
        self.debug = ini.as_ref().and_then(|ini| ini.get_bool(&section, "Debug").or_else(|| ini.get_bool(DEFAULT_SECTION, "Debug"))).unwrap_or(false);
        if self.left {
            return;
        }
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
        let inbox = Inbox::new(mapping.region(), slot);
        self.connection = Some(Connection { name: posse.to_owned(), mapping, slot, me, roster: Roster::default(), inbox, processes, refreshed: Instant::now() });
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
            "help" => print_help(mq),
            "debug" if rest.trim().eq_ignore_ascii_case("on") => self.set_debug(mq, true),
            "debug" if rest.trim().eq_ignore_ascii_case("off") => self.set_debug(mq, false),
            "debug" => mq.chat(format!("[Posse] debug logging is {}. usage: /posse debug on|off", if self.debug { "on" } else { "off" })),
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

    /// Turns debug logging on or off and saves that for this character. It
    /// still changes for now if it can't be saved.
    fn set_debug(&mut self, mq: &Mq<Self>, on: bool) {
        self.debug = on;
        mq.chat(format!("[Posse] debug logging {}", if on { "on" } else { "off" }));
        if self.settings_for.is_empty() {
            mq.chat("[Posse] couldn't save that: no character to save it for yet");
        } else if let Err(e) = settings_file(mq).and_then(|ini| ini.set(&settings_section(mq, &self.settings_for), "Debug", on)) {
            mq.chat(format!("[Posse] couldn't save that to {SETTINGS_FILE}: {e}"));
        }
    }

    /// Handles `/pc` and the rest. The sender's own copy goes through `mq.command`
    /// as well, so it runs on the next pulse and not in the middle of this one.
    fn send_command(&mut self, mq: &Mq<Self>, alias: &Alias, args: &str) {
        let line = args.trim();
        if !line.starts_with('/') {
            mq.chat(format!("[Posse] usage: {} <command>, starting with /", alias.name));
            return;
        }
        let Some(connection) = &mut self.connection else {
            mq.chat("[Posse] not in a posse");
            return;
        };
        let audience = match alias.scope {
            Scope::All => Audience::All,
            Scope::Group => {
                let Some(group) = mq.group() else {
                    mq.chat("[Posse] not in a group");
                    return;
                };
                Audience::Names(group.members().map(|member| member.name()).collect())
            }
            Scope::Raid => {
                let Some(raid) = mq.raid() else {
                    mq.chat("[Posse] not in a raid");
                    return;
                };
                Audience::Names(raid.members().map(|member| member.name()).collect())
            }
            Scope::Zone => Audience::Zone { zone: self.status.zone(), instance: self.status.zone_instance },
        };
        connection.refresh();
        let region = connection.mapping.region();
        let targets = connection.roster.targets(region, connection.me, &audience);
        if !targets.is_empty() {
            if let Err(e) = region.send(connection.slot, targets, now_ms(), line) {
                mq.chat(format!("[Posse] couldn't send: {e}"));
                return;
            }
        } else if !alias.includes_sender {
            mq.chat("[Posse] no other clients to send it to");
            return;
        }
        if self.debug {
            let names: Vec<&str> = connection.roster.members().iter().filter(|member| targets.contains(member.slot)).map(|member| member.name.as_str()).collect();
            let names = if names.is_empty() { "no other clients".to_owned() } else { names.join(", ") };
            let here = if alias.includes_sender { ", and running it here" } else { "" };
            mq.chat(format!("[Posse] {} sent {line} to {names}{here}", alias.name));
        }
        if alias.includes_sender {
            run(mq, line);
        }
    }

    /// Runs what others have sent. Not during a load screen: those wait, and
    /// are dropped if they've waited too long.
    fn run_commands(&mut self, mq: &Mq<Self>) {
        if self.zoning || mq.game_state() != GameState::InGame || mq.me().is_none() {
            return;
        }
        let Some(connection) = &mut self.connection else { return };
        let region = connection.mapping.region();
        let sender = |from: SlotId| region.read(from).map_or_else(|| "another client".to_owned(), |status| status.name());
        for delivery in connection.inbox.receive(region, now_ms()) {
            if let Delivery::Run { from, text, age_ms } = &delivery {
                self.stats.commands.add(*age_ms);
                if self.debug {
                    mq.chat(format!("[Posse] received {text} from {}, {age_ms} ms after it was sent", sender(*from)));
                }
            }
            match delivery {
                Delivery::Run { from, text, .. } if !self.allow_commands => mq.chat(format!("[Posse] refused {text} from {}: AllowCommands is 0", sender(from))),
                Delivery::Run { text, .. } => run(mq, &text),
                Delivery::TooLate { from, text, age_ms } => mq.chat(format!("[Posse] didn't run {text} from {}: it's {} s old", sender(from), age_ms / 1000)),
                Delivery::Lost { from, count } => mq.chat(format!("[Posse] {} sent commands too fast: {count} lost, not all of them meant for this client", sender(from))),
            }
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
        let commands = &self.stats.commands;
        mq.chat(format!("  commands from others: {} ms to arrive on average, {} ms at worst, {} received", commands.total_ms / commands.samples.max(1), commands.worst_ms, commands.samples));
    }
}

fn settings_file(mq: &Mq<Posse>) -> Result<Ini, IniError> {
    Ini::new(mq.config_dir().join(SETTINGS_FILE))
}

fn settings_section(mq: &Mq<Posse>, character: &str) -> String {
    format!("{}_{character}", mq.parse("${EverQuest.Server}"))
}

/// Runs a command on the next pulse, saying so if MacroQuest won't take it.
fn run(mq: &Mq<Posse>, line: &str) {
    if !mq.command(line) {
        mq.chat(format!("[Posse] couldn't run {line}"));
    }
}

fn print_help(mq: &Mq<Posse>) {
    mq.chat("[Posse] commands:");
    for (usage, description) in SUBCOMMANDS {
        mq.chat(format!("  {usage}: {description}"));
    }
    for alias in &ALIASES {
        mq.chat(format!("  {} <command>: {}", alias.name, alias.description));
    }
}

mq_emu::plugin!(Posse);
