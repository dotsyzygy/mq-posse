//! What MQ2Posse's clients share, with nothing from MacroQuest or Windows in
//! it, so that it can be tested without the game.

mod board;
mod channels;
mod commands;
mod names;
mod paths;
mod roster;
mod status;
#[cfg(test)]
mod tlo_docs;

pub use board::{AttachError, ClaimError, LAYOUT_VERSION, Liveness, MAX_CLIENTS, Owner, REGION_SIZE, Region, SlotId};
pub use channels::{ChannelError, MAX_CHANNELS};
pub use commands::{ALIASES, Alias, Answer, Audience, COMMAND_TIMEOUT_MS, CommandTooLong, Complaint, Complaints, Delivery, HELP, HelpTopic, Inbox, LastCommand, MAX_COMMAND_LEN, SUBCOMMANDS, Scope, Tally, Targets};
pub use names::{DEFAULT_POSSE, DEFAULT_SECTION, InvalidPosseName, MAX_CHANNEL_NAME, SETTINGS_FILE, auto_join_posse, compass, mapping_name, settings_section, validate_channel_name, validate_posse_name};
pub use paths::{COST_FACTOR, Evaluator, InvalidPath, MAX_PATH_LEN, MAX_REQUESTS, MAX_RESULTS, MAX_VALUE_LEN, MIN_INTERVAL_MS, PathResult, PathValue, Reader, Step, UNREAD_AFTER_MS, interval_ms, parse_path};
pub use roster::{Member, Roster};
pub use status::{BuffEntry, MAX_BUFFS, MAX_CHANNELS_PER_CLIENT, MAX_SONGS, STALE_AFTER_MS, State, Status, TooManyChannels, flags};
