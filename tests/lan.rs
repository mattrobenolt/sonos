//! Live LAN tests — run explicitly with `cargo test -- --ignored`.
//! Some mutate household state (documented per test) and restore it.

use std::time::Duration;

use sonos::{discover, snapshot};

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
    assert!(state.groups.len() >= 4, "expected >= 4 groups, got {}", state.groups.len());
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
    std::thread::sleep(Duration::from_secs(1));

    let joined = snapshot(&ips);
    assert!(
        joined.groups.iter().any(|g| g.group.label() == "Office + Bedroom"),
        "Bedroom did not join Office"
    );

    sonos::sonos::control::leave(bedroom.group.coordinator_ip().unwrap()).unwrap();
    std::thread::sleep(Duration::from_secs(1));

    let restored = snapshot(&ips);
    assert!(
        restored.groups.iter().any(|g| g.group.label() == "Bedroom" && g.group.rooms.len() == 1),
        "Bedroom did not return to standalone"
    );
}
