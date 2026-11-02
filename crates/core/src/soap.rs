use crate::didl::DidlItem;
use crate::xml::tag_text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoapAction {
    SetAvTransportUri {
        uri: String,
        metadata: String,
    },
    Play,
    Pause,
    Stop,
    Seek,
    GetTransportInfo,
    GetMediaInfo,
    GetPositionInfo,
    GetTransportSettings,
    GetDeviceCapabilities,
    GetCurrentTransportActions,
    GetProtocolInfo,
    GetCurrentConnectionIds,
    GetCurrentConnectionInfo,
    GetVolume,
    SetVolume {
        percent: u8,
    },
    GetMute,
    SetMute {
        muted: bool,
    },
    Unknown(String),
}

impl SoapAction {
    pub fn parse(soap_action_header: &str, body: &str) -> Self {
        let name = action_name(soap_action_header, body);
        match name.to_ascii_lowercase().as_str() {
            "setavtransporturi" => {
                let uri = tag_text(body, "CurrentURI").unwrap_or_default();
                let metadata = tag_text(body, "CurrentURIMetaData").unwrap_or_default();
                Self::SetAvTransportUri { uri, metadata }
            }
            "play" => Self::Play,
            "pause" => Self::Pause,
            "stop" => Self::Stop,
            "seek" => Self::Seek,
            "gettransportinfo" => Self::GetTransportInfo,
            "getmediainfo" => Self::GetMediaInfo,
            "getpositioninfo" => Self::GetPositionInfo,
            "gettransportsettings" => Self::GetTransportSettings,
            "getdevicecapabilities" => Self::GetDeviceCapabilities,
            "getcurrenttransportactions" => Self::GetCurrentTransportActions,
            "getprotocolinfo" => Self::GetProtocolInfo,
            "getcurrentconnectionids" => Self::GetCurrentConnectionIds,
            "getcurrentconnectioninfo" => Self::GetCurrentConnectionInfo,
            "getvolume" => Self::GetVolume,
            "setvolume" => {
                let percent = tag_text(body, "DesiredVolume")
                    .and_then(|s| s.parse::<u16>().ok())
                    .map(|v| v.min(100) as u8)
                    .unwrap_or(50);
                Self::SetVolume { percent }
            }
            "getmute" => Self::GetMute,
            "setmute" => {
                let raw = tag_text(body, "DesiredMute").unwrap_or_default();
                let muted = raw == "1" || raw.eq_ignore_ascii_case("true");
                Self::SetMute { muted }
            }
            other => Self::Unknown(other.to_string()),
        }
    }
}

fn action_name(header: &str, body: &str) -> String {
    let h = header.trim().trim_matches('"');
    if let Some(idx) = h.rfind('#') {
        let name = h[idx + 1..].trim().trim_matches('"');
        if !name.is_empty() {
            return name.to_string();
        }
    }
    // <u:SetAVTransportURI xmlns:u="...">
    if let Some(start) = body.find("<u:") {
        let rest = &body[start + 3..];
        let end = rest
            .find(|c: char| c == ' ' || c == '>' || c == '/')
            .unwrap_or(rest.len());
        return rest[..end].to_string();
    }
    String::new()
}

pub fn envelope(service: &str, action: &str, inner: &str) -> String {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" ",
            "s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">",
            "<s:Body><u:{action} xmlns:u=\"{service}\">{inner}</u:{action}></s:Body>",
            "</s:Envelope>"
        ),
        action = action,
        service = service,
        inner = inner
    )
}

pub fn didl_from_set_uri(metadata: &str) -> DidlItem {
    DidlItem::parse(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_set_uri_from_header() {
        let body = r#"<s:Body><u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID><CurrentURI>http://10.0.0.8/a.jpg</CurrentURI><CurrentURIMetaData></CurrentURIMetaData></u:SetAVTransportURI></s:Body>"#;
        match SoapAction::parse(
            r#""urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI""#,
            body,
        ) {
            SoapAction::SetAvTransportUri { uri, .. } => {
                assert_eq!(uri, "http://10.0.0.8/a.jpg");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_set_volume() {
        let body = "<DesiredVolume>80</DesiredVolume>";
        match SoapAction::parse(
            "urn:schemas-upnp-org:service:RenderingControl:1#SetVolume",
            body,
        ) {
            SoapAction::SetVolume { percent } => assert_eq!(percent, 80),
            other => panic!("unexpected {other:?}"),
        }
    }
}
