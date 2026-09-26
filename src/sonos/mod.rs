//! Sonos LAN protocol: SSDP discovery, SOAP transport, topology, group state.
//!
//! Verified against live speakers (fw 97.1, swGen 2); see tests/lan.rs for
//! the live-action counterparts and docs/decisions.md for lane context.

pub mod control;
pub mod soap;
pub mod ssdp;
pub mod topology;

pub use ssdp::discover;

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
    /// Visible rooms, alphabetical by name — canonical for labels and
    /// chips so cards never jump when the topology XML shuffles members.
    pub fn visible_rooms(&self) -> impl Iterator<Item = &Room> {
        let mut rooms: Vec<&Room> = self.rooms.iter().filter(|r| !r.invisible).collect();
        rooms.sort_by(|a, b| a.name.cmp(&b.name));
        rooms.into_iter()
    }

    /// Display label: visible room names joined with " + ".
    pub fn label(&self) -> String {
        self.visible_rooms().map(|r| r.name.as_str()).collect::<Vec<_>>().join(" + ")
    }

    pub fn coordinator_ip(&self) -> Option<Ipv4Addr> {
        self.rooms.iter().find(|r| r.uuid == self.coordinator_uuid).map(|r| r.ip)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub rel_time: Option<String>,
    pub track_duration: Option<String>,
}

/// A group plus its live state (volume, transport, now-playing).
#[derive(Debug, Clone)]
pub struct GroupView {
    pub group: Group,
    pub volume: Option<u8>,
    pub transport_state: Option<String>,
    pub now_playing: Option<NowPlaying>,
}

#[derive(Debug, Clone, Default)]
pub struct SystemState {
    pub groups: Vec<GroupView>,
}

/// Assemble the household state: topology from the first speaker that
/// answers, then volume/transport/now-playing per group in parallel.
/// Always returns a state (empty when nothing answers) so the UI can
/// degrade to "discovering..." instead of erroring out.
pub fn snapshot(ips: &[Ipv4Addr]) -> SystemState {
    for &ip in ips {
        if let Ok(groups) = topology::zone_groups(ip) {
            let mut groups: Vec<GroupView> = std::thread::scope(|scope| {
                let handles: Vec<_> = groups
                    .into_iter()
                    .map(|group| {
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
                            GroupView { group, volume, transport_state, now_playing }
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
