//! What MQ2Posse's clients share, with nothing from MacroQuest or Windows in
//! it, so that it can be tested without the game.

mod board;
mod names;
mod roster;
mod status;

pub use board::{AttachError, ClaimError, LAYOUT_VERSION, Liveness, MAX_CLIENTS, Owner, REGION_SIZE, Region, SlotId};
pub use names::{InvalidPosseName, compass, mapping_name, validate_posse_name};
pub use roster::{Member, Roster};
pub use status::{BuffEntry, MAX_BUFFS, MAX_SONGS, STALE_AFTER_MS, State, Status, flags};
