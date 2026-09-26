//! Live LAN tests — run explicitly with `cargo test -- --ignored`.
//! Some mutate household state (documented per test) and restore it.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use sonos::{SystemState, discover, snapshot};

/// The LAN tests mutate and observe the same physical household — libtest
/// runs tests in parallel by default, so a concurrent observer reads a
/// round-trip mid-join as a genuinely grouped household (a real failure:
/// snapshots saw "Bedroom + Office" and "3 groups" mid-round-trip, which
/// was first — wrongly — blamed on the Move napping). Serialize on this.
static LAN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Poison-recovering guard: a failed test must not block the next one —
/// each test restores the shape it found, so a re-run heals state.
fn lan_guard() -> std::sync::MutexGuard<'static, ()> {
    match LAN_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Poll snapshots until `check` passes or the deadline elapses. Sonos
/// propagates topology changes asynchronously; fixed sleeps flake.
fn wait_for_state(ips: &[Ipv4Addr], check: impl Fn(&SystemState) -> bool) -> SystemState {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let state = snapshot(ips);
        if check(&state) || Instant::now() > deadline {
            return state;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "hits the LAN"]
fn discovers_at_least_one_speaker() {
    let _guard = lan_guard();
    let ips = discover(Duration::from_secs(3)).unwrap();
    assert!(!ips.is_empty(), "no ZonePlayer SSDP responses");
}

#[test]
#[ignore = "hits the LAN"]
fn snapshots_group_state() {
    let _guard = lan_guard();
    let ips = discover(Duration::from_secs(3)).unwrap();
    let state = snapshot(&ips);

    // Assert ROOM presence, not group shape: the household may be grouped
    // any way at test time, and the battery Move sleeps on and off the
    // network (group counts change).
    let room_names: Vec<String> = state
        .groups
        .iter()
        .flat_map(|g| g.group.visible_rooms().map(|r| r.name.clone()))
        .collect();
    for expected in ["Family Room", "Office", "Bedroom"] {
        assert!(
            room_names.iter().any(|name| name == expected),
            "missing room {expected} (rooms: {room_names:?})"
        );
    }
    for group in &state.groups {
        assert!(
            group.volume.is_some(),
            "no volume for {}",
            group.group.label()
        );
    }
    // The Office stereo pair always carries one invisible bonded twin,
    // whatever else is grouped with it.
    let office = state
        .groups
        .iter()
        .find(|g| g.group.visible_rooms().any(|r| r.name == "Office"))
        .expect("Office room");
    assert_eq!(
        office.group.rooms.iter().filter(|r| r.invisible).count(),
        1,
        "Office pair invisible twin"
    );
}

#[test]
#[ignore = "mutates: joins the Office pair to Bedroom and back, always restoring the starting shape"]
fn pair_joins_and_leaves_as_a_unit() {
    let _guard = lan_guard();
    let ips = discover(Duration::from_secs(3)).unwrap();
    let state = snapshot(&ips);

    let group_with = |room: &str| {
        state
            .groups
            .iter()
            .find(|g| g.group.visible_rooms().any(|r| r.name == room))
            .unwrap_or_else(|| panic!("no {room} room"))
            .clone()
    };
    let bedroom = group_with("Bedroom");
    let office = group_with("Office");
    let pair_ip = office
        .group
        .visible_rooms()
        .find(|r| r.name == "Office")
        .unwrap()
        .ip;
    let was_grouped = bedroom.group.coordinator_uuid == office.group.coordinator_uuid;

    let merged_shape = |state: &SystemState| {
        state
            .groups
            .iter()
            .any(|g| g.group.label() == "Bedroom + Office" && g.group.rooms.len() == 3)
    };
    let split_shape = |state: &SystemState| {
        state
            .groups
            .iter()
            .any(|g| g.group.label() == "Office" && g.group.rooms.len() == 2)
    };

    if was_grouped {
        sonos::sonos::control::leave(pair_ip).unwrap();
        let split = wait_for_state(&ips, split_shape);
        assert!(split_shape(&split), "pair did not leave Bedroom");
        sonos::sonos::control::join(pair_ip, &bedroom.group.coordinator_uuid).unwrap();
        let merged = wait_for_state(&ips, merged_shape);
        assert!(merged_shape(&merged), "pair did not rejoin Bedroom");
    } else {
        sonos::sonos::control::join(pair_ip, &bedroom.group.coordinator_uuid).unwrap();
        let merged = wait_for_state(&ips, merged_shape);
        assert!(merged_shape(&merged), "pair did not join Bedroom");
        sonos::sonos::control::leave(pair_ip).unwrap();
        let split = wait_for_state(&ips, split_shape);
        assert!(split_shape(&split), "pair did not return to standalone");
    }
}

#[test]
#[ignore = "mutates: regroups Bedroom/Office either way, always restoring the starting shape"]
fn join_and_leave_round_trip() {
    let _guard = lan_guard();
    let ips = discover(Duration::from_secs(3)).unwrap();
    let state = snapshot(&ips);

    // The household may be grouped any way at test time; find the rooms'
    // current groups and end the test in the shape we found.
    let group_with = |room: &str| {
        state
            .groups
            .iter()
            .find(|g| g.group.visible_rooms().any(|r| r.name == room))
            .unwrap_or_else(|| panic!("no {room} room"))
            .clone()
    };
    let office = group_with("Office");
    let bedroom = group_with("Bedroom");
    let bedroom_ip = bedroom
        .group
        .visible_rooms()
        .find(|r| r.name == "Bedroom")
        .unwrap()
        .ip;
    let was_grouped = office.group.coordinator_uuid == bedroom.group.coordinator_uuid;

    let joined_shape = |state: &SystemState| {
        state
            .groups
            .iter()
            .any(|g| g.group.label() == "Bedroom + Office")
    };
    let standalone_shape = |state: &SystemState| {
        state
            .groups
            .iter()
            .any(|g| g.group.label() == "Bedroom" && g.group.rooms.len() == 1)
    };

    if was_grouped {
        sonos::sonos::control::leave(bedroom_ip).unwrap();
        let standalone = wait_for_state(&ips, standalone_shape);
        assert!(
            standalone_shape(&standalone),
            "Bedroom did not leave Office"
        );
        sonos::sonos::control::join(bedroom_ip, &office.group.coordinator_uuid).unwrap();
        let rejoined = wait_for_state(&ips, joined_shape);
        assert!(joined_shape(&rejoined), "Bedroom did not rejoin Office");
    } else {
        sonos::sonos::control::join(bedroom_ip, &office.group.coordinator_uuid).unwrap();
        let joined = wait_for_state(&ips, joined_shape);
        assert!(joined_shape(&joined), "Bedroom did not join Office");
        sonos::sonos::control::leave(bedroom_ip).unwrap();
        let restored = wait_for_state(&ips, standalone_shape);
        assert!(
            standalone_shape(&restored),
            "Bedroom did not return to standalone"
        );
    }
}
