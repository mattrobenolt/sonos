//! Sonos LAN protocol: SSDP discovery, SOAP transport, topology, group state.
//!
//! Verified against live speakers (fw 97.1, swGen 2); see tests/lan.rs for
//! the live-action counterparts and docs/decisions.md for lane context.

pub mod control;
pub mod soap;
pub mod ssdp;
pub mod topology;

pub use ssdp::discover;

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Http(String),
    Xml(roxmltree::Error),
    Upnp(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Http(msg) => write!(f, "http: {msg}"),
            Error::Xml(e) => write!(f, "xml: {e}"),
            Error::Upnp(msg) => write!(f, "upnp: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<roxmltree::Error> for Error {
    fn from(e: roxmltree::Error) -> Self {
        Error::Xml(e)
    }
}

/// One physical speaker. Bonded-pair members (stereo pairs) appear as one
/// visible room plus `invisible` twins; groups act on visible rooms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub uuid: String,
    pub name: String,
    pub ip: Ipv4Addr,
    pub invisible: bool,
}

/// A playback group: a coordinator plus its member rooms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub coordinator_uuid: String,
    pub rooms: Vec<Room>,
}

impl Group {
    /// Visible rooms, alphabetical by name — canonical for labels and chips
    /// so cards never jump when the topology XML shuffles members.
    pub fn visible_rooms(&self) -> impl Iterator<Item = &Room> {
        let mut rooms: Vec<&Room> = self.rooms.iter().filter(|r| !r.invisible).collect();
        rooms.sort_by(|a, b| a.name.cmp(&b.name));
        rooms.into_iter()
    }

    /// Display label: visible room names joined with " + ".
    pub fn label(&self) -> String {
        self.visible_rooms()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    }

    pub fn coordinator_ip(&self) -> Option<Ipv4Addr> {
        self.rooms
            .iter()
            .find(|r| r.uuid == self.coordinator_uuid)
            .map(|r| r.ip)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_art_uri: Option<String>,
    pub rel_time: Option<String>,
    pub track_duration: Option<String>,
}

/// A group plus its live state.
#[derive(Debug, Clone)]
pub struct GroupView {
    pub group: Group,
    pub volume: Option<u8>,
    pub transport_state: Option<String>,
    pub now_playing: Option<NowPlaying>,
    /// Per-room volumes keyed by room uuid; missing = unknown.
    pub room_volumes: HashMap<String, u8>,
}

#[derive(Debug, Clone, Default)]
pub struct SystemState {
    pub groups: Vec<GroupView>,
}

impl SystemState {
    /// A room plus its bonded hardware: within the room's group, all rooms
    /// sharing the room's name travel together (stereo pairs and other
    /// bonded sets share a ZoneName; grouped rooms keep distinct names).
    fn room_family(&self, room_ip: Ipv4Addr) -> Option<(usize, Vec<Room>)> {
        for (index, view) in self.groups.iter().enumerate() {
            if let Some(room) = view.group.rooms.iter().find(|r| r.ip == room_ip) {
                let name = room.name.clone();
                let family: Vec<Room> = view
                    .group
                    .rooms
                    .iter()
                    .filter(|r| r.name == name)
                    .cloned()
                    .collect();
                return Some((index, family));
            }
        }
        None
    }

    /// Eager-UI fold of a join: move the room and its bonded hardware into
    /// the target group locally so the UI reflects it immediately; the poll
    /// corrects within an interval. Pure — unit tested against the topology
    /// fixture.
    pub fn joined(mut self, room_ip: Ipv4Addr, coordinator_uuid: &str) -> Self {
        let Some((source, family)) = self.room_family(room_ip) else {
            return self;
        };
        let target = self
            .groups
            .iter()
            .position(|view| view.group.coordinator_uuid == coordinator_uuid);
        let Some(target) = target else {
            return self;
        };
        if source == target {
            return self;
        }
        let family_ips: Vec<Ipv4Addr> = family.iter().map(|r| r.ip).collect();
        let family_uuids: Vec<String> = family.iter().map(|r| r.uuid.clone()).collect();
        // Carry the family's per-room volumes over to the target.
        let mut carried: HashMap<String, u8> = HashMap::new();
        if let Some(view) = self.groups.get(source) {
            for room in &family {
                if let Some(volume) = view.room_volumes.get(&room.uuid) {
                    carried.insert(room.uuid.clone(), *volume);
                }
            }
        }
        // Attach to the target FIRST: dropping the emptied source group
        // below shifts indices, and a stale `target` index would attach the
        // family to the wrong group (caught by the fixture unit test).
        if let Some(view) = self.groups.get_mut(target) {
            view.group.rooms.extend(family);
            view.room_volumes.extend(carried);
        }
        if let Some(view) = self.groups.get_mut(source) {
            view.group.rooms.retain(|r| !family_ips.contains(&r.ip));
            view.room_volumes
                .retain(|uuid, _| !family_uuids.contains(uuid));
        }
        self.groups.retain(|view| !view.group.rooms.is_empty());
        self.resort();
        self
    }

    /// Eager-UI fold of a leave: the room and its bonded hardware become
    /// their own standalone group; a group that lost its coordinator
    /// re-elects its first room.
    pub fn left(mut self, room_ip: Ipv4Addr) -> Self {
        let Some((source, family)) = self.room_family(room_ip) else {
            return self;
        };
        let family_ips: Vec<Ipv4Addr> = family.iter().map(|r| r.ip).collect();
        let family_uuids: Vec<String> = family.iter().map(|r| r.uuid.clone()).collect();
        // A standalone group's group volume is the visible room's volume.
        let visible = family.iter().find(|r| !r.invisible).cloned();
        let room_volume = self
            .groups
            .get(source)
            .and_then(|view| {
                visible
                    .as_ref()
                    .and_then(|r| view.room_volumes.get(&r.uuid))
            })
            .copied();
        let mut room_volumes = HashMap::new();
        if let Some(volume) = room_volume {
            if let Some(room) = &visible {
                room_volumes.insert(room.uuid.clone(), volume);
            }
        }
        if let Some(view) = self.groups.get_mut(source) {
            view.group.rooms.retain(|r| !family_ips.contains(&r.ip));
            view.room_volumes
                .retain(|uuid, _| !family_uuids.contains(uuid));
        }
        self.groups.retain(|view| !view.group.rooms.is_empty());
        for view in self.groups.iter_mut() {
            if !view
                .group
                .rooms
                .iter()
                .any(|r| r.uuid == view.group.coordinator_uuid)
            {
                if let Some(first) = view.group.rooms.first() {
                    view.group.coordinator_uuid = first.uuid.clone();
                }
            }
        }
        let coordinator_uuid = visible
            .as_ref()
            .map(|r| r.uuid.clone())
            .unwrap_or_else(|| family[0].uuid.clone());
        self.groups.push(GroupView {
            group: Group {
                coordinator_uuid,
                rooms: family,
            },
            volume: room_volume,
            transport_state: None,
            now_playing: None,
            room_volumes,
        });
        self.resort();
        self
    }

    fn resort(&mut self) {
        self.groups
            .sort_by(|a, b| a.group.label().cmp(&b.group.label()));
    }
}

/// Assemble the household state: topology from the first speaker that
/// answers, then per-group state and per-room volumes in parallel. Always
/// returns a state (empty when nothing answers) so the UI can degrade to
/// "discovering..." instead of erroring out.
pub fn snapshot(ips: &[Ipv4Addr]) -> SystemState {
    for &ip in ips {
        if let Ok(groups) = topology::zone_groups(ip) {
            let mut groups: Vec<GroupView> = std::thread::scope(|scope| {
                let handles: Vec<_> = groups
                    .into_iter()
                    .map(|group| {
                        let room_specs: Vec<(String, Ipv4Addr)> = group
                            .visible_rooms()
                            .map(|room| (room.uuid.clone(), room.ip))
                            .collect();
                        let room_handles: Vec<_> = room_specs
                            .into_iter()
                            .map(|(uuid, ip)| {
                                scope.spawn(move || (uuid, control::room_volume(ip).ok()))
                            })
                            .collect();
                        scope.spawn(move || {
                            let coordinator_ip = group.coordinator_ip();
                            let volume =
                                coordinator_ip.and_then(|ip| control::group_volume(ip).ok());
                            let mut transport_state = None;
                            let mut now_playing = None;
                            if let Some(ip) = coordinator_ip {
                                transport_state = control::transport_state(ip).ok();
                                if let Ok(np) = control::position_info(ip) {
                                    now_playing = Some(np);
                                }
                            }
                            let mut room_volumes = HashMap::new();
                            for handle in room_handles {
                                let (uuid, volume) = handle.join().unwrap();
                                if let Some(volume) = volume {
                                    room_volumes.insert(uuid, volume);
                                }
                            }
                            GroupView {
                                group,
                                volume,
                                transport_state,
                                now_playing,
                                room_volumes,
                            }
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            // Stable display order: the topology XML order shuffles on
            // group changes.
            groups.sort_by(|a, b| a.group.label().cmp(&b.group.label()));
            return SystemState { groups };
        }
    }
    SystemState::default()
}
