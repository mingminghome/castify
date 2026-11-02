use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const MAX_HEADER_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_REQUEST_BODY: usize = 1024 * 1024;
const MAX_EXCHANGE_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or(&self.path)
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub fn read_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let (first, headers) = read_header_lines(&mut reader)?;
    let first = first.trim_end_matches(['\r', '\n']);
    let mut parts = first.splitn(3, ' ');
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if len > MAX_REQUEST_BODY {
        return Err(io::Error::new(ErrorKind::InvalidData, "request body too large"));
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn read_limited_line(reader: &mut impl BufRead) -> io::Result<String> {
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let take = match newline {
            Some(i) => i + 1,
            None => available.len(),
        };
        let room = MAX_HEADER_LINE.saturating_sub(buf.len());
        if take > room {
            return Err(io::Error::new(ErrorKind::InvalidData, "header line too long"));
        }
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            break;
        }
    }
    String::from_utf8(buf).map_err(|_| io::Error::new(ErrorKind::InvalidData, "header is not utf-8"))
}

fn read_header_lines(reader: &mut impl BufRead) -> io::Result<(String, Vec<(String, String)>)> {
    let first = read_limited_line(reader)?;
    let mut headers = Vec::new();
    loop {
        let line = read_limited_line(reader)?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(io::Error::new(ErrorKind::InvalidData, "too many headers"));
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((header_text(k.trim()), header_text(v.trim())));
        }
    }
    Ok((first, headers))
}

fn header_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
        .collect()
}

pub fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> std::io::Result<()> {
    write_headers(stream, status, content_type, body.len() as u64, extra)?;
    stream.write_all(body)?;
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

pub fn write_raw_headers(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {}\r\n", header_text(status));
    for (k, v) in headers {
        head.push_str(&header_text(k));
        head.push_str(": ");
        head.push_str(&header_text(v));
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())
}

pub fn write_headers(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    content_length: u64,
    extra: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nCONTENT-TYPE: {content_type}\r\nCONTENT-LENGTH: {content_length}\r\nCONNECTION: close\r\nACCEPT-RANGES: bytes\r\nSERVER: Linux/3.0 UPnP/1.0 castify/0.1\r\n",
        status = header_text(status),
        content_type = header_text(content_type),
    );
    for (k, v) in extra {
        head.push_str(&header_text(k));
        head.push_str(": ");
        head.push_str(&header_text(v));
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl HttpUrl {
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("http://")?;
        let (hostport, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = if let Some((h, p)) = hostport.rsplit_once(':') {
            if h.starts_with('[') {
                return None;
            }
            (h.to_string(), p.parse().ok()?)
        } else {
            (hostport.to_string(), 80)
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            host,
            port,
            path: if path.is_empty() {
                "/".into()
            } else {
                path.to_string()
            },
        })
    }

    pub fn origin(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn join(&self, rel: &str) -> String {
        let rel = rel.trim();
        if rel.starts_with("http://") || rel.starts_with("https://") {
            return rel.to_string();
        }
        if rel.starts_with('/') {
            return format!("{}{}", self.origin(), rel);
        }
        let dir = self
            .path
            .rsplit_once('/')
            .map(|(d, _)| d.trim_start_matches('/'))
            .unwrap_or("");
        if dir.is_empty() {
            format!("{}/{}", self.origin(), rel)
        } else {
            format!("{}/{}/{}", self.origin(), dir, rel)
        }
    }
}

/// HTTP/1.1 request, returns status and body.
pub fn http_exchange(
    url: &str,
    method: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<(u16, Vec<u8>)> {
    let parsed = HttpUrl::parse(url).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad url {url}"))
    })?;
    let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(8)))?;
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nUser-Agent: castify/0.1\r\n",
        method = method,
        path = parsed.path,
        host = parsed.host,
        port = parsed.port,
    );
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (k, v) in extra {
        req.push_str(&header_text(k));
        req.push_str(": ");
        req.push_str(&header_text(v));
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body)?;
    }
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut reader = BufReader::new(stream);
    let (first, headers) = read_header_lines(&mut reader)?;
    let status = first
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let mut resp_body = Vec::new();
    if let Some((_, len)) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        if let Ok(n) = len.parse::<usize>() {
            if n > MAX_EXCHANGE_BODY {
                return Err(io::Error::new(ErrorKind::InvalidData, "response body too large"));
            }
            resp_body.resize(n, 0);
            reader.read_exact(&mut resp_body)?;
        }
    } else {
        reader.take(MAX_EXCHANGE_BODY as u64).read_to_end(&mut resp_body)?;
    }
    Ok((status, resp_body))
}

pub struct HttpBody {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    reader: BufReader<TcpStream>,
    left: Option<u64>,
}

impl Read for HttpBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(left) = self.left {
            if left == 0 {
                return Ok(0);
            }
            let max = buf.len().min(left as usize);
            let n = self.reader.read(&mut buf[..max])?;
            self.left = Some(left - n as u64);
            return Ok(n);
        }
        self.reader.read(buf)
    }
}

pub fn http_head(url: &str) -> std::io::Result<(u16, Vec<(String, String)>)> {
    let body = open_http(url, "HEAD", &[], std::time::Duration::from_secs(3), false)?;
    Ok((body.status, body.headers))
}

pub fn http_open(
    url: &str,
    method: &str,
    extra: &[(&str, &str)],
    timeout: std::time::Duration,
) -> std::io::Result<HttpBody> {
    open_http(url, method, extra, timeout, true)
}

fn open_http(
    url: &str,
    method: &str,
    extra: &[(&str, &str)],
    timeout: std::time::Duration,
    read_body: bool,
) -> std::io::Result<HttpBody> {
    let parsed = HttpUrl::parse(url).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad url {url}"))
    })?;
    let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nUser-Agent: castify/0.1\r\n",
        method = method,
        path = parsed.path,
        host = parsed.host,
        port = parsed.port,
    );
    for (k, v) in extra {
        req.push_str(&header_text(k));
        req.push_str(": ");
        req.push_str(&header_text(v));
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let (first, headers) = read_header_lines(&mut reader)?;
    let status = first
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let left = if read_body {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse::<u64>().ok())
    } else {
        Some(0)
    };
    Ok(HttpBody {
        status,
        headers,
        reader,
        left,
    })
}

/// Walk an inclusive byte range in bounded windows so one request cannot
/// allocate the whole object.
pub fn for_inclusive_windows<F>(start: u64, end: u64, window: u64, mut body: F) -> io::Result<()>
where
    F: FnMut(u64, u64) -> io::Result<()>,
{
    if window == 0 || end < start {
        return Ok(());
    }
    let mut cursor = start;
    loop {
        let window_end = cursor.saturating_add(window - 1).min(end);
        body(cursor, window_end)?;
        if window_end >= end {
            return Ok(());
        }
        cursor = window_end + 1;
    }
}

/// Caps how many request threads a listener may run at once.
#[derive(Clone)]
pub struct ConnLimit {
    inflight: Arc<AtomicUsize>,
    max: usize,
}

pub struct ConnPermit {
    inflight: Arc<AtomicUsize>,
}

impl Drop for ConnPermit {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ConnLimit {
    pub fn new(max: usize) -> Self {
        Self {
            inflight: Arc::new(AtomicUsize::new(0)),
            max,
        }
    }

    pub fn try_acquire(&self) -> Option<ConnPermit> {
        let mut current = self.inflight.load(Ordering::Relaxed);
        loop {
            if current >= self.max {
                return None;
            }
            match self.inflight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ConnPermit {
                        inflight: self.inflight.clone(),
                    })
                }
                Err(actual) => current = actual,
            }
        }
    }
}

pub fn http_get_text(url: &str) -> std::io::Result<String> {
    let (status, body) = http_exchange(url, "GET", &[], &[])?;
    if status != 200 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("GET {url} -> {status}"),
        ));
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_strips_query() {
        let req = HttpRequest {
            method: "GET".into(),
            path: "/device.xml?x=1".into(),
            headers: vec![],
            body: vec![],
        };
        assert_eq!(req.path_only(), "/device.xml");
    }

    #[test]
    fn rejects_huge_content_length() {
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"POST / HTTP/1.1\r\nContent-Length: 999999999\r\n\r\n")
                .unwrap();
        });
        let (mut sock, _) = listener.accept().unwrap();
        let err = read_request(&mut sock).unwrap_err();
        assert!(err.to_string().contains("too large"));
        client.join().unwrap();
    }

    #[test]
    fn parses_http_url_and_joins_control_path() {
        let u = HttpUrl::parse("http://192.168.3.177:7676/device.xml").unwrap();
        assert_eq!(u.host, "192.168.3.177");
        assert_eq!(u.port, 7676);
        assert_eq!(u.path, "/device.xml");
        assert_eq!(
            u.join("/avtransport/control"),
            "http://192.168.3.177:7676/avtransport/control"
        );
        assert_eq!(
            u.join("http://10.0.0.2/c"),
            "http://10.0.0.2/c"
        );
    }
}
