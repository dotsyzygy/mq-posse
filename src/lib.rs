#![allow(non_snake_case)] // the crate is named after the DLL

mod publish;
mod tlo;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mq_emu::{green, red, teal, yellow, GameState, Ini, IniError, Mq, Plugin, Value};
use posse_core::{ALIASES, Alias, Answer, Audience, Complaints, Delivery, HELP, HelpTopic, Inbox, LastCommand, Owner, Roster, SUBCOMMANDS, Scope, SlotId, State, Status, Tally, COMMAND_TIMEOUT_MS, mapping_name, validate_channel_name, validate_posse_name};

use posse_shm::{Mapping, Processes, now_ms, this_process};
use tlo::Types;

const COMMAND: &str = "/posse";
const DEFAULT_POSSE: &str = "default";
const DEFAULT_SECTION: &str = "Default";
const USAGE: &str = "usage: /posse [help [all | <topic>] | list | join <name> | leave | channel [list | create|delete|join|leave <name>] | allowcommands true|false | debug on|off | stats [reset]]";
const CHANNEL_USAGE: &str = "usage: /posse channel [list | create <name> | delete <name> | join <name> | leave <name>]";
const SETTINGS_FILE: &str = "MQ2Posse.ini";
/// The key in a character's section listing the channels it's in.
const CHANNELS_KEY: &str = "Channels";
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
    complaints: Complaints,
    last_command: Option<LastCommand>,
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

    /// What's become of the last command sent, or all zeros if none has been.
    fn last_command_tally(&self) -> Option<Tally> {
        let connection = self.connection.as_ref()?;
        Some(connection.last_command.as_ref().map_or_else(Tally::default, |last| last.tally(connection.mapping.region(), connection.slot, now_ms())))
    }

    /// The names of the online members of a channel, separated by spaces.
    fn channel_members(&self, channel: &str) -> Option<Vec<String>> {
        let connection = self.connection.as_ref()?;
        Some(connection.roster.in_channel(connection.mapping.region(), channel).iter().map(|member| member.name.clone()).collect())
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
        let switch = |key: &str| ini.as_ref().and_then(|ini| ini.get_bool(&section, key).or_else(|| ini.get_bool(DEFAULT_SECTION, key)));
        self.allow_commands = switch("AllowCommands").unwrap_or(true);
        self.debug = switch("Debug").unwrap_or(false);
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
                self.load_channels(mq);
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
        let mapping = Mapping::open(&mapping_name(&server(mq), posse)).map_err(|e| e.to_string())?;
        let mut processes = Processes::default();
        let slot = mapping.region().claim(me, &mut processes).map_err(|_| "the posse is full")?;
        let inbox = Inbox::new(mapping.region(), slot);
        let complaints = Complaints::new(mapping.region(), slot);
        self.connection = Some(Connection { name: posse.to_owned(), mapping, slot, me, roster: Roster::default(), inbox, complaints, last_command: None, processes, refreshed: Instant::now() });
        Ok(())
    }

    /// Puts this posse's saved channels in the shared table, as nothing else
    /// does once every client has left, and this character back in its own.
    /// One deleted while it was away is forgotten.
    fn load_channels(&mut self, mq: &Mq<Self>) {
        self.status.set_channels(&[]);
        let Some(connection) = &self.connection else { return };
        let ini = match settings_file(mq) {
            Ok(ini) => ini,
            Err(e) => {
                mq.chat(format!("[Posse] couldn't open {SETTINGS_FILE}, so no channels were restored: {e}"));
                return;
            }
        };
        let region = connection.mapping.region();
        for name in ini.keys(&channels_section(mq, &connection.name)) {
            if let Err(e) = region.create_channel(&name) {
                mq.chat(format!("[Posse] couldn't restore channel {name}: {e}"));
            }
        }
        if self.settings_for.is_empty() {
            return;
        }
        // Saved channels this posse doesn't have stay saved, as they may be
        // another posse's, or come back if the channel is created again.
        let saved = saved_channels(&ini, &settings_section(mq, &self.settings_for));
        let kept: Vec<String> = saved.iter().filter_map(|name| validate_channel_name(name).ok().and_then(|()| region.channel(name))).collect();
        self.status.set_channels(&kept);
    }

    /// Adds a channel to the ones saved for this character, or removes one,
    /// leaving the rest as they are. It stays changed for now if that can't be
    /// saved.
    fn save_channel(&self, mq: &Mq<Self>, channel: &str, joined: bool) {
        if self.settings_for.is_empty() {
            mq.chat("[Posse] couldn't save that: no character to save it for yet");
            return;
        }
        let section = settings_section(mq, &self.settings_for);
        let saved = settings_file(mq).and_then(|ini| {
            let mut channels = saved_channels(&ini, &section);
            channels.retain(|name| !name.eq_ignore_ascii_case(channel));
            if joined {
                channels.push(channel.to_owned());
            }
            if channels.is_empty() { ini.remove(&section, CHANNELS_KEY) } else { ini.set(&section, CHANNELS_KEY, channels.join(",")) }
        });
        if let Err(e) = saved {
            mq.chat(format!("[Posse] couldn't save that to {SETTINGS_FILE}: {e}"));
        }
    }

    fn channel_command(&mut self, mq: &Mq<Self>, args: &str) {
        let (action, name) = split_word(args);
        let action = action.to_ascii_lowercase();
        let Some(connection) = &mut self.connection else {
            mq.chat("[Posse] not in a posse");
            return;
        };
        connection.refresh();
        if action.is_empty() || action == "list" {
            self.list_channels(mq);
            return;
        }
        if name.is_empty() || !matches!(action.as_str(), "create" | "delete" | "join" | "leave") {
            mq.chat(format!("[Posse] {CHANNEL_USAGE}"));
            return;
        }
        let region = connection.mapping.region();
        let section = channels_section(mq, &connection.name);
        match action.as_str() {
            "create" => match region.create_channel(name) {
                Ok(created) => {
                    if let Err(e) = settings_file(mq).and_then(|ini| ini.set(&section, name, 1)) {
                        mq.chat(format!("[Posse] couldn't save that to {SETTINGS_FILE}: {e}"));
                    }
                    mq.chat(if created { format!("[Posse] created channel {name}") } else { format!("[Posse] channel {name} already exists") });
                }
                Err(e) => mq.chat(format!("[Posse] couldn't create {name}: {e}")),
            },
            "delete" => {
                let members = self.channel_members(name).unwrap_or_default();
                if !members.is_empty() {
                    mq.chat(format!("[Posse] couldn't delete {name}: {} still in it", members.join(", ")));
                    return;
                }
                let Some(connection) = &self.connection else { return };
                if !connection.mapping.region().delete_channel(name) {
                    mq.chat(format!("[Posse] no channel named {name}"));
                    return;
                }
                if let Err(e) = settings_file(mq).and_then(|ini| ini.remove(&section, name)) {
                    mq.chat(format!("[Posse] couldn't save that to {SETTINGS_FILE}: {e}"));
                }
                mq.chat(format!("[Posse] deleted channel {name}"));
            }
            "join" => {
                let Some(channel) = region.channel(name) else {
                    mq.chat(format!("[Posse] no channel named {name}. /posse channel create {name} makes one"));
                    return;
                };
                match self.status.join_channel(&channel) {
                    Ok(true) => {
                        self.save_channel(mq, &channel, true);
                        self.publish(mq);
                        mq.chat(format!("[Posse] joined channel {channel}"));
                    }
                    Ok(false) => mq.chat(format!("[Posse] already in channel {channel}")),
                    Err(e) => mq.chat(format!("[Posse] couldn't join {channel}: {e}")),
                }
            }
            _ => {
                if self.status.leave_channel(name) {
                    self.save_channel(mq, name, false);
                    self.publish(mq);
                    mq.chat(format!("[Posse] left channel {name}"));
                } else {
                    mq.chat(format!("[Posse] not in channel {name}"));
                }
            }
        }
    }

    fn list_channels(&self, mq: &Mq<Self>) {
        let Some(connection) = &self.connection else { return };
        let names = connection.mapping.region().channel_names();
        if names.is_empty() {
            mq.chat("[Posse] no channels. /posse channel create <name> makes one");
            return;
        }
        mq.chat(format!("[Posse] channels in {}:", connection.name));
        for name in names {
            let members = self.channel_members(&name).unwrap_or_default();
            mq.chat(format!("  {name}: {}", if members.is_empty() { "nobody online".to_owned() } else { members.join(", ") }));
        }
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
            "help" => print_help(mq, rest.trim()),
            "debug" if rest.trim().eq_ignore_ascii_case("on") => self.set_debug(mq, true),
            "debug" if rest.trim().eq_ignore_ascii_case("off") => self.set_debug(mq, false),
            "debug" => mq.chat(format!("[Posse] debug logging is {}. usage: /posse debug on|off", if self.debug { "on" } else { "off" })),
            "allowcommands" => match rest.trim().to_ascii_lowercase().as_str() {
                "true" | "on" => self.set_allow_commands(mq, true),
                "false" | "off" => self.set_allow_commands(mq, false),
                _ => mq.chat(format!("[Posse] AllowCommands is {}. usage: /posse allowcommands true|false", self.allow_commands)),
            },
            "join" if !rest.trim().is_empty() => {
                let posse = rest.trim();
                self.left = false;
                if self.join(mq, posse) {
                    mq.chat(format!("[Posse] joined {posse}"));
                }
            }
            "channel" => self.channel_command(mq, rest),
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

    /// Turns debug logging on or off and saves that for this character.
    fn set_debug(&mut self, mq: &Mq<Self>, on: bool) {
        self.debug = on;
        mq.chat(format!("[Posse] debug logging {}", if on { "on" } else { "off" }));
        self.save_setting(mq, "Debug", on);
    }

    /// Runs or refuses what others send from now on, and saves that for this
    /// character.
    fn set_allow_commands(&mut self, mq: &Mq<Self>, allow: bool) {
        self.allow_commands = allow;
        mq.chat(format!("[Posse] {} commands from other clients", if allow { "running" } else { "refusing" }));
        self.save_setting(mq, "AllowCommands", allow);
    }

    /// Saves a setting for this character. It still changes for now if it can't
    /// be saved.
    fn save_setting(&self, mq: &Mq<Self>, key: &str, value: bool) {
        if self.settings_for.is_empty() {
            mq.chat("[Posse] couldn't save that: no character to save it for yet");
        } else if let Err(e) = settings_file(mq).and_then(|ini| ini.set(&settings_section(mq, &self.settings_for), key, value)) {
            mq.chat(format!("[Posse] couldn't save that to {SETTINGS_FILE}: {e}"));
        }
    }

    /// Handles `/pc` and the rest.
    fn send_command(&mut self, mq: &Mq<Self>, alias: &Alias, args: &str) {
        let named = matches!(alias.scope, Scope::Name | Scope::Channel);
        let (first, line) = if named { split_word(args) } else { ("", args.trim()) };
        if !line.starts_with('/') || (named && first.is_empty()) {
            mq.chat(format!("[Posse] usage: {}, starting with /", alias.usage()));
            return;
        }
        let Some(connection) = &mut self.connection else {
            mq.chat("[Posse] not in a posse");
            return;
        };
        let mut here = alias.includes_sender;
        let audience = match alias.scope {
            Scope::Name => {
                connection.refresh();
                let Some(member) = connection.roster.find(first) else {
                    mq.chat(format!("[Posse] {first} isn't in the posse"));
                    return;
                };
                here = member.owner == connection.me;
                Audience::Names(vec![first.to_owned()])
            }
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
            Scope::Channel => {
                if connection.mapping.region().channel(first).is_none() {
                    mq.chat(format!("[Posse] no channel named {first}"));
                    return;
                }
                here = here && self.status.in_channel(first);
                Audience::Channel(first.to_owned())
            }
        };
        self.deliver(mq, alias.name, &audience, here, line);
    }

    /// Sends a command to an audience, and runs it here too if `here` is set. The
    /// sender's own copy goes through `mq.command` as well, so it runs on the next
    /// pulse and not in the middle of this one.
    fn deliver(&mut self, mq: &Mq<Self>, sent_by: &str, audience: &Audience, here: bool, line: &str) {
        let Some(connection) = &mut self.connection else { return };
        connection.refresh();
        let region = connection.mapping.region();
        let targets = connection.roster.targets(region, connection.me, audience);
        let sent_ms = now_ms();
        let mut sequence = 0;
        if !targets.is_empty() {
            match region.send(connection.slot, targets, sent_ms, line) {
                Ok(sent) => sequence = sent,
                Err(e) => {
                    mq.chat(format!("[Posse] couldn't send: {e}"));
                    return;
                }
            }
        } else if !here {
            mq.chat("[Posse] no other clients to send it to");
            return;
        }
        if self.debug {
            let names: Vec<&str> = connection.roster.members().iter().filter(|member| targets.contains(member.slot)).map(|member| member.name.as_str()).collect();
            let names = if names.is_empty() { "no other clients".to_owned() } else { names.join(", ") };
            let running_here = if here { ", and running it here" } else { "" };
            mq.chat(format!("[Posse] {sent_by} sent {line} to {names}{running_here}"));
        }
        let answer_here = here.then(|| if run(mq, line) { Answer::Ran } else { Answer::Refused });
        connection.last_command = Some(LastCommand::new(line, sequence, targets, sent_ms, answer_here));
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
            if let Delivery::Run { from, text, age_ms, .. } = &delivery {
                self.stats.commands.add(*age_ms);
                if self.debug {
                    mq.chat(format!("[Posse] received {text} from {}, {age_ms} ms after it was sent", sender(*from)));
                }
            }
            match delivery {
                Delivery::Run { from, sequence, text, .. } if !self.allow_commands => {
                    mq.chat(format!("[Posse] refused {text} from {}: AllowCommands is false", sender(from)));
                    region.answer(connection.slot, from, sequence, Answer::Refused);
                }
                Delivery::Run { from, sequence, text, .. } => {
                    let answer = if run(mq, &text) { Answer::Ran } else { Answer::Refused };
                    region.answer(connection.slot, from, sequence, answer);
                }
                Delivery::TooLate { from, text, age_ms } => mq.chat(format!("[Posse] didn't run {text} from {}: it's {} s old", sender(from), age_ms / 1000)),
                Delivery::Lost { from, count } => mq.chat(format!("[Posse] {} sent commands too fast: {count} lost, not all of them meant for this client", sender(from))),
            }
        }
        for complaint in connection.complaints.check(region) {
            if complaint.dropped > 0 {
                mq.chat(format!("[Posse] {} dropped {} of your commands: they reached it over {} s after you sent them", sender(complaint.from), complaint.dropped, COMMAND_TIMEOUT_MS / 1000));
            }
            if complaint.lost > 0 {
                mq.chat(format!("[Posse] {} lost {} commands you sent, by not reading them before they were overwritten. They may not all have been meant for it", sender(complaint.from), complaint.lost));
                if let Some(last) = &mut connection.last_command {
                    last.add_lost(complaint.lost);
                }
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

/// The server's short name, which keeps posses and settings apart per server.
fn server(mq: &Mq<Posse>) -> String {
    mq.server_name().unwrap_or_default()
}

fn settings_section(mq: &Mq<Posse>, character: &str) -> String {
    format!("{}_{character}", server(mq))
}

/// The section naming a posse's channels. It's shared by all characters, so
/// that a channel outlasts the clients in it.
fn channels_section(mq: &Mq<Posse>, posse: &str) -> String {
    format!("Channels_{}_{posse}", server(mq))
}

/// The channels saved in a character's section.
fn saved_channels(ini: &Ini, section: &str) -> Vec<String> {
    ini.get(section, CHANNELS_KEY).unwrap_or_default().split(',').map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned).collect()
}

/// The first word of `text`, and the rest with no spaces around it.
fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim();
    text.split_once(char::is_whitespace).map_or((text, ""), |(word, rest)| (word, rest.trim()))
}

/// Runs a command on the next pulse, saying so if MacroQuest won't take it.
/// Returns whether it took it.
fn run(mq: &Mq<Posse>, line: &str) -> bool {
    let taken = mq.command(line);
    if !taken {
        mq.chat(format!("[Posse] couldn't run {line}"));
    }
    taken
}

/// `/posse help [all | <topic>]`.
fn print_help(mq: &Mq<Posse>, topic: &str) {
    match topic {
        "" => {
            mq.chat(format!("[Posse] {}", yellow!("help topics:")));
            for (usage, description) in HELP.iter().take(2) {
                print_command(mq, usage, description);
            }
            for topic in HelpTopic::ALL {
                print_command(mq, &format!("/posse help {}", topic.name()), topic.description());
            }
        }
        _ if topic.eq_ignore_ascii_case("all") => {
            mq.chat(format!("[Posse] {}", yellow!("help:")));
            for (usage, description) in HELP {
                print_command(mq, usage, description);
            }
            for topic in HelpTopic::ALL {
                print_topic(mq, topic);
            }
        }
        _ => match HelpTopic::parse(topic) {
            Some(topic) => print_topic(mq, topic),
            None => {
                mq.chat(format!("[Posse] {} isn't a help topic", red!("{topic}")));
                print_help(mq, "");
            }
        },
    }
}

fn print_topic(mq: &Mq<Posse>, topic: HelpTopic) {
    mq.chat(format!("[Posse] {}", yellow!("{} commands:", topic.name())));
    if topic == HelpTopic::Command {
        for alias in &ALIASES {
            print_command(mq, &alias.usage(), alias.description);
        }
    }
    for (_, usage, description) in SUBCOMMANDS.iter().filter(|(t, ..)| *t == topic) {
        print_command(mq, usage, description);
    }
}

/// The command in green and its arguments in teal, so each line's command
/// stands out from the description that wraps under it.
fn print_command(mq: &Mq<Posse>, usage: &str, description: &str) {
    let usage = match usage.find('<') {
        Some(at) => format!("{}{}", green!("{}", usage[..at].trim_end()), teal!(" {}", &usage[at..])),
        None => green!("{usage}"),
    };
    mq.chat(format!("  {usage}: {description}"));
}

mq_emu::plugin!(Posse);
