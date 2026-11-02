//! Optional passphrase protection for media on the LAN.
//!
//! SSDP and SOAP stay in the clear so ordinary DLNA controllers still work
//! when no passphrase is set. With a passphrase, local files and clear
//! `http://` media use AES-256-GCM, and the receiver refuses a clear `http://`
//! cast. An `https://` URL stays a direct TLS fetch, and the sender offers it
//! only to a castify renderer.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::crypto::{
    chunk_plain_len, cipher_total, header_value, hex_decode_16, hex_encode, pack_stream_frame,
    read_cipher_range, read_plain_range, MediaKey, CHUNK, ENC_ID, ENC_QUERY, HDR_CHUNK, HDR_ENC,
    HDR_MIME, HDR_PLAIN, HDR_SALT, HDR_STREAM, OVERHEAD,
};
use crate::http::{
    self, for_inclusive_windows, http_get_text, read_request, write_raw_headers, write_response,
    ConnLimit, HttpUrl,
};

const CONN_CAP: usize = 32;
const RANGE_WINDOW: u64 = 256 * 1024;
const MAX_PLAIN_LEN: u64 = 32 * 1024 * 1024 * 1024;
use crate::net::local_ipv4;
use crate::secret;
use crate::serve::parse_byte_range;
use crate::xml::tag_text;

static RELAY: Mutex<Option<Relay>> = Mutex::new(None);
static PROXY: Mutex<Option<DecryptProxy>> = Mutex::new(None);

pub fn stop_relay() {
    if let Ok(mut slot) = RELAY.lock() {
        if let Some(relay) = slot.take() {
            relay.stop();
        }
    }
}

pub fn stop_proxy() {
    if let Ok(mut slot) = PROXY.lock() {
        if let Some(proxy) = slot.take() {
            proxy.stop();
        }
    }
}

/// Rewrite a play URL when a passphrase is set.
pub fn wrap_outgoing(control_url: &str, media_url: &str, mime: &str) -> io::Result<String> {
    wrap_outgoing_with(control_url, media_url, mime, secret::passphrase().as_deref())
}

fn wrap_outgoing_with(
    control_url: &str,
    media_url: &str,
    mime: &str,
    passphrase: Option<&str>,
) -> io::Result<String> {
    let Some(passphrase) = passphrase.filter(|p| !p.trim().is_empty()) else {
        return Ok(media_url.to_string());
    };
    if !target_is_castify(control_url)? {
        return Err(io::Error::new(
            ErrorKind::Other,
            "encryption is on, and this renderer is not castify",
        ));
    }
    if media_url.to_ascii_lowercase().starts_with("https://") {
        return Ok(media_url.to_string());
    }
    if media_url.contains(ENC_QUERY) {
        stop_relay();
        return Ok(media_url.to_string());
    }
    if !media_url.to_ascii_lowercase().starts_with("http://") {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "encryption requires an http media URL",
        ));
    }
    let relay = Relay::start(media_url, mime, passphrase)?;
    let url = relay.public_url.clone();
    if let Ok(mut slot) = RELAY.lock() {
        if let Some(prev) = slot.replace(relay) {
            prev.stop();
        }
    }
    Ok(url)
}

/// URL the host player should open. Encrypted casts become a localhost proxy.
pub fn open_for_host(uri: &str) -> io::Result<String> {
    open_for_host_with(uri, secret::passphrase().as_deref())
}

fn open_for_host_with(uri: &str, passphrase: Option<&str>) -> io::Result<String> {
    if !uri.contains(ENC_QUERY) {
        if passphrase.filter(|p| !p.trim().is_empty()).is_some()
            && !uri.to_ascii_lowercase().starts_with("https://")
        {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "passphrase is set; refusing an unencrypted cast",
            ));
        }
        return Ok(uri.to_string());
    }
    let passphrase = passphrase.ok_or_else(|| {
        io::Error::new(
            ErrorKind::PermissionDenied,
            "encrypted cast requires a passphrase",
        )
    })?;
    let proxy = DecryptProxy::open(uri, passphrase)?;
    let local = proxy.local_url.clone();
    if let Ok(mut slot) = PROXY.lock() {
        if let Some(prev) = slot.replace(proxy) {
            prev.stop();
        }
    }
    Ok(local)
}

fn target_is_castify(control_url: &str) -> io::Result<bool> {
    let parsed = HttpUrl::parse(control_url).ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            format!("bad control url {control_url}"),
        )
    })?;
    let xml = http_get_text(&format!("{}/device.xml", parsed.origin()))?;
    let model = tag_text(&xml, "modelName").unwrap_or_default();
    Ok(model.eq_ignore_ascii_case("castify"))
}

struct Relay {
    stop: ArcFlag,
    public_url: String,
}

type ArcFlag = std::sync::Arc<AtomicBool>;

impl Relay {
    fn start(upstream: &str, mime: &str, passphrase: &str) -> io::Result<Self> {
        let key = MediaKey::generate(passphrase)?;
        let probe = probe_upstream(upstream)?;
        let token = crate::crypto::random_token()?;
        let listener = TcpListener::bind(("0.0.0.0", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let ip = local_ipv4().unwrap_or(std::net::Ipv4Addr::LOCALHOST);
        let public_url = format!("http://{ip}:{port}/{token}/media?{ENC_QUERY}");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let upstream = upstream.to_string();
        let mime = sanitize_header(mime);
        thread::Builder::new()
            .name("castify-relay".into())
            .spawn(move || relay_loop(listener, flag, upstream, mime, key, probe, token))?;
        Ok(Self { stop, public_url })
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Probe {
    plain_len: Option<u64>,
}

fn probe_upstream(url: &str) -> io::Result<Probe> {
    let (status, headers) = http::http_head(url)?;
    if status >= 400 {
        return Err(io::Error::new(
            ErrorKind::Other,
            format!("upstream HEAD {status}"),
        ));
    }
    let plain_len = header_value(&headers, "content-length").and_then(|s| s.parse().ok());
    Ok(Probe { plain_len })
}

fn relay_loop(
    listener: TcpListener,
    stop: ArcFlag,
    upstream: String,
    mime: String,
    key: MediaKey,
    probe: Probe,
    token: String,
) {
    let limit = ConnLimit::new(CONN_CAP);
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = limit.try_acquire() else {
                    drop(stream);
                    continue;
                };
                let upstream = upstream.clone();
                let mime = mime.clone();
                let key = key.clone();
                let plain_len = probe.plain_len;
                let flag = stop.clone();
                let token = token.clone();
                let _ = thread::Builder::new()
                    .name("castify-relay-conn".into())
                    .spawn(move || {
                        let _permit = permit;
                        let _ = relay_conn(stream, &upstream, &mime, &key, plain_len, &flag, &token);
                    });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn relay_conn(
    mut stream: TcpStream,
    upstream: &str,
    mime: &str,
    key: &MediaKey,
    plain_len: Option<u64>,
    stop: &AtomicBool,
    token: &str,
) -> io::Result<()> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    if !req.path.contains(token) {
        let _ = write_response(&mut stream, "404 Not Found", "text/plain", b"missing", &[]);
        return Ok(());
    }
    let is_head = req.method.eq_ignore_ascii_case("HEAD");
    let salt = hex_encode(&key.salt);
    if let Some(len) = plain_len {
        let total_c = cipher_total(len, CHUNK);
        let (start, end, partial) = parse_byte_range(req.header("Range"), total_c);
        let status = if partial {
            "206 Partial Content"
        } else {
            "200 OK"
        };
        let length = if total_c == 0 {
            0
        } else {
            end.saturating_sub(start).saturating_add(1)
        };
        let length_text = length.to_string();
        let plain_text = len.to_string();
        let range_text = format!("bytes {start}-{end}/{total_c}");
        let mut headers = vec![
            ("CONTENT-TYPE", "application/octet-stream"),
            ("CONTENT-LENGTH", length_text.as_str()),
            ("CONNECTION", "close"),
            ("ACCEPT-RANGES", "bytes"),
            (HDR_ENC, ENC_ID),
            (HDR_SALT, salt.as_str()),
            (HDR_CHUNK, "65536"),
            (HDR_MIME, mime),
            (HDR_PLAIN, plain_text.as_str()),
        ];
        if partial {
            headers.push(("CONTENT-RANGE", range_text.as_str()));
        }
        write_raw_headers(&mut stream, status, &headers)?;
        if !is_head && length > 0 {
            for_inclusive_windows(start, end, RANGE_WINDOW, |from, to| {
                let bytes = read_cipher_range(key, CHUNK, len, from, to, |off, n| {
                    http_read_at(upstream, off, n)
                })?;
                stream.write_all(&bytes)?;
                Ok(())
            })?;
        }
        stream.flush()?;
        Ok(())
    } else {
        let headers = [
            ("CONTENT-TYPE", "application/octet-stream"),
            ("CONNECTION", "close"),
            (HDR_ENC, ENC_ID),
            (HDR_SALT, salt.as_str()),
            (HDR_CHUNK, "65536"),
            (HDR_MIME, mime),
            (HDR_STREAM, "1"),
        ];
        write_raw_headers(&mut stream, "200 OK", &headers)?;
        if is_head {
            stream.flush()?;
            return Ok(());
        }
        let mut body = http::http_open(upstream, "GET", &[], Duration::from_millis(200))?;
        if body.status >= 400 {
            return Err(io::Error::new(ErrorKind::Other, "upstream get failed"));
        }
        encrypt_stream(&mut body, &mut stream, key, stop)
    }
}

fn encrypt_stream(
    src: &mut http::HttpBody,
    dst: &mut TcpStream,
    key: &MediaKey,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut index = 0u64;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut filled = 0usize;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match src.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled > 0 {
                    write_frame(dst, key, index, &buf[..filled])?;
                }
                break;
            }
            Ok(n) => {
                filled += n;
                if filled == buf.len() {
                    write_frame(dst, key, index, &buf)?;
                    filled = 0;
                    index += 1;
                }
            }
            Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {
                if filled > 0 {
                    write_frame(dst, key, index, &buf[..filled])?;
                    filled = 0;
                    index += 1;
                }
            }
            Err(e) => return Err(e),
        }
    }
    dst.flush()?;
    Ok(())
}

fn write_frame(dst: &mut TcpStream, key: &MediaKey, index: u64, plain: &[u8]) -> io::Result<()> {
    let packed = pack_stream_frame(key, index, plain)?;
    dst.write_all(&packed)?;
    dst.flush()?;
    Ok(())
}

fn http_read_at(url: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let end = offset + len as u64 - 1;
    let range = format!("bytes={offset}-{end}");
    let mut body = http::http_open(url, "GET", &[("Range", &range)], Duration::from_secs(8))?;
    if body.status != 206 && !(body.status == 200 && offset == 0) {
        return Err(io::Error::new(
            ErrorKind::Other,
            format!("upstream range {}", body.status),
        ));
    }
    let mut buf = vec![0u8; len];
    body.read_exact(&mut buf)?;
    Ok(buf)
}

struct Meta {
    key: MediaKey,
    chunk: u64,
    plain_len: Option<u64>,
    mime: String,
    upstream: String,
    token: String,
}

struct DecryptProxy {
    stop: ArcFlag,
    local_url: String,
}

impl DecryptProxy {
    fn open(upstream: &str, passphrase: &str) -> io::Result<Self> {
        let (status, headers) = http::http_head(upstream)?;
        if status >= 400 {
            return Err(io::Error::new(ErrorKind::Other, format!("HEAD {status}")));
        }
        if header_value(&headers, HDR_ENC) != Some(ENC_ID) {
            return Err(io::Error::new(ErrorKind::InvalidData, "not an encrypted cast"));
        }
        let salt = hex_decode_16(header_value(&headers, HDR_SALT).unwrap_or(""))
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "bad salt"))?;
        let key = MediaKey::open(passphrase, salt)?;
        let chunk = header_value(&headers, HDR_CHUNK)
            .and_then(|s| s.parse().ok())
            .unwrap_or(CHUNK);
        if chunk != CHUNK {
            return Err(io::Error::new(ErrorKind::InvalidData, "bad chunk"));
        }
        let plain_len = header_value(&headers, HDR_PLAIN).and_then(|s| s.parse::<u64>().ok());
        let streamed = header_value(&headers, HDR_STREAM).is_some();
        if plain_len.is_none() && !streamed {
            return Err(io::Error::new(ErrorKind::InvalidData, "missing enc length"));
        }
        if plain_len.is_some_and(|len| len > MAX_PLAIN_LEN) {
            return Err(io::Error::new(ErrorKind::InvalidData, "plain length too large"));
        }
        if let Some(len) = plain_len {
            if len > 0 {
                let plen = chunk_plain_len(0, chunk, len);
                let frame = fetch_range(upstream, 0, plen + OVERHEAD - 1)?;
                key.decrypt_chunk(0, &frame)?;
            }
        }
        let mime = sanitize_header(header_value(&headers, HDR_MIME).unwrap_or("application/octet-stream"));
        let token = crate::crypto::random_token()?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let local_url = format!("http://127.0.0.1:{port}/{token}");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let meta = Meta {
            key,
            chunk,
            plain_len,
            mime,
            upstream: upstream.to_string(),
            token,
        };
        thread::Builder::new()
            .name("castify-decrypt".into())
            .spawn(move || proxy_loop(listener, flag, meta))?;
        Ok(Self { stop, local_url })
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for DecryptProxy {
    fn drop(&mut self) {
        self.stop();
    }
}

fn proxy_loop(listener: TcpListener, stop: ArcFlag, meta: Meta) {
    let limit = ConnLimit::new(CONN_CAP);
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = limit.try_acquire() else {
                    drop(stream);
                    continue;
                };
                let meta_up = meta.upstream.clone();
                let meta_mime = meta.mime.clone();
                let key = meta.key.clone();
                let chunk = meta.chunk;
                let plain_len = meta.plain_len;
                let token = meta.token.clone();
                let _ = thread::Builder::new()
                    .name("castify-decrypt-conn".into())
                    .spawn(move || {
                        let _permit = permit;
                        let one = Meta {
                            key,
                            chunk,
                            plain_len,
                            mime: meta_mime,
                            upstream: meta_up,
                            token,
                        };
                        let _ = proxy_conn(stream, &one);
                    });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn proxy_conn(mut stream: TcpStream, meta: &Meta) -> io::Result<()> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    if !req.path.contains(&meta.token) {
        let _ = write_response(&mut stream, "404 Not Found", "text/plain", b"missing", &[]);
        return Ok(());
    }
    let is_head = req.method.eq_ignore_ascii_case("HEAD");
    if let Some(len) = meta.plain_len {
        let (start, end, partial) = parse_byte_range(req.header("Range"), len);
        let status = if partial {
            "206 Partial Content"
        } else {
            "200 OK"
        };
        let length = if len == 0 {
            0
        } else {
            end.saturating_sub(start).saturating_add(1)
        };
        let length_text = length.to_string();
        let range_text = format!("bytes {start}-{end}/{len}");
        let mut headers = vec![
            ("CONTENT-TYPE", meta.mime.as_str()),
            ("CONTENT-LENGTH", length_text.as_str()),
            ("CONNECTION", "close"),
            ("ACCEPT-RANGES", "bytes"),
        ];
        if partial {
            headers.push(("CONTENT-RANGE", range_text.as_str()));
        }
        write_raw_headers(&mut stream, status, &headers)?;
        if !is_head && length > 0 {
            for_inclusive_windows(start, end, RANGE_WINDOW, |from, to| {
                let bytes = read_plain_range(&meta.key, meta.chunk, len, from, to, |c0, c1| {
                    fetch_range(&meta.upstream, c0, c1)
                })?;
                stream.write_all(&bytes)?;
                Ok(())
            })?;
        }
        stream.flush()?;
        Ok(())
    } else {
        let headers = [
            ("CONTENT-TYPE", meta.mime.as_str()),
            ("CONNECTION", "close"),
            ("CACHE-CONTROL", "no-store"),
        ];
        write_raw_headers(&mut stream, "200 OK", &headers)?;
        if is_head {
            stream.flush()?;
            return Ok(());
        }
        let mut body = http::http_open(&meta.upstream, "GET", &[], Duration::from_secs(30))?;
        if body.status >= 400 {
            return Err(io::Error::new(ErrorKind::Other, "encrypted get failed"));
        }
        decrypt_stream(&mut body, &mut stream, &meta.key)
    }
}

fn decrypt_stream(src: &mut http::HttpBody, dst: &mut TcpStream, key: &MediaKey) -> io::Result<()> {
    let mut index = 0u64;
    loop {
        let mut len_buf = [0u8; 4];
        if !read_exact_or_eof(src, &mut len_buf)? {
            break;
        }
        let n = u32::from_be_bytes(len_buf) as usize;
        if n == 0 || n > CHUNK as usize + OVERHEAD as usize + 64 {
            return Err(io::Error::new(ErrorKind::InvalidData, "bad frame length"));
        }
        let mut frame = vec![0u8; n];
        src.read_exact(&mut frame)?;
        let plain = key.decrypt_chunk(index, &frame)?;
        dst.write_all(&plain)?;
        dst.flush()?;
        index += 1;
    }
    Ok(())
}

fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match reader.read(&mut buf[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => {
                return Err(io::Error::new(ErrorKind::UnexpectedEof, "truncated frame"));
            }
            Ok(n) => got += n,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn fetch_range(url: &str, start: u64, end: u64) -> io::Result<Vec<u8>> {
    if end < start {
        return Err(io::Error::new(ErrorKind::InvalidInput, "bad cipher range"));
    }
    let expect = end - start + 1;
    if expect > CHUNK + OVERHEAD + 64 {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "cipher range too large",
        ));
    }
    let range = format!("bytes={start}-{end}");
    let body = http::http_open(url, "GET", &[("Range", &range)], Duration::from_secs(8))?;
    if body.status != 206 && body.status != 200 {
        return Err(io::Error::new(
            ErrorKind::Other,
            format!("cipher range {}", body.status),
        ));
    }
    let mut buf = Vec::new();
    body.take(expect).read_to_end(&mut buf)?;
    Ok(buf)
}

fn sanitize_header(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::http_exchange;
    use crate::serve::FileServer;
    use std::io::Write;

    fn loopback(url: &str) -> String {
        let rest = url.trim_start_matches("http://");
        let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
        let port = hostport.rsplit_once(':').map(|(_, p)| p).unwrap_or("80");
        if path.is_empty() {
            format!("http://127.0.0.1:{port}/")
        } else {
            format!("http://127.0.0.1:{port}/{path}")
        }
    }

    #[test]
    fn passphrase_refuses_clear_http_and_keeps_https() {
        let clear = "http://10.0.0.8/a.jpg";
        let err = open_for_host_with(clear, Some("secret")).unwrap_err();
        assert!(err.to_string().contains("unencrypted"));
        let https = "https://example.com/a.jpg";
        assert_eq!(open_for_host_with(https, Some("secret")).unwrap(), https);
        assert_eq!(open_for_host_with(clear, None).unwrap(), clear);
        assert_eq!(open_for_host_with(clear, Some("  ")).unwrap(), clear);
    }

    #[test]
    fn encrypted_file_roundtrip_and_range() {
        let path = std::env::temp_dir().join("castify-enc-range.bin");
        let mut plain = vec![0u8; 70_000];
        for (i, b) in plain.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        std::fs::write(&path, &plain).unwrap();
        let server = FileServer::start_with(&path, "video/mp4", Some("lan-secret")).unwrap();
        let remote = loopback(&server.public_url);
        let proxy = DecryptProxy::open(&remote, "lan-secret").unwrap();
        let (status, body) =
            http_exchange(&proxy.local_url, "GET", &[("Range", "bytes=65000-66000")], &[]).unwrap();
        assert_eq!(status, 206);
        assert_eq!(body, plain[65000..=66000]);
        assert!(DecryptProxy::open(&remote, "other-secret").is_err());
        drop(proxy);
        server.stop();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stream_roundtrip() {
        let body = b"hello-from-stream-0123456789-abcdef".to_vec();
        let upstream = spawn_raw_server(body.clone());
        let relay = Relay::start(&upstream, "video/mp2t", "stream-secret").unwrap();
        let remote = loopback(&relay.public_url);
        let proxy = DecryptProxy::open(&remote, "stream-secret").unwrap();
        let (status, got) = http_exchange(&proxy.local_url, "GET", &[], &[]).unwrap();
        assert_eq!(status, 200);
        assert_eq!(got, body);
        drop(proxy);
        relay.stop();
    }

    fn spawn_raw_server(body: Vec<u8>) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for _ in 0..8 {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(head.as_bytes());
                let is_head = buf.starts_with(b"HEAD");
                if !is_head {
                    let _ = sock.write_all(&body);
                }
            }
        });
        format!("http://127.0.0.1:{port}/live.ts")
    }
}
