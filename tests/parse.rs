//! Pure parser tests against live-captured fixtures.

use sonos::sonos::{control, topology};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap()
}

#[test]
fn parses_live_topology() {
    let groups = topology::parse_groups(&fixture("zonegroupstate.xml")).unwrap();
    assert_eq!(groups.len(), 4);

    let office = groups.iter().find(|g| g.label() == "Office").unwrap();
    assert_eq!(office.rooms.len(), 2, "stereo pair: one visible, one invisible twin");
    assert_eq!(office.rooms.iter().filter(|r| r.invisible).count(), 1);
    assert_eq!(office.coordinator_ip().unwrap().to_string(), "192.168.2.91");

    for group in &groups {
        assert!(
            group.coordinator_ip().is_some(),
            "no coordinator room for {}",
            group.label()
        );
    }
}

#[test]
fn parses_group_volume() {
    let volume = control::parse_group_volume(&fixture("getgroupvolume_response.xml")).unwrap();
    assert!(volume <= 100, "volume out of range: {volume}");
}

#[test]
fn parses_position_info() {
    let np = control::parse_position(&fixture("getpositioninfo_response.xml")).unwrap();
    // Captured while Plex played to the Family Room: full DIDL metadata.
    assert!(np.rel_time.is_some());
    assert_eq!(np.title.as_deref(), Some("Eraser"));
    assert_eq!(np.artist.as_deref(), Some("Coheed and Cambria"));
}

#[test]
fn parses_didl_metadata() {
    let didl = control::parse_didl(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\">\
         <item><dc:title xmlns:dc=\"http://purl.org/dc/elements/1.1/\">Fake Song</dc:title>\
         <dc:creator xmlns:dc=\"http://purl.org/dc/elements/1.1/\">Fake Artist</dc:creator>\
         </item></DIDL-Lite>",
    )
    .unwrap();
    assert_eq!(didl.title, "Fake Song");
    assert_eq!(didl.artist.as_deref(), Some("Fake Artist"));
}

#[test]
fn rejects_not_implemented_metadata() {
    assert!(control::parse_didl("NOT_IMPLEMENTED").is_none());
    assert!(control::parse_didl("").is_none());
}

#[test]
fn treats_not_implemented_times_as_absent() {
    let response = concat!(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>",
        "<u:GetPositionInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">",
        "<Track>1</Track>",
        "<TrackDuration>NOT_IMPLEMENTED</TrackDuration>",
        "<TrackMetaData>NOT_IMPLEMENTED</TrackMetaData>",
        "<RelTime>NOT_IMPLEMENTED</RelTime>",
        "</u:GetPositionInfoResponse></s:Body></s:Envelope>",
    );
    let np = control::parse_position(response).unwrap();
    assert!(np.rel_time.is_none());
    assert!(np.track_duration.is_none());
    assert!(np.title.is_none());
}
