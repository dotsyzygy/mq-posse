//! What MQ2Posse's clients share, with nothing from MacroQuest or Windows in
//! it, so that it can be tested without the game.

mod board;
mod channels;
mod commands;
mod names;
mod roster;
mod status;
#[cfg(test)]
mod tlo_docs;

pub use board::{AttachError, ClaimError, LAYOUT_VERSION, Liveness, MAX_CLIENTS, Owner, REGION_SIZE, Region, SlotId};
pub use channels::{ChannelError, MAX_CHANNELS};
pub use commands::{ALIASES, Alias, Answer, Audience, COMMAND_TIMEOUT_MS, CommandTooLong, Complaint, Complaints, Delivery, Inbox, LastCommand, MAX_COMMAND_LEN, SUBCOMMANDS, Scope, Tally, Targets};
pub use names::{InvalidPosseName, MAX_CHANNEL_NAME, compass, mapping_name, validate_channel_name, validate_posse_name};
pub use roster::{Member, Roster};
pub use status::{BuffEntry, MAX_BUFFS, MAX_CHANNELS_PER_CLIENT, MAX_SONGS, STALE_AFTER_MS, State, Status, TooManyChannels, flags};
