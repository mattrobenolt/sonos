//! Sonos LAN control protocol (UPnP/SOAP over port 1400).
//!
//! GPUI-free by design: this crate must build and test headless so the UI
//! layer stays replaceable (see docs/decisions.md).

pub mod sonos;

pub use sonos::{
    Error, Group, GroupView, NowPlaying, Result, Room, SystemState, control, discover, snapshot,
};
