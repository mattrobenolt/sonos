//! Minimal UPnP/SOAP client: hand-rolled HTTP/1.1 over TCP to port 1400.
//!
//! Sonos serves plain HTTP with Content-Length bodies and honors
//! `Connection: close`, so no TLS or chunked-transfer handling is needed.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::time::Duration;

use super::{Error, Result};

/// POST one SOAP action and return the response body.
pub(crate) fn call(
    ip: Ipv4Addr,
    service: &str,
    path: &str,
    action: &str,
    body: &str,
) -> Result<String> {
    let envelope = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"urn:schemas-upnp-org:service:{service}:1\">{body}</u:{action}></s:Body></s:Envelope>"
    );
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {ip}:1400\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nSOAPACTION: \"urn:schemas-upnp-org:service:{service}:1#{action}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{envelope}",
        envelope.len()
    );

    let addr = std::net::SocketAddr::from(SocketAddrV4::new(ip, 1400));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_write_timeout(Some(Duration::from_secs(4)))?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    let body = split_body(&response)?;
    check_fault(body)
}

fn split_body(response: &str) -> Result<&str> {
    let (_, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| Error::Http("no header/body split in response".to_string()))?;
    let status = response.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") && !status.ends_with(" 200") {
        return Err(Error::Http(format!("unexpected status: {status}")));
    }
    Ok(body)
}

/// Surface UPnP faults as errors; pass the raw body through otherwise.
fn check_fault(body: &str) -> Result<String> {
    if body.contains("UPnPError") {
        let doc = roxmltree::Document::parse(body)?;
        let code = doc
            .descendants()
            .find(|n| n.tag_name().name() == "errorCode")
            .and_then(|n| n.text());
        let description = doc
            .descendants()
            .find(|n| n.tag_name().name() == "errorDescription")
            .and_then(|n| n.text());
        return Err(Error::Upnp(format!(
            "errorCode {}: {}",
            code.unwrap_or("?"),
            description.unwrap_or("?")
        )));
    }
    Ok(body.to_string())
}

/// Extract the (entity-decoded) text of the first element with this local
/// name. Sonos nests the real payload as escaped XML inside a SOAP response
/// element; roxmltree's `text()` performs the entity decoding.
pub(crate) fn text(response: &str, element: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(response)?;
    let node = doc
        .descendants()
        .find(|n| n.tag_name().name() == element)
        .ok_or_else(|| Error::Upnp(format!("missing <{element}> in response")))?;
    Ok(node.text().unwrap_or_default().to_string())
}
