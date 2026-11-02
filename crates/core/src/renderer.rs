use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::device::{
    av_transport_scpd, connection_manager_scpd, device_description, rendering_control_scpd,
    DeviceIdentity, SINK_PROTOCOL_INFO,
};
use crate::http::{read_request, write_response, ConnLimit, HttpRequest};
use crate::media::{ReceivedItem, ReceiverEvent};
use crate::net::local_ipv4;
use crate::soap::{envelope, SoapAction};
use crate::ssdp::{announce, bind_ssdp, matches_st, parse_msearch, search_response};
use crate::xml::escape;

#[derive(Debug, Clone)]
pub struct RendererConfig {
    pub identity: DeviceIdentity,
    pub bind_ip: Option<String>,
    pub http_port: u16,
}

impl RendererConfig {
    pub fn new(friendly_name: impl Into<String>, uuid: impl Into<String>) -> Self {
        Self {
            identity: DeviceIdentity::new(friendly_name, uuid),
            bind_ip: None,
            http_port: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct Transport {
    uri: String,
    metadata: String,
    title: Option<String>,
    mime: Option<String>,
    host_uri: String,
    state: &'static str,
    volume: u8,
    muted: bool,
}

impl Default for Transport {
    fn default() -> Self {
        Self {
            uri: String::new(),
            metadata: String::new(),
            title: None,
            mime: None,
            host_uri: String::new(),
            state: "NO_MEDIA_PRESENT",
            volume: 50,
            muted: false,
        }
    }
}

/// UPnP MediaRenderer. Runs SSDP + HTTP/SOAP on background threads.
pub struct MediaRenderer {
    stop: Arc<AtomicBool>,
    location: Arc<Mutex<String>>,
}

impl MediaRenderer {
    pub fn start<F>(config: RendererConfig, on_event: F) -> std::io::Result<Self>
    where
        F: FnMut(ReceiverEvent) + Send + 'static,
    {
        let bind_ip: Option<std::net::Ipv4Addr> = config
            .bind_ip
            .as_deref()
            .and_then(|s| s.parse().ok());
        let listen_ip = bind_ip.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
        let listener = TcpListener::bind((listen_ip, config.http_port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let ip = bind_ip.or_else(local_ipv4).ok_or_else(|| {
            std::io::Error::new(ErrorKind::AddrNotAvailable, "no local IPv4 address")
        })?;
        let location = format!("http://{ip}:{port}/device.xml");
        let identity = config.identity.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let transport = Arc::new(Mutex::new(Transport::default()));
        let event_tx: Arc<Mutex<Box<dyn FnMut(ReceiverEvent) + Send>>> =
            Arc::new(Mutex::new(Box::new(on_event)));

        emit(
            &event_tx,
            ReceiverEvent::Ready {
                location: location.clone(),
                friendly_name: identity.friendly_name.clone(),
            },
        );

        announce(&identity, &location, true);

        let stop_http = stop.clone();
        let identity_http = identity.clone();
        let location_http = location.clone();
        let transport_http = transport.clone();
        let event_http = event_tx.clone();
        thread::Builder::new()
            .name("castify-http".into())
            .spawn(move || {
                http_loop(
                    listener,
                    stop_http,
                    identity_http,
                    location_http,
                    transport_http,
                    event_http,
                );
            })?;

        let stop_ssdp = stop.clone();
        let identity_ssdp = identity.clone();
        let location_ssdp = location.clone();
        thread::Builder::new()
            .name("castify-ssdp".into())
            .spawn(move || ssdp_loop(stop_ssdp, identity_ssdp, location_ssdp))?;

        let stop_alive = stop.clone();
        let identity_alive = identity.clone();
        let location_alive = location.clone();
        thread::Builder::new()
            .name("castify-alive".into())
            .spawn(move || {
                while !stop_alive.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(30));
                    if stop_alive.load(Ordering::Relaxed) {
                        break;
                    }
                    announce(&identity_alive, &location_alive, true);
                }
            })?;

        Ok(Self {
            stop,
            location: Arc::new(Mutex::new(location)),
        })
    }

    pub fn location(&self) -> String {
        self.location.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        crate::guard::stop_proxy();
    }
}

impl Drop for MediaRenderer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn emit(tx: &Arc<Mutex<Box<dyn FnMut(ReceiverEvent) + Send>>>, event: ReceiverEvent) {
    if let Ok(mut cb) = tx.lock() {
        cb(event);
    }
}

fn http_loop(
    listener: TcpListener,
    stop: Arc<AtomicBool>,
    identity: DeviceIdentity,
    location: String,
    transport: Arc<Mutex<Transport>>,
    events: Arc<Mutex<Box<dyn FnMut(ReceiverEvent) + Send>>>,
) {
    let limit = ConnLimit::new(32);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = limit.try_acquire() else {
                    drop(stream);
                    continue;
                };
                let identity = identity.clone();
                let location = location.clone();
                let transport = transport.clone();
                let events = events.clone();
                thread::spawn(move || {
                    let _permit = permit;
                    let _ = handle_client(stream, &identity, &location, &transport, &events);
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                log::warn!("dlna http accept: {e}");
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn ssdp_loop(stop: Arc<AtomicBool>, identity: DeviceIdentity, location: String) {
    let sock = match bind_ssdp() {
        Ok(s) => s,
        Err(e) => {
            log::warn!("SSDP bind 1900 failed ({e}); continuing with NOTIFY only");
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(400));
            }
            announce(&identity, &location, false);
            return;
        }
    };
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Some(search) = parse_msearch(&buf[..n]) {
                    if matches_st(&search.st, &identity.uuid) {
                        let delay = (search.mx as u64).min(2) * 80;
                        thread::sleep(Duration::from_millis(delay));
                        let payload = search_response(&identity, &location, &search.st);
                        let _ = sock.send_to(payload.as_bytes(), from);
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(e) => {
                log::debug!("ssdp recv: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    announce(&identity, &location, false);
}

fn handle_client(
    mut stream: TcpStream,
    identity: &DeviceIdentity,
    location: &str,
    transport: &Arc<Mutex<Transport>>,
    events: &Arc<Mutex<Box<dyn FnMut(ReceiverEvent) + Send>>>,
) -> std::io::Result<()> {
    let _ = stream.set_nonblocking(false);
    let req = read_request(&mut stream)?;
    let path = req.path_only().to_ascii_lowercase();
    match (req.method.as_str(), path.as_str()) {
        ("GET" | "HEAD", "/" | "/index.html") => {
            let body = format!(
                "<html><body style='background:#111;color:#eee;font-family:sans-serif'>\
                 <h1>{}</h1><p>DLNA MediaRenderer ready.</p><p>{}</p></body></html>",
                escape(&identity.friendly_name),
                escape(location)
            );
            write_response(&mut stream, "200 OK", "text/html; charset=utf-8", body.as_bytes(), &[])
        }
        ("GET" | "HEAD", "/device.xml" | "/description.xml") => {
            let base = location.trim_end_matches("/device.xml");
            let xml = device_description(identity, base);
            write_response(
                &mut stream,
                "200 OK",
                "text/xml; charset=\"utf-8\"",
                xml.as_bytes(),
                &[],
            )
        }
        ("GET" | "HEAD", "/avtransport.xml") => write_response(
            &mut stream,
            "200 OK",
            "text/xml; charset=\"utf-8\"",
            av_transport_scpd().as_bytes(),
            &[],
        ),
        ("GET" | "HEAD", "/renderingcontrol.xml") => write_response(
            &mut stream,
            "200 OK",
            "text/xml; charset=\"utf-8\"",
            rendering_control_scpd().as_bytes(),
            &[],
        ),
        ("GET" | "HEAD", "/connectionmanager.xml") => write_response(
            &mut stream,
            "200 OK",
            "text/xml; charset=\"utf-8\"",
            connection_manager_scpd().as_bytes(),
            &[],
        ),
        ("POST", "/avtransport/control") => {
            soap_avtransport(&mut stream, &req, transport, events)
        }
        ("POST", "/renderingcontrol/control") => {
            soap_rendering(&mut stream, &req, transport, events)
        }
        ("POST", "/connectionmanager/control") => soap_connection(&mut stream, &req),
        ("SUBSCRIBE", _) => {
            write_response(
                &mut stream,
                "200 OK",
                "text/xml; charset=\"utf-8\"",
                b"",
                &[
                    ("SID", "uuid:castify-event-1"),
                    ("TIMEOUT", "Second-1800"),
                ],
            )
        }
        ("UNSUBSCRIBE", _) => write_response(
            &mut stream,
            "200 OK",
            "text/xml; charset=\"utf-8\"",
            b"",
            &[],
        ),
        _ => write_response(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found",
            &[],
        ),
    }
}

fn soap_avtransport(
    stream: &mut TcpStream,
    req: &HttpRequest,
    transport: &Arc<Mutex<Transport>>,
    events: &Arc<Mutex<Box<dyn FnMut(ReceiverEvent) + Send>>>,
) -> std::io::Result<()> {
    const SVC: &str = "urn:schemas-upnp-org:service:AVTransport:1";
    let action = SoapAction::parse(req.header("SOAPACTION").unwrap_or(""), &req.body_text());
    let (action_name, inner) = match action {
        SoapAction::SetAvTransportUri { uri, metadata } => {
            let didl = crate::soap::didl_from_set_uri(&metadata);
            let mime = didl.mime.clone();
            let title = didl.title.clone();
            let play_uri = if uri.is_empty() {
                didl.uri.clone().unwrap_or_default()
            } else {
                uri
            };
            {
                let mut t = transport.lock().unwrap();
                t.uri = play_uri.clone();
                t.metadata = metadata;
                t.mime = mime.clone();
                t.title = title.clone();
                t.state = "STOPPED";
            }
            if !play_uri.is_empty() {
                let host_uri = match crate::guard::open_for_host(&play_uri) {
                    Ok(uri) => uri,
                    Err(err) => {
                        emit(events, ReceiverEvent::Error(err.to_string()));
                        let _ = write_response(
                            stream,
                            "500 Internal Server Error",
                            "text/plain",
                            err.to_string().as_bytes(),
                            &[],
                        );
                        return Ok(());
                    }
                };
                let kind = didl.kind(&play_uri);
                {
                    let mut t = transport.lock().unwrap();
                    t.host_uri = host_uri.clone();
                    t.state = "PLAYING";
                }
                emit(
                    events,
                    ReceiverEvent::Play(ReceivedItem {
                        uri: host_uri,
                        mime,
                        title,
                        kind,
                    }),
                );
            }
            ("SetAVTransportURIResponse", String::new())
        }
        SoapAction::Play => {
            let item = {
                let mut t = transport.lock().unwrap();
                t.state = "PLAYING";
                let uri = if t.host_uri.is_empty() {
                    t.uri.clone()
                } else {
                    t.host_uri.clone()
                };
                ReceivedItem {
                    uri,
                    mime: t.mime.clone(),
                    title: t.title.clone(),
                    kind: crate::media::MediaKind::from_mime_and_uri(t.mime.as_deref(), &t.uri),
                }
            };
            if !item.uri.is_empty() {
                emit(events, ReceiverEvent::Play(item));
            }
            ("PlayResponse", String::new())
        }
        SoapAction::Pause => {
            transport.lock().unwrap().state = "PAUSED_PLAYBACK";
            emit(events, ReceiverEvent::Pause);
            ("PauseResponse", String::new())
        }
        SoapAction::Stop => {
            transport.lock().unwrap().state = "STOPPED";
            emit(events, ReceiverEvent::Stop);
            ("StopResponse", String::new())
        }
        SoapAction::Seek => ("SeekResponse", String::new()),
        SoapAction::GetTransportInfo => {
            let state = transport.lock().unwrap().state;
            (
                "GetTransportInfoResponse",
                format!(
                    "<CurrentTransportState>{state}</CurrentTransportState>\
                     <CurrentTransportStatus>OK</CurrentTransportStatus>\
                     <CurrentSpeed>1</CurrentSpeed>"
                ),
            )
        }
        SoapAction::GetMediaInfo => {
            let t = transport.lock().unwrap();
            (
                "GetMediaInfoResponse",
                format!(
                    "<NrTracks>1</NrTracks><MediaDuration>00:00:00</MediaDuration>\
                     <CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>\
                     <NextURI></NextURI><NextURIMetaData></NextURIMetaData>\
                     <PlayMedium>NETWORK</PlayMedium><RecordMedium>NOT_IMPLEMENTED</RecordMedium>\
                     <WriteStatus>NOT_IMPLEMENTED</WriteStatus>",
                    escape(&t.uri),
                    escape(&t.metadata)
                ),
            )
        }
        SoapAction::GetPositionInfo => {
            let t = transport.lock().unwrap();
            (
                "GetPositionInfoResponse",
                format!(
                    "<Track>1</Track><TrackDuration>00:00:00</TrackDuration>\
                     <TrackMetaData>{}</TrackMetaData><TrackURI>{}</TrackURI>\
                     <RelTime>00:00:00</RelTime><AbsTime>00:00:00</AbsTime>\
                     <RelCount>2147483647</RelCount><AbsCount>2147483647</AbsCount>",
                    escape(&t.metadata),
                    escape(&t.uri)
                ),
            )
        }
        SoapAction::GetDeviceCapabilities => (
            "GetDeviceCapabilitiesResponse",
            "<PlayMedia>NETWORK</PlayMedia><RecMedia>NOT_IMPLEMENTED</RecMedia>\
             <RecQualityModes>NOT_IMPLEMENTED</RecQualityModes>"
                .into(),
        ),
        SoapAction::GetTransportSettings => (
            "GetTransportSettingsResponse",
            "<PlayMode>NORMAL</PlayMode><RecQualityMode>NOT_IMPLEMENTED</RecQualityMode>".into(),
        ),
        SoapAction::GetCurrentTransportActions => (
            "GetCurrentTransportActionsResponse",
            "<Actions>Play,Pause,Stop,Seek</Actions>".into(),
        ),
        SoapAction::Unknown(name) => {
            log::debug!("unhandled AVTransport action {name}");
            ("PlayResponse", String::new())
        }
        _ => ("PlayResponse", String::new()),
    };
    let xml = envelope(SVC, action_name, &inner);
    write_response(
        stream,
        "200 OK",
        "text/xml; charset=\"utf-8\"",
        xml.as_bytes(),
        &[("EXT", "")],
    )
}

fn soap_rendering(
    stream: &mut TcpStream,
    req: &HttpRequest,
    transport: &Arc<Mutex<Transport>>,
    events: &Arc<Mutex<Box<dyn FnMut(ReceiverEvent) + Send>>>,
) -> std::io::Result<()> {
    const SVC: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
    let action = SoapAction::parse(req.header("SOAPACTION").unwrap_or(""), &req.body_text());
    let (name, inner) = match action {
        SoapAction::GetVolume => {
            let v = transport.lock().unwrap().volume;
            (
                "GetVolumeResponse",
                format!("<CurrentVolume>{v}</CurrentVolume>"),
            )
        }
        SoapAction::SetVolume { percent } => {
            transport.lock().unwrap().volume = percent;
            let muted = transport.lock().unwrap().muted;
            emit(
                events,
                ReceiverEvent::Volume {
                    percent,
                    muted,
                },
            );
            ("SetVolumeResponse", String::new())
        }
        SoapAction::GetMute => {
            let m = if transport.lock().unwrap().muted { "1" } else { "0" };
            ("GetMuteResponse", format!("<CurrentMute>{m}</CurrentMute>"))
        }
        SoapAction::SetMute { muted } => {
            transport.lock().unwrap().muted = muted;
            let percent = transport.lock().unwrap().volume;
            emit(events, ReceiverEvent::Volume { percent, muted });
            ("SetMuteResponse", String::new())
        }
        _ => ("GetVolumeResponse", "<CurrentVolume>50</CurrentVolume>".into()),
    };
    let xml = envelope(SVC, name, &inner);
    write_response(
        stream,
        "200 OK",
        "text/xml; charset=\"utf-8\"",
        xml.as_bytes(),
        &[("EXT", "")],
    )
}

fn soap_connection(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    const SVC: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";
    let action = SoapAction::parse(req.header("SOAPACTION").unwrap_or(""), &req.body_text());
    let (name, inner) = match action {
        SoapAction::GetProtocolInfo => (
            "GetProtocolInfoResponse",
            format!(
                "<Source></Source><Sink>{}</Sink>",
                escape(SINK_PROTOCOL_INFO)
            ),
        ),
        SoapAction::GetCurrentConnectionIds => (
            "GetCurrentConnectionIDsResponse",
            "<ConnectionIDs>0</ConnectionIDs>".into(),
        ),
        _ => (
            "GetCurrentConnectionInfoResponse",
            "<RcsID>0</RcsID><AVTransportID>0</AVTransportID><ProtocolInfo></ProtocolInfo>\
             <PeerConnectionManager></PeerConnectionManager><PeerConnectionID>-1</PeerConnectionID>\
             <Direction>Input</Direction><Status>OK</Status>"
                .into(),
        ),
    };
    let xml = envelope(SVC, name, &inner);
    write_response(
        stream,
        "200 OK",
        "text/xml; charset=\"utf-8\"",
        xml.as_bytes(),
        &[("EXT", "")],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream as StdTcp};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn start_loopback() -> (MediaRenderer, mpsc::Receiver<ReceiverEvent>, String) {
        let (tx, rx) = mpsc::channel();
        let mut cfg = RendererConfig::new("TV, Test", "11111111-2222-3333-4444-555555555555");
        cfg.bind_ip = Some("127.0.0.1".into());
        cfg.http_port = 0;
        let renderer = MediaRenderer::start(cfg, move |ev| {
            let _ = tx.send(ev);
        })
        .expect("start renderer");
        let location = renderer.location();
        (renderer, rx, location)
    }

    fn connect(hostport: &str) -> StdTcp {
        let addr: SocketAddr = hostport.parse().expect("hostport");
        for _ in 0..40 {
            if let Ok(s) = StdTcp::connect_timeout(&addr, Duration::from_millis(100)) {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                return s;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("could not connect to {hostport}");
    }

    fn http_exchange(hostport: &str, request: &str) -> String {
        let mut s = connect(hostport);
        s.write_all(request.as_bytes()).unwrap();
        let _ = s.shutdown(std::net::Shutdown::Write);
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        buf
    }

    fn hostport_of(location: &str) -> String {
        let rest = location.trim_start_matches("http://");
        rest.split_once('/')
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| rest.to_string())
    }

    #[test]
    fn serves_device_xml() {
        let (renderer, rx, location) = start_loopback();
        assert!(location.starts_with("http://127.0.0.1:"));
        let hostport = hostport_of(&location);
        let buf = http_exchange(
            &hostport,
            &format!("GET /device.xml HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n"),
        );
        assert!(buf.contains("MediaRenderer:1"));
        assert!(buf.contains("AVTransport"));
        renderer.stop();
        drop(rx);
    }

    #[test]
    fn set_uri_emits_play() {
        let (renderer, rx, location) = start_loopback();
        let hostport = hostport_of(&location);
        let body = concat!(
            "<?xml version=\"1.0\"?>",
            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">",
            "<s:Body>",
            "<u:SetAVTransportURI xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">",
            "<InstanceID>0</InstanceID>",
            "<CurrentURI>http://10.0.0.8/photo.jpg</CurrentURI>",
            "<CurrentURIMetaData></CurrentURIMetaData>",
            "</u:SetAVTransportURI></s:Body></s:Envelope>"
        );
        let req = format!(
            "POST /avtransport/control HTTP/1.1\r\n\
Host: {hostport}\r\n\
CONTENT-TYPE: text/xml; charset=\"utf-8\"\r\n\
SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI\"\r\n\
CONTENT-LENGTH: {}\r\n\
CONNECTION: close\r\n\r\n{body}",
            body.len()
        );
        let buf = http_exchange(&hostport, &req);
        assert!(buf.contains("SetAVTransportURIResponse"));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let play = loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(ReceiverEvent::Play(item)) => break item,
                Ok(_) => continue,
                Err(_) => panic!("timed out waiting for play event"),
            }
        };
        assert_eq!(play.uri, "http://10.0.0.8/photo.jpg");
        assert_eq!(play.kind, crate::media::MediaKind::Image);
        renderer.stop();
    }
}
