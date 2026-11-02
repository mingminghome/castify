use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::http::{http_get_text, HttpUrl};
use crate::ssdp::{MULTICAST, SSDP_PORT};
use crate::xml::tag_text;

const MEDIA_RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRenderer {
    pub name: String,
    pub uuid: String,
    pub location: String,
    pub control_url: String,
    /// True when the device description says `modelName` is `castify`.
    pub castify: bool,
}

impl DiscoveredRenderer {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"name\":\"{}\",\"uuid\":\"{}\",\"location\":\"{}\",\"controlUrl\":\"{}\",\"castify\":{}}}",
            json_escape(&self.name),
            json_escape(&self.uuid),
            json_escape(&self.location),
            json_escape(&self.control_url),
            if self.castify { "true" } else { "false" }
        )
    }
}

pub fn renderers_to_json(list: &[DiscoveredRenderer]) -> String {
    let body = list
        .iter()
        .map(DiscoveredRenderer::to_json)
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

pub fn search_renderers(timeout: Duration) -> std::io::Result<Vec<DiscoveredRenderer>> {
    let locations = msearch_locations(timeout)?;
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for loc in locations {
        match fetch_renderer(&loc) {
            Ok(dev) => {
                if seen.insert(dev.control_url.clone()) {
                    out.push(dev);
                }
            }
            Err(e) => log::info!("skip renderer {loc}: {e}"),
        }
    }
    Ok(out)
}

pub fn fetch_renderer(location: &str) -> std::io::Result<DiscoveredRenderer> {
    let xml = http_get_text(location)?;
    parse_device_xml(&xml, location).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("no AVTransport in {location}"),
        )
    })
}

pub fn parse_device_xml(xml: &str, location: &str) -> Option<DiscoveredRenderer> {
    let base = HttpUrl::parse(location)?;
    let name = tag_text(xml, "friendlyName").unwrap_or_else(|| "MediaRenderer".into());
    let udn = tag_text(xml, "UDN").unwrap_or_default();
    let uuid = udn.trim().trim_start_matches("uuid:").trim().to_string();
    let control = av_transport_control(xml)?;
    let control_url = base.join(&control);
    let model = tag_text(xml, "modelName").unwrap_or_default();
    Some(DiscoveredRenderer {
        name,
        uuid,
        location: location.to_string(),
        control_url,
        castify: model.eq_ignore_ascii_case("castify"),
    })
}

fn av_transport_control(xml: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<service") {
        let abs = from + rel;
        let rest = &xml[abs..];
        let rest_l = &lower[abs..];
        let end = rest_l.find("</service>").unwrap_or(rest.len());
        let block = &rest[..end];
        let block_l = block.to_ascii_lowercase();
        if block_l.contains("avtransport") {
            return tag_text(block, "controlURL");
        }
        from = abs + 8;
    }
    None
}

fn msearch_locations(timeout: Duration) -> std::io::Result<Vec<String>> {
    let payload = format!(
        "M-SEARCH * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
MAN: \"ssdp:discover\"\r\n\
MX: {mx}\r\n\
ST: {st}\r\n\
\r\n",
        mx = timeout.as_secs().clamp(1, 5),
        st = MEDIA_RENDERER
    );
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_millis(300)))?;
    sock.set_multicast_ttl_v4(2)?;
    sock.send_to(payload.as_bytes(), SocketAddr::from((MULTICAST, SSDP_PORT)))?;
    let mut locations = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(loc) = header_value(&buf[..n], "LOCATION") {
                    if !locations.iter().any(|s: &String| s.eq_ignore_ascii_case(&loc)) {
                        locations.push(loc);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(locations)
}

pub fn header_value(packet: &[u8], name: &str) -> Option<String> {
    let text = std::str::from_utf8(packet).ok()?;
    let want = name.to_ascii_uppercase();
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(&want) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_xml() {
        let xml = r#"<?xml version="1.0"?>
<root>
  <device>
    <friendlyName>TV, Receive</friendlyName>
    <UDN>uuid:6ba7b810-9dad-11d1-80b4-abc</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <controlURL>/avtransport/control</controlURL>
      </service>
    </serviceList>
  </device>
</root>"#;
        let d = parse_device_xml(xml, "http://192.168.3.177:9000/device.xml").unwrap();
        assert_eq!(d.name, "TV, Receive");
        assert_eq!(d.uuid, "6ba7b810-9dad-11d1-80b4-abc");
        assert_eq!(d.control_url, "http://192.168.3.177:9000/avtransport/control");
    }

    #[test]
    fn json_list_escapes_quotes() {
        let d = DiscoveredRenderer {
            name: r#"TV "Living""#.into(),
            uuid: "u".into(),
            location: "http://10.0.0.2/d".into(),
            control_url: "http://10.0.0.2/c".into(),
            castify: false,
        };
        let j = renderers_to_json(&[d]);
        assert!(j.contains(r#"\"Living\""#));
        assert!(j.starts_with('['));
    }

    #[test]
    fn ssdp_header_is_case_insensitive() {
        let pkt = b"HTTP/1.1 200 OK\r\nLocation: http://10.0.0.5/device.xml\r\nST: urn:x\r\n\r\n";
        assert_eq!(
            header_value(pkt, "LOCATION").as_deref(),
            Some("http://10.0.0.5/device.xml")
        );
    }
}
