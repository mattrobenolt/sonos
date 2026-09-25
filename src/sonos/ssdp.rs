//! SSDP discovery of Sonos ZonePlayers on the LAN.

use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use super::Result;

const MSEARCH: &str = concat!(
    "M-SEARCH * HTTP/1.1\r\n",
    "HOST: 239.255.255.250:1900\r\n",
    "MAN: \"ssdp:discover\"\r\n",
    "MX: 3\r\n",
    "ST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n",
    "\r\n",
);

/// Multicast an M-SEARCH and collect ZonePlayer addresses until `timeout`
/// elapses. Speakers reply from their own IPs, so the source address is
/// the speaker.
pub fn discover(timeout: Duration) -> Result<Vec<Ipv4Addr>> {
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    socket.send_to(MSEARCH.as_bytes(), ("239.255.255.250", 1900))?;

    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    let mut found = std::collections::BTreeSet::new();
    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((_, from)) => {
                if let std::net::IpAddr::V4(ip) = from.ip() {
                    found.insert(ip);
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(found.into_iter().collect())
}
