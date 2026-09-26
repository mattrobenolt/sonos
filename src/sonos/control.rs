//! Group volume, now-playing, and grouping actions.
//!
//! All group actions go to the group coordinator (per the service docs,
//! non-coordinators answer error 800/701).

use std::net::Ipv4Addr;

use super::{Error, NowPlaying, Result, soap};

const AV_TRANSPORT: &str = "AVTransport";
const GROUP_RENDERING: &str = "GroupRenderingControl";
const AV_PATH: &str = "/MediaRenderer/AVTransport/Control";
const GROUP_PATH: &str = "/MediaRenderer/GroupRenderingControl/Control";
const RENDERING: &str = "RenderingControl";
const RENDERING_PATH: &str = "/MediaRenderer/RenderingControl/Control";
const INSTANCE: &str = "<InstanceID>0</InstanceID>";
const CHANNEL: &str = "<InstanceID>0</InstanceID><Channel>Master</Channel>";

pub fn group_volume(ip: Ipv4Addr) -> Result<u8> {
    let response = soap::call(ip, GROUP_RENDERING, GROUP_PATH, "GetGroupVolume", INSTANCE)?;
    parse_volume(&response)
}

/// Per-speaker volume (RenderingControl, Master channel). A bonded stereo
/// pair is one visible room with one volume.
pub fn room_volume(ip: Ipv4Addr) -> Result<u8> {
    let response = soap::call(ip, RENDERING, RENDERING_PATH, "GetVolume", CHANNEL)?;
    parse_volume(&response)
}

pub fn set_room_volume(ip: Ipv4Addr, volume: u8) -> Result<()> {
    soap::call(
        ip,
        RENDERING,
        RENDERING_PATH,
        "SetVolume",
        &format!("{CHANNEL}<DesiredVolume>{volume}</DesiredVolume>"),
    )?;
    Ok(())
}

/// Pure: parse a GetVolume/GetGroupVolume SOAP response (both return
/// CurrentVolume).
pub fn parse_volume(response: &str) -> Result<u8> {
    soap::text(response, "CurrentVolume")?
        .trim()
        .parse()
        .map_err(|e| Error::Upnp(format!("bad CurrentVolume: {e}")))
}

/// Set absolute group volume (0-100). Members scale proportionally to their
/// last snapshot ratios.
pub fn set_group_volume(ip: Ipv4Addr, volume: u8) -> Result<()> {
    soap::call(
        ip,
        GROUP_RENDERING,
        GROUP_PATH,
        "SetGroupVolume",
        &format!("{INSTANCE}<DesiredVolume>{volume}</DesiredVolume>"),
    )?;
    Ok(())
}

/// Nudge group volume; returns the resulting volume.
pub fn relative_group_volume(ip: Ipv4Addr, adjustment: i32) -> Result<u8> {
    let response = soap::call(
        ip,
        GROUP_RENDERING,
        GROUP_PATH,
        "SetRelativeGroupVolume",
        &format!("{INSTANCE}<Adjustment>{adjustment}</Adjustment>"),
    )?;
    soap::text(&response, "NewVolume")?
        .trim()
        .parse()
        .map_err(|e| Error::Upnp(format!("bad NewVolume: {e}")))
}

pub fn transport_state(ip: Ipv4Addr) -> Result<String> {
    let response = soap::call(ip, AV_TRANSPORT, AV_PATH, "GetTransportInfo", INSTANCE)?;
    soap::text(&response, "CurrentTransportState")
}

pub fn position_info(ip: Ipv4Addr) -> Result<NowPlaying> {
    let response = soap::call(ip, AV_TRANSPORT, AV_PATH, "GetPositionInfo", INSTANCE)?;
    parse_position(&response)
}

/// Pure: parse a GetPositionInfo SOAP response. Title/artist come from the
/// embedded DIDL-Lite metadata; line-in/TV sources carry none, so times are
/// the only signal there.
pub fn parse_position(response: &str) -> Result<NowPlaying> {
    // Line-in/TV sources report NOT_IMPLEMENTED for times; treat as absent.
    let rel_time = clean_time(soap::text(response, "RelTime").ok());
    let track_duration = clean_time(soap::text(response, "TrackDuration").ok());
    let mut now_playing = NowPlaying {
        title: None,
        artist: None,
        album_art_uri: None,
        rel_time,
        track_duration,
    };
    if let Ok(meta) = soap::text(response, "TrackMetaData") {
        if let Some(didl) = parse_didl(&meta) {
            now_playing.title = Some(didl.title);
            now_playing.artist = didl.artist;
            now_playing.album_art_uri = didl.album_art_uri;
        }
    }
    Ok(now_playing)
}

fn clean_time(value: Option<String>) -> Option<String> {
    value.filter(|t| !t.is_empty() && t != "NOT_IMPLEMENTED")
}

pub struct Didl {
    pub title: String,
    pub artist: Option<String>,
    pub album_art_uri: Option<String>,
}

/// Extract title/creator/album art from a DIDL-Lite metadata blob. Returns
/// None for "NOT_IMPLEMENTED", empty, or otherwise unparseable metadata.
pub fn parse_didl(meta: &str) -> Option<Didl> {
    if !meta.contains("<DIDL") {
        return None;
    }
    let doc = roxmltree::Document::parse(meta).ok()?;
    let title = doc
        .descendants()
        .find(|n| n.tag_name().name() == "title")?
        .text()?
        .to_string();
    let artist = doc
        .descendants()
        .find(|n| n.tag_name().name() == "creator")
        .and_then(|n| n.text())
        .map(|t| t.to_string());
    let album_art_uri = doc
        .descendants()
        .find(|n| n.tag_name().name() == "albumArtURI")
        .and_then(|n| n.text())
        .map(|t| t.to_string());
    Some(Didl {
        title,
        artist,
        album_art_uri,
    })
}

/// Join this player to the group of `coordinator_uuid`
/// (SetAVTransportURI with an x-rincon URI, per the service docs).
pub fn join(ip: Ipv4Addr, coordinator_uuid: &str) -> Result<()> {
    soap::call(
        ip,
        AV_TRANSPORT,
        AV_PATH,
        "SetAVTransportURI",
        &format!(
            "{INSTANCE}<CurrentURI>x-rincon:{coordinator_uuid}</CurrentURI><CurrentURIMetaData></CurrentURIMetaData>"
        ),
    )?;
    Ok(())
}

/// Break this player out of its group into a standalone group.
pub fn leave(ip: Ipv4Addr) -> Result<()> {
    soap::call(
        ip,
        AV_TRANSPORT,
        AV_PATH,
        "BecomeCoordinatorOfStandaloneGroup",
        INSTANCE,
    )?;
    Ok(())
}
