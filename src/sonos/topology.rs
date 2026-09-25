//! ZoneGroupTopology: the group/room map of the whole household.

use std::net::Ipv4Addr;

use super::{soap, Group, Result, Room};

/// Fetch and parse the household's zone groups from any live speaker.
pub fn zone_groups(ip: Ipv4Addr) -> Result<Vec<Group>> {
    let response = soap::call(
        ip,
        "ZoneGroupTopology",
        "/ZoneGroupTopology/Control",
        "GetZoneGroupState",
        "",
    )?;
    // The ZoneGroupState element holds the topology as escaped XML text;
    // soap::text() hands it back entity-decoded.
    let state = soap::text(&response, "ZoneGroupState")?;
    parse_groups(&state)
}

/// Parse a (decoded) ZoneGroupState document into groups. Pure: the fixture
/// in tests/fixtures/ is a live capture.
pub fn parse_groups(state: &str) -> Result<Vec<Group>> {
    let doc = roxmltree::Document::parse(state)?;
    let mut groups = Vec::new();
    for node in doc.descendants().filter(|n| n.tag_name().name() == "ZoneGroup") {
        let Some(coordinator_uuid) = node.attribute("Coordinator") else {
            continue;
        };
        let mut rooms = Vec::new();
        for member in node.children().filter(|n| n.tag_name().name() == "ZoneGroupMember") {
            let (Some(uuid), Some(name), Some(ip)) = (
                member.attribute("UUID"),
                member.attribute("ZoneName"),
                member.attribute("Location").and_then(ip_from_location),
            ) else {
                continue;
            };
            rooms.push(Room {
                uuid: uuid.to_string(),
                name: name.to_string(),
                ip,
                invisible: member.attribute("Invisible").map(|v| v == "1").unwrap_or(false),
            });
        }
        groups.push(Group {
            coordinator_uuid: coordinator_uuid.to_string(),
            rooms,
        });
    }
    Ok(groups)
}

fn ip_from_location(location: &str) -> Option<Ipv4Addr> {
    let rest = location.strip_prefix("http://")?;
    rest.split(':').next()?.parse().ok()
}
