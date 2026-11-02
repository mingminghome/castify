use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crate::crypto::{self, read_cipher_range, MediaKey, CHUNK, ENC_ID, ENC_QUERY, HDR_CHUNK, HDR_ENC, HDR_MIME, HDR_PLAIN, HDR_SALT};
use crate::http::{
    for_inclusive_windows, read_request, write_headers, write_raw_headers, write_response,
    HttpRequest,
};
use crate::net::local_ipv4;

/// Serves one local file over HTTP so a LAN renderer can fetch it.
/// Supports `Range` so video players can seek.
pub struct FileServer {
    stop: Arc<AtomicBool>,
    pub public_url: String,
}

impl FileServer {
    pub fn start(path: impl AsRef<Path>, mime: impl Into<String>) -> std::io::Result<Self> {
        Self::start_with(path, mime, None)
    }

    /// `passphrase` encrypts the bytes on the wire. The receiver needs the same value.
    pub fn start_with(
        path: impl AsRef<Path>,
        mime: impl Into<String>,
        passphrase: Option<&str>,
    ) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("missing {}", path.display()),
            ));
        }
        let mime = mime.into();
        let plain_len = path.metadata()?.len();
        let key = match passphrase.map(str::trim).filter(|s| !s.is_empty()) {
            Some(pass) => Some(MediaKey::generate(pass)?),
            None => None,
        };
        let token = crypto::random_token()?;
        let listener = TcpListener::bind(("0.0.0.0", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let ip = local_ipv4().unwrap_or(std::net::Ipv4Addr::LOCALHOST);
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("media.bin");
        let public_url = if key.is_some() {
            format!("http://{ip}:{port}/{token}/{name}?{ENC_QUERY}")
        } else {
            format!("http://{ip}:{port}/{token}/{name}")
        };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        thread::Builder::new()
            .name("castify-serve".into())
            .spawn(move || serve_loop(listener, path, mime, plain_len, key, token, flag))?;
        Ok(Self { stop, public_url })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Inclusive start/end and whether this is a 206 partial response.
pub fn parse_byte_range(header: Option<&str>, total: u64) -> (u64, u64, bool) {
    if total == 0 {
        return (0, 0, false);
    }
    let last = total - 1;
    let raw = match header {
        Some(h) => h.trim(),
        None => return (0, last, false),
    };
    let spec = raw
        .strip_prefix("bytes=")
        .or_else(|| raw.strip_prefix("BYTES="))
        .unwrap_or(raw)
        .trim();
    if spec.is_empty() {
        return (0, last, false);
    }
    if let Some(suffix) = spec.strip_prefix('-') {
        if let Ok(n) = suffix.parse::<u64>() {
            let n = n.min(total);
            return (total - n, last, true);
        }
        return (0, last, false);
    }
    let (a, b) = match spec.split_once('-') {
        Some(p) => p,
        None => return (0, last, false),
    };
    let start = a.parse::<u64>().unwrap_or(0).min(last);
    let end = if b.is_empty() {
        last
    } else {
        b.parse::<u64>().unwrap_or(last).min(last)
    };
    if end < start {
        return (0, last, false);
    }
    (start, end, true)
}

fn serve_loop(
    listener: TcpListener,
    path: PathBuf,
    mime: String,
    plain_len: u64,
    key: Option<MediaKey>,
    token: String,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(15)));
                match read_request(&mut stream) {
                    Ok(req) => {
                        if !req.path.contains(&token) {
                            let _ = write_response(
                                &mut stream,
                                "404 Not Found",
                                "text/plain",
                                b"missing",
                                &[],
                            );
                        } else if req.method.eq_ignore_ascii_case("GET")
                            || req.method.eq_ignore_ascii_case("HEAD")
                        {
                            let served = if let Some(key) = &key {
                                serve_encrypted(&mut stream, &path, &mime, plain_len, key, &req)
                            } else {
                                serve_file(&mut stream, &path, &mime, &req)
                            };
                            let _ = served;
                        } else {
                            let _ = write_response(
                                &mut stream,
                                "405 Method Not Allowed",
                                "text/plain",
                                b"",
                                &[],
                            );
                        }
                    }
                    Err(_) => {}
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn serve_file(
    stream: &mut std::net::TcpStream,
    path: &Path,
    mime: &str,
    req: &HttpRequest,
) -> std::io::Result<()> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            return write_response(stream, "404 Not Found", "text/plain", b"missing", &[]);
        }
    };
    let total = file.metadata()?.len();
    let (start, end, partial) = parse_byte_range(req.header("Range"), total);
    let length = if total == 0 { 0 } else { end.saturating_sub(start) + 1 };
    let status = if partial { "206 Partial Content" } else { "200 OK" };
    let content_range = format!("bytes {start}-{end}/{total}");
    let extra: Vec<(&str, &str)> = if partial {
        vec![("CONTENT-RANGE", content_range.as_str())]
    } else {
        vec![]
    };
    write_headers(stream, status, mime, length, &extra)?;
    if req.method.eq_ignore_ascii_case("HEAD") || length == 0 {
        stream.flush()?;
        let _ = stream.shutdown(std::net::Shutdown::Write);
        return Ok(());
    }
    file.seek(SeekFrom::Start(start))?;
    let mut remaining = length;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let chunk = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..chunk])?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

fn serve_encrypted(
    stream: &mut std::net::TcpStream,
    path: &Path,
    mime: &str,
    plain_len: u64,
    key: &MediaKey,
    req: &HttpRequest,
) -> std::io::Result<()> {
    let total_c = crypto::cipher_total(plain_len, CHUNK);
    let (start, end, partial) = parse_byte_range(req.header("Range"), total_c.max(1));
    let (start, end, partial) = if total_c == 0 {
        (0, 0, false)
    } else {
        (start, end, partial)
    };
    let is_head = req.method.eq_ignore_ascii_case("HEAD");
    let length = if total_c == 0 {
        0
    } else {
        end.saturating_sub(start).saturating_add(1)
    };
    let status = if partial { "206 Partial Content" } else { "200 OK" };
    let length_text = length.to_string();
    let plain_text = plain_len.to_string();
    let salt = crypto::hex_encode(&key.salt);
    let range_text = format!("bytes {start}-{end}/{total_c}");
    let mime_safe: String = mime.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    let mut headers = vec![
        ("CONTENT-TYPE", "application/octet-stream"),
        ("CONTENT-LENGTH", length_text.as_str()),
        ("CONNECTION", "close"),
        ("ACCEPT-RANGES", "bytes"),
        (HDR_ENC, ENC_ID),
        (HDR_SALT, salt.as_str()),
        (HDR_CHUNK, "65536"),
        (HDR_MIME, mime_safe.as_str()),
        (HDR_PLAIN, plain_text.as_str()),
    ];
    if partial {
        headers.push(("CONTENT-RANGE", range_text.as_str()));
    }
    write_raw_headers(stream, status, &headers)?;
    if !is_head && length > 0 {
        for_inclusive_windows(start, end, 256 * 1024, |from, to| {
            let bytes = read_cipher_range(key, CHUNK, plain_len, from, to, |off, len| {
                read_file_at(path, off, len)
            })?;
            stream.write_all(&bytes)?;
            Ok(())
        })?;
    }
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

fn read_file_at(path: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::http_exchange;
    use std::io::Write;

    fn serve_url(path: &Path) -> (FileServer, String) {
        let server = FileServer::start(path, "video/mp4").unwrap();
        let rest = server.public_url.trim_start_matches("http://");
        let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
        let port = hostport.rsplit(':').next().unwrap_or(hostport);
        let url = format!("http://127.0.0.1:{port}/{path}");
        (server, url)
    }

    #[test]
    fn parse_range_suffix_and_open_end() {
        assert_eq!(parse_byte_range(None, 100), (0, 99, false));
        assert_eq!(parse_byte_range(Some("bytes=0-9"), 100), (0, 9, true));
        assert_eq!(parse_byte_range(Some("bytes=50-"), 100), (50, 99, true));
        assert_eq!(parse_byte_range(Some("bytes=-10"), 100), (90, 99, true));
    }

    #[test]
    fn serves_rejects_missing_token() {
        let path = std::env::temp_dir().join("castify-serve-token.bin");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"secret").unwrap();
        }
        let (server, url) = serve_url(&path);
        let port = url.trim_start_matches("http://127.0.0.1:").split('/').next().unwrap();
        let bare = format!("http://127.0.0.1:{port}/castify-serve-token.bin");
        let (status, _) = http_exchange(&bare, "GET", &[], &[]).unwrap();
        assert_eq!(status, 404);
        server.stop();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn serves_temp_file() {
        let path = std::env::temp_dir().join("castify-serve-test.bin");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"hello-cast").unwrap();
        }
        let (server, url) = serve_url(&path);
        let (status, body) = http_exchange(&url, "GET", &[], &[]).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"hello-cast");
        server.stop();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn serves_byte_range() {
        let path = std::env::temp_dir().join("castify-serve-range.bin");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"0123456789").unwrap();
        }
        let (server, url) = serve_url(&path);
        let (status, body) = http_exchange(&url, "GET", &[("Range", "bytes=2-5")], &[]).unwrap();
        assert_eq!(status, 206);
        assert_eq!(body, b"2345");
        server.stop();
        let _ = std::fs::remove_file(path);
    }
}
