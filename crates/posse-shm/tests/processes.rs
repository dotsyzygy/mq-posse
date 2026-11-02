//! The shared memory between real processes: each test here runs a copy of
//! this test binary as the other client.

use std::env;
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use posse_core::{Answer, Audience, AttachError, Delivery, Inbox, LAYOUT_VERSION, LastCommand, Liveness, Owner, REGION_SIZE, Roster, Status};
use posse_shm::{Mapping, OpenError, Processes, now_ms, this_process};
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile};

const CHILD_MAPPING: &str = "POSSE_TEST_MAPPING";
const CHILD_NAME: &str = "POSSE_TEST_NAME";
const CHILD_CHANNEL: &str = "POSSE_TEST_CHANNEL";
const WAIT: Duration = Duration::from_secs(10);

/// Not a test: the other client, when run by `spawn_client`. It joins and
/// publishes until it's killed, as a game client does. It "runs" a command by
/// publishing it as its zone and answering, so the test can see that it did.
#[test]
fn other_client() {
    let (Ok(mapping), Ok(name)) = (env::var(CHILD_MAPPING), env::var(CHILD_NAME)) else { return };
    let mapping = Mapping::open(&mapping).unwrap();
    let me = this_process().unwrap();
    let slot = mapping.region().claim(me, &mut Processes::default()).unwrap();
    let mut status = Status::default();
    status.set_name(&name);
    if let Ok(channel) = env::var(CHILD_CHANNEL) {
        status.join_channel(&channel).unwrap();
    }
    let mut inbox = Inbox::new(mapping.region(), slot);
    loop {
        for delivery in inbox.receive(mapping.region(), now_ms()) {
            if let Delivery::Run { from, sequence, text, .. } = delivery {
                status.set_zone(&text);
                mapping.region().answer(slot, from, sequence, Answer::Ran);
            }
        }
        status.set_published_ms(now_ms());
        mapping.region().publish(slot, &status);
        thread::sleep(Duration::from_millis(5));
    }
}

/// Kills the client when dropped, so a failing test doesn't leave it running.
struct Client(Child);

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_client(mapping: &str, name: &str) -> Client {
    Client(client_command(mapping, name).spawn().unwrap())
}

fn spawn_client_in_channel(mapping: &str, name: &str, channel: &str) -> Client {
    Client(client_command(mapping, name).env(CHILD_CHANNEL, channel).spawn().unwrap())
}

fn client_command(mapping: &str, name: &str) -> Command {
    let mut command = Command::new(env::current_exe().unwrap());
    command.args(["--exact", "other_client", "--nocapture"]).env(CHILD_MAPPING, mapping).env(CHILD_NAME, name).stdin(Stdio::null()).stdout(Stdio::null());
    command
}

/// A mapping name no other test run uses.
fn unique(test: &str) -> String {
    format!("Local\\MQ2PosseTest-{test}-{}", std::process::id())
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Makes a mapping as another build would, with this size and first word.
fn other_build_mapping(name: &str, size: u32, first_word: u32) -> impl Drop {
    struct Held(*mut core::ffi::c_void);
    impl Drop for Held {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, ptr::null(), PAGE_READWRITE, 0, size, wide.as_ptr()) };
    assert!(!handle.is_null());
    let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) }.Value;
    assert!(!view.is_null());
    unsafe {
        view.cast::<u32>().write(first_word);
        UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view });
    }
    Held(handle)
}

#[test]
fn clients_see_each_other_across_processes() {
    let name = unique("see");
    let mapping = Mapping::open(&name).unwrap();
    let _bob = spawn_client(&name, "Bob");
    let _cleric = spawn_client(&name, "Cleric");

    let (mut roster, mut processes) = (Roster::default(), Processes::default());
    wait_for("both clients", || {
        roster.refresh(mapping.region(), &mut processes);
        roster.members().len() == 2
    });
    assert!(roster.find("Bob").is_some() && roster.find("Cleric").is_some());
}

#[test]
fn a_command_reaches_only_the_client_addressed_across_processes() {
    let name = unique("command");
    let mapping = Mapping::open(&name).unwrap();
    let region = mapping.region();
    let me = this_process().unwrap();
    let mut processes = Processes::default();
    let slot = region.claim(me, &mut processes).unwrap();
    let _bob = spawn_client(&name, "Bob");
    let _cleric = spawn_client(&name, "Cleric");

    let mut roster = Roster::default();
    wait_for("both clients", || {
        roster.refresh(region, &mut processes);
        roster.members().len() == 2
    });
    let targets = roster.targets(region, me, &Audience::Names(vec!["Bob".to_owned()]));
    region.send(slot, targets, now_ms(), "/sit").unwrap();

    wait_for("Bob to run it", || roster.read(region, "Bob").is_some_and(|status| status.zone() == "/sit"));
    let published = roster.read(region, "Cleric").unwrap().published_ms();
    wait_for("Cleric to read its log after the send", || roster.read(region, "Cleric").is_some_and(|status| status.published_ms() > published));
    assert_eq!(roster.read(region, "Cleric").unwrap().zone(), "", "the command wasn't for Cleric");
}

#[test]
fn a_command_to_a_channel_reaches_only_its_members_and_they_answer_across_processes() {
    let name = unique("channel");
    let mapping = Mapping::open(&name).unwrap();
    let region = mapping.region();
    let me = this_process().unwrap();
    let mut processes = Processes::default();
    let slot = region.claim(me, &mut processes).unwrap();
    let _bob = spawn_client_in_channel(&name, "Bob", "healers");
    let _cleric = spawn_client_in_channel(&name, "Cleric", "healers");
    let _dave = spawn_client(&name, "Dave");

    let mut roster = Roster::default();
    wait_for("all three clients", || {
        roster.refresh(region, &mut processes);
        roster.members().len() == 3
    });
    assert_eq!(roster.in_channel(region, "HEALERS").len(), 2);

    let targets = roster.targets(region, me, &Audience::Channel("healers".to_owned()));
    let sent_ms = now_ms();
    let sequence = region.send(slot, targets, sent_ms, "/sit").unwrap();
    let last = LastCommand::new("/sit", sequence, targets, sent_ms, None);
    assert_eq!(last.tally(region, slot, sent_ms).targets, 2);

    wait_for("both members to answer", || last.tally(region, slot, sent_ms).done == 2);
    assert_eq!(last.tally(region, slot, sent_ms).pending, 0);
    let published = roster.read(region, "Dave").unwrap().published_ms();
    wait_for("Dave to read its log after the send", || roster.read(region, "Dave").is_some_and(|status| status.published_ms() > published));
    assert_eq!(roster.read(region, "Dave").unwrap().zone(), "", "the command wasn't for Dave");
}

#[test]
fn a_client_killed_before_it_answers_times_out_and_leaves_its_channel() {
    let name = unique("killed-target");
    let mapping = Mapping::open(&name).unwrap();
    let region = mapping.region();
    let me = this_process().unwrap();
    let mut processes = Processes::default();
    let slot = region.claim(me, &mut processes).unwrap();
    let mut bob = spawn_client_in_channel(&name, "Bob", "healers");

    let mut roster = Roster::default();
    wait_for("Bob", || {
        roster.refresh(region, &mut processes);
        roster.find("Bob").is_some()
    });
    let targets = roster.targets(region, me, &Audience::Channel("healers".to_owned()));
    bob.0.kill().unwrap();
    bob.0.wait().unwrap();

    let sent_ms = now_ms();
    let sequence = region.send(slot, targets, sent_ms, "/sit").unwrap();
    let last = LastCommand::new("/sit", sequence, targets, sent_ms, None);
    assert_eq!(last.tally(region, slot, sent_ms).pending, 1);
    assert_eq!(last.tally(region, slot, sent_ms + posse_core::COMMAND_TIMEOUT_MS + 1).timed_out, 1);

    roster.refresh(region, &mut processes);
    assert!(roster.in_channel(region, "healers").is_empty());
}

#[test]
fn a_channel_created_in_one_process_is_seen_in_another() {
    let name = unique("channel-table");
    let first = Mapping::open(&name).unwrap();
    first.region().create_channel("Healers").unwrap();
    let second = Mapping::open(&name).unwrap();
    assert_eq!(second.region().channel("healers").as_deref(), Some("Healers"));
    assert!(second.region().delete_channel("healers"));
    assert_eq!(first.region().channel("healers"), None);
}

#[test]
fn every_process_shares_one_clock() {
    let name = unique("clock");
    let mapping = Mapping::open(&name).unwrap();
    let _bob = spawn_client(&name, "Bob");

    let (mut roster, mut processes) = (Roster::default(), Processes::default());
    wait_for("Bob", || {
        roster.refresh(mapping.region(), &mut processes);
        roster.find("Bob").is_some()
    });
    // Bob publishes every few milliseconds, so if the clocks agree his status
    // is always fresh. (A busy test machine can hold him up, but not by this.)
    let age = roster.read(mapping.region(), "Bob").unwrap().age_ms(now_ms());
    assert!(age < 2000, "Bob's status looks {age} ms old");
}

#[test]
fn a_killed_client_drops_out_at_the_next_refresh() {
    let name = unique("kill");
    let mapping = Mapping::open(&name).unwrap();
    let mut bob = spawn_client(&name, "Bob");

    let (mut roster, mut processes) = (Roster::default(), Processes::default());
    wait_for("Bob", || {
        roster.refresh(mapping.region(), &mut processes);
        roster.find("Bob").is_some()
    });

    bob.0.kill().unwrap();
    bob.0.wait().unwrap();
    roster.refresh(mapping.region(), &mut processes);
    assert_eq!(roster.find("Bob"), None, "a dead client must go without waiting for a timeout");
}

#[test]
fn a_killed_clients_slot_is_taken_by_the_next_to_join() {
    let name = unique("reuse");
    let mapping = Mapping::open(&name).unwrap();
    let mut bob = spawn_client(&name, "Bob");

    let (mut roster, mut processes) = (Roster::default(), Processes::default());
    wait_for("Bob", || {
        roster.refresh(mapping.region(), &mut processes);
        roster.find("Bob").is_some()
    });
    let bobs_slot = roster.find("Bob").unwrap().slot;

    bob.0.kill().unwrap();
    bob.0.wait().unwrap();
    assert_eq!(mapping.region().claim(this_process().unwrap(), &mut processes), Ok(bobs_slot));
}

/// The negative for the two tests above: a client still running stays a
/// member, so they're seeing the kill and not just dropping everyone.
#[test]
fn a_running_client_stays_alive() {
    let name = unique("alive");
    let mapping = Mapping::open(&name).unwrap();
    let _bob = spawn_client(&name, "Bob");

    let (mut roster, mut processes) = (Roster::default(), Processes::default());
    wait_for("Bob", || {
        roster.refresh(mapping.region(), &mut processes);
        roster.find("Bob").is_some()
    });
    for _ in 0..20 {
        roster.refresh(mapping.region(), &mut processes);
        assert!(roster.find("Bob").is_some());
    }
}

#[test]
fn a_process_id_reused_by_another_process_is_not_the_old_client() {
    let me = this_process().unwrap();
    let mut processes = Processes::default();
    assert!(processes.is_alive(me));
    assert!(!processes.is_alive(Owner { pid: me.pid, started: me.started + 1 }));
}

#[test]
fn a_process_that_does_not_exist_is_dead() {
    // Process IDs are multiples of 4, so this one can't exist.
    assert!(!Processes::default().is_alive(Owner { pid: 0xFFFF_FFF1, started: 1 }));
}

#[test]
fn a_mapping_from_another_build_is_refused_with_its_version() {
    let name = unique("version");
    let found = LAYOUT_VERSION + 1;
    let _other = other_build_mapping(&name, REGION_SIZE as u32, found);
    assert!(matches!(Mapping::open(&name), Err(OpenError::Attach(AttachError::VersionMismatch { found: f })) if f == found));
}

#[test]
fn a_smaller_mapping_is_refused_rather_than_read_past_its_end() {
    let name = unique("small");
    let _other = other_build_mapping(&name, 4096, LAYOUT_VERSION);
    assert!(matches!(Mapping::open(&name), Err(OpenError::Attach(AttachError::TooSmall))));
}

#[test]
fn a_new_mapping_is_the_full_size() {
    let name = unique("size");
    let mapping = Mapping::open(&name).unwrap();
    let me = this_process().unwrap();
    let slot = mapping.region().claim(me, &mut Processes::default()).unwrap();
    mapping.region().publish(slot, &Status::default());
    assert!(mapping.region().read(slot).is_some());
}
