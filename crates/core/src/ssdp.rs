use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::device::DeviceIdentity;

pub const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
pub const SSDP_PORT: u16 = 1900;

const SEARCH_TARGETS: &[&str] = &[
    "ssdp:all",
    "upnp:rootdevice",
    "urn:schemas-upnp-org:device:MediaRenderer:1",
    "urn:schemas-upnp-org:service:AVTransport:1",
    "urn:schemas-upnp-org:service:RenderingControl:1",
    "urn:schemas-upnp-org:service:ConnectionManager:1",
];

#[derive(Debug, Clone)]
pub struct MSearch {
    pub st: String,
    pub mx: u8,
}

pub fn parse_msearch(buf: &[u8]) -> Option<MSearch> {
    let text = std::str::from_utf8(buf).ok()?;
    let first = text.lines().next()?.to_ascii_uppercase();
    if !first.starts_with("M-SEARCH") {
        return None;
    }
    let mut st = String::new();
    let mut mx = 1u8;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_uppercase().as_str() {
                "ST" => st = v.trim().to_string(),
                "MX" => mx = v.trim().parse().unwrap_or(1).clamp(0, 5),
                _ => {}
            }
        }
    }
    if st.is_empty() {
        None
    } else {
        Some(MSearch { st, mx })
    }
}

pub fn matches_st(st: &str, uuid: &str) -> bool {
    let st_l = st.trim();
    SEARCH_TARGETS
        .iter()
        .any(|t| t.eq_ignore_ascii_case(st_l))
        || st_l.eq_ignore_ascii_case(&format!("uuid:{uuid}"))
}

pub fn bind_ssdp() -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    {
        let _ = socket.set_reuse_port(true);
    }
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    socket.bind(&SockAddr::from(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        SSDP_PORT,
    )))?;
    socket.join_multicast_v4(&MULTICAST, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    Ok(socket.into())
}

pub fn notify_alive(id: &DeviceIdentity, location: &str, st: &str) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
CACHE-CONTROL: max-age=1800\r\n\
LOCATION: {location}\r\n\
NT: {st}\r\n\
NTS: ssdp:alive\r\n\
SERVER: Linux/3.0 UPnP/1.0 castify/0.1\r\n\
USN: {usn}\r\n\
\r\n",
        location = location,
        st = st,
        usn = id.usn(st)
    )
}

pub fn notify_byebye(id: &DeviceIdentity, st: &str) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
NT: {st}\r\n\
NTS: ssdp:byebye\r\n\
USN: {usn}\r\n\
\r\n",
        st = st,
        usn = id.usn(st)
    )
}

pub fn search_response(id: &DeviceIdentity, location: &str, st: &str) -> String {
    let st_out = if st.eq_ignore_ascii_case("ssdp:all") {
        "upnp:rootdevice"
    } else {
        st
    };
    format!(
        "HTTP/1.1 200 OK\r\n\
CACHE-CONTROL: max-age=1800\r\n\
EXT:\r\n\
LOCATION: {location}\r\n\
SERVER: Linux/3.0 UPnP/1.0 castify/0.1\r\n\
ST: {st_out}\r\n\
USN: {usn}\r\n\
\r\n",
        location = location,
        st_out = st_out,
        usn = id.usn(st_out)
    )
}

pub fn send_multicast(payload: &str) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_multicast_ttl_v4(2)?;
    sock.send_to(payload.as_bytes(), SocketAddr::from((MULTICAST, SSDP_PORT)))?;
    Ok(())
}

pub fn announce(id: &DeviceIdentity, location: &str, alive: bool) {
    let sts = [
        "upnp:rootdevice",
        &format!("uuid:{}", id.uuid),
        "urn:schemas-upnp-org:device:MediaRenderer:1",
        "urn:schemas-upnp-org:service:AVTransport:1",
        "urn:schemas-upnp-org:service:RenderingControl:1",
        "urn:schemas-upnp-org:service:ConnectionManager:1",
    ];
    for st in sts {
        let msg = if alive {
            notify_alive(id, location, st)
        } else {
            notify_byebye(id, st)
        };
        let _ = send_multicast(&msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_msearch() {
        let raw = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 3\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
        let m = parse_msearch(raw).unwrap();
        assert!(matches_st(&m.st, "x"));
        assert_eq!(m.mx, 3);
    }
}
