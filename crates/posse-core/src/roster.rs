//! Who's in the block right now.

use crate::board::{Liveness, Owner, Region, SlotId};
use crate::commands::{Audience, Targets};
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

    /// The members in a channel, by name.
    pub fn in_channel(&self, region: &Region, channel: &str) -> Vec<&Member> {
        self.members.iter().filter(|member| region.read(member.slot).unwrap_or(member.status).in_channel(channel)).collect()
    }

    /// The members in an audience, other than `me`.
    pub fn targets(&self, region: &Region, me: Owner, audience: &Audience) -> Targets {
        let mut targets = Targets::default();
        for member in self.members.iter().filter(|member| member.owner != me) {
            if audience.includes(&region.read(member.slot).unwrap_or(member.status)) {
                targets.add(member.slot);
            }
        }
        targets
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
    fn targets_are_the_audience_without_the_sender() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (alice, bob) = (join(region, 1, "Alice"), join(region, 2, "Bob"));
        let carol = join(region, 3, "Carol");
        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());

        let all = roster.targets(region, owner(1), &Audience::All);
        assert!(!all.contains(alice) && all.contains(bob) && all.contains(carol));

        let names = vec!["alice".to_owned(), "bob".to_owned(), "Merc".to_owned()];
        let group = roster.targets(region, owner(1), &Audience::Names(names));
        assert!(!group.contains(alice) && group.contains(bob) && !group.contains(carol));

        let alone = roster.targets(region, owner(1), &Audience::Names(vec!["Alice".to_owned()]));
        assert!(alone.is_empty());
    }

    #[test]
    fn a_channel_holds_the_members_who_list_it_and_the_sender_is_never_a_target() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let (alice, bob) = (join(region, 1, "Alice"), join(region, 2, "Bob"));
        let carol = join(region, 3, "Carol");
        for (slot, name) in [(alice, "Alice"), (bob, "Bob")] {
            let mut status = region.read(slot).unwrap();
            assert_eq!(status.name(), name);
            status.join_channel("Healers").unwrap();
            region.publish(slot, &status);
        }
        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());

        let names: Vec<&str> = roster.in_channel(region, "healers").iter().map(|member| member.name.as_str()).collect();
        assert_eq!(names, ["Alice", "Bob"]);
        assert!(roster.in_channel(region, "tanks").is_empty());

        let targets = roster.targets(region, owner(1), &Audience::Channel("healers".to_owned()));
        assert!(!targets.contains(alice) && targets.contains(bob) && !targets.contains(carol));
    }

    #[test]
    fn a_member_who_left_a_channel_is_out_of_it_before_the_next_refresh() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let bob = join(region, 2, "Bob");
        let mut status = region.read(bob).unwrap();
        status.join_channel("healers").unwrap();
        region.publish(bob, &status);
        let mut roster = Roster::default();
        roster.refresh(region, &mut Dead::default());
        assert_eq!(roster.in_channel(region, "healers").len(), 1);

        status.leave_channel("healers");
        region.publish(bob, &status);
        assert!(roster.in_channel(region, "healers").is_empty());
    }

    #[test]
    fn a_dead_member_is_out_of_its_channels_and_the_next_owner_of_its_slot_starts_in_none() {
        let mut memory = zeroed_memory();
        let region = region(&mut memory);
        let bob = join(region, 2, "Bob");
        let mut status = region.read(bob).unwrap();
        status.join_channel("healers").unwrap();
        region.publish(bob, &status);

        let mut dead = Dead::default();
        dead.0.insert(owner(2));
        let mut roster = Roster::default();
        roster.refresh(region, &mut dead);
        assert!(roster.in_channel(region, "healers").is_empty());

        assert_eq!(region.claim(owner(3), &mut dead).unwrap(), bob);
        assert_eq!(region.read(bob), None, "nothing of Bob's is left to read");
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
