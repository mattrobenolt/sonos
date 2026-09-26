//! Live LAN tests — run explicitly with `cargo test -- --ignored`.
//! Some mutate household state (documented per test) and restore it.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use sonos::{discover, snapshot, SystemState};

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
    let ips = discover(Duration::from_secs(3)).unwrap();
    assert!(!ips.is_empty(), "no ZonePlayer SSDP responses");
}

#[test]
#[ignore = "hits the LAN"]
fn snapshots_group_state() {
    let ips = discover(Duration::from_secs(3)).unwrap();
    let state = snapshot(&ips);
    // The battery Move sleeps on and off the network, so assert the
    // always-present groups by name, not a group count.
    let labels: Vec<String> = state.groups.iter().map(|g| g.group.label()).collect();
    for expected in ["Family Room", "Office", "Bedroom"] {
        assert!(
            labels.iter().any(|label| label == expected),
            "missing {expected} (groups: {labels:?})"
        );
    }
    for group in &state.groups {
        assert!(group.volume.is_some(), "no volume for {}", group.group.label());
    }
    let office = state.groups.iter().find(|g| g.group.label() == "Office");
    assert!(office.is_some(), "no Office group");
    assert_eq!(office.unwrap().group.rooms.len(), 2, "Office stereo pair members");
}

#[test]
#[ignore = "mutates: joins Bedroom to the Office pair, then restores it"]
fn join_and_leave_round_trip() {
    let ips = discover(Duration::from_secs(3)).unwrap();
    let state = snapshot(&ips);
    let office = state
        .groups
        .iter()
        .find(|g| g.group.label() == "Office")
        .expect("Office group")
        .clone();
    let bedroom = state
        .groups
        .iter()
        .find(|g| g.group.label() == "Bedroom")
        .expect("Bedroom group")
        .clone();

    sonos::sonos::control::join(
        bedroom.group.coordinator_ip().unwrap(),
        &office.group.coordinator_uuid,
    )
    .unwrap();
    let joined = wait_for_state(&ips, |state| {
        state.groups.iter().any(|g| g.group.label() == "Office + Bedroom")
    });
    assert!(
        joined.groups.iter().any(|g| g.group.label() == "Office + Bedroom"),
        "Bedroom did not join Office"
    );

    sonos::sonos::control::leave(bedroom.group.coordinator_ip().unwrap()).unwrap();
    let restored = wait_for_state(&ips, |state| {
        state
            .groups
            .iter()
            .any(|g| g.group.label() == "Bedroom" && g.group.rooms.len() == 1)
    });
    assert!(
        restored.groups.iter().any(|g| g.group.label() == "Bedroom" && g.group.rooms.len() == 1),
        "Bedroom did not return to standalone"
    );
}
