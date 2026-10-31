//! Who's in the block right now.

use crate::board::{Liveness, Owner, Region, SlotId};
use crate::status::Status;

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub slot: SlotId,
    pub owner: Owner,
    pub name: String,
    /// As of the last refresh, for when the slot can't be read.
    status: Status,
}

/// The living members, refreshed now and then so that a read by name doesn't
/// have to go through every slot.
#[derive(Debug, Default)]
pub struct Roster {
    members: Vec<Member>,
}

impl Roster {
    /// Rebuilds the list, leaving out owners that have died and slots with no
    /// name yet, sorted by name.
    pub fn refresh(&mut self, region: &Region, liveness: &mut impl Liveness) {
        self.members.clear();
        for (slot, owner) in region.owners() {
            if !liveness.is_alive(owner) {
                continue;
            }
            if let Some(status) = region.read(slot) {
                let name = status.name();
                if !name.is_empty() {
                    self.members.push(Member { slot, owner, name, status });
                }
            }
        }
        self.members.sort_by_key(|member| member.name.to_lowercase());
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn find(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.name.eq_ignore_ascii_case(name))
    }

    /// A member's latest status. If its client is stopped partway through
    /// publishing, this is the status from the last refresh rather than
    /// nothing, so a macro's math doesn't meet a NULL. None if the slot now
    /// holds another character.
    pub fn read(&self, region: &Region, name: &str) -> Option<Status> {
        let member = self.find(name)?;
        let status = region.read(member.slot).unwrap_or(member.status);
        status.name().eq_ignore_ascii_case(name).then_some(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::tests::{Dead, owner, region, stop_mid_write, zeroed_memory};

    fn join(region: &Region, pid: u32, name: &str) -> SlotId {
        let slot = region.claim(owner(pid), &mut Dead::default()).unwrap();
        let mut status = Status::default();
        status.set_name(name);
        region.publish(slot, &status);
        slot
    }

    #[test]
    fn lists_members_by_name_and_finds_them_in_any_case() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        join(region, 1, "Cleric");
        let bob = join(region, 2, "Bob");

        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());
        let names: Vec<&str> = roster.members().iter().map(|member| member.name.as_str()).collect();
        assert_eq!(names, ["Bob", "Cleric"]);
        assert_eq!(roster.find("BOB").unwrap().slot, bob);
        assert_eq!(roster.find("Nobody"), None);
    }

    #[test]
    fn a_dead_client_drops_out_at_the_next_refresh() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        join(region, 1, "Bob");
        let mut roster = Roster::default();
        let mut live = Dead::default();
        roster.refresh(region, &mut live);
        assert!(roster.find("Bob").is_some());

        live.0.insert(owner(1));
        roster.refresh(region, &mut live);
        assert_eq!(roster.find("Bob"), None);
    }

    #[test]
    fn reads_show_the_latest_status_without_a_refresh() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = join(region, 1, "Bob");
        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());

        let mut status = Status::default();
        status.set_name("Bob");
        status.hp_percent = 40;
        region.publish(slot, &status);
        assert_eq!(roster.read(region, "bob").unwrap().hp_percent, 40);
    }

    #[test]
    fn a_read_during_a_stopped_write_falls_back_to_the_last_refresh() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = join(region, 1, "Bob");
        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());
        stop_mid_write(region, slot);
        assert_eq!(region.read(slot), None);
        assert_eq!(roster.read(region, "Bob").unwrap().name(), "Bob");
    }

    #[test]
    fn a_slot_now_holding_another_character_is_not_read_as_the_old_one() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = join(region, 1, "Bob");
        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());

        let mut status = Status::default();
        status.set_name("Alt");
        region.publish(slot, &status);
        assert_eq!(roster.read(region, "Bob"), None);
    }

    #[test]
    fn a_client_that_left_drops_out() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let slot = join(region, 1, "Bob");
        let mut roster = Roster::default();
        region.release(slot, owner(1));
        roster.refresh(region, &mut Dead::default());
        assert!(roster.members().is_empty());
    }

    #[test]
    fn a_client_yet_to_publish_is_not_listed() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        region.claim(owner(1), &mut Dead::default()).unwrap();
        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());
        assert!(roster.members().is_empty());
    }
}
