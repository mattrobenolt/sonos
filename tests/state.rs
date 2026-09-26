//! Eager-UI state transforms (SystemState::joined/left) against the live
//! topology fixture.

use std::net::Ipv4Addr;

use sonos::sonos::topology::parse_groups;
use sonos::{GroupView, SystemState};

const BEDROOM_IP: &str = "192.168.2.114";

fn fixture_state() -> SystemState {
    let xml = std::fs::read_to_string("tests/fixtures/zonegroupstate.xml").unwrap();
    let groups = parse_groups(&xml)
        .unwrap()
        .into_iter()
        .map(|group| GroupView {
            group,
            volume: None,
            transport_state: None,
            now_playing: None,
            room_volumes: Default::default(),
        })
        .collect();
    SystemState { groups }
}

fn office_coordinator(state: &SystemState) -> String {
    state
        .groups
        .iter()
        .find(|view| view.group.label() == "Office")
        .unwrap()
        .group
        .coordinator_uuid
        .clone()
}

#[test]
fn eager_join_merges_and_sorts() {
    let state = fixture_state();
    let coordinator = office_coordinator(&state);

    let joined = state.joined(BEDROOM_IP.parse().unwrap(), &coordinator);

    assert_eq!(joined.groups.len(), 3);
    let merged = joined
        .groups
        .iter()
        .find(|view| view.group.label() == "Bedroom + Office")
        .expect("merged group");
    assert_eq!(merged.group.visible_rooms().count(), 2);
    assert_eq!(merged.group.rooms.len(), 3, "office pair twin stays bonded");
}

#[test]
fn eager_leave_restores_standalone() {
    let state = fixture_state();
    let coordinator = office_coordinator(&state);

    let round_trip = state
        .joined(BEDROOM_IP.parse().unwrap(), &coordinator)
        .left(BEDROOM_IP.parse().unwrap());

    assert_eq!(round_trip.groups.len(), 4);
    assert!(
        round_trip
            .groups
            .iter()
            .any(|view| view.group.label() == "Bedroom" && view.group.rooms.len() == 1)
    );
    assert!(
        round_trip
            .groups
            .iter()
            .any(|view| view.group.label() == "Office" && view.group.rooms.len() == 2)
    );
}

#[test]
fn eager_join_without_target_changes_nothing() {
    let state = fixture_state();
    let unchanged = state.joined(BEDROOM_IP.parse().unwrap(), "RINCON_MISSING");
    assert_eq!(unchanged.groups.len(), 4);
    assert!(
        unchanged
            .groups
            .iter()
            .any(|view| view.group.label() == "Bedroom")
    );
}
