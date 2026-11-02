use crate::didl::DidlItem;
use crate::http::http_exchange;
use crate::soap::envelope;
use crate::xml::escape;

const AV_TRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";

/// SetAVTransportURI + Play on a MediaRenderer control URL.
pub fn play(control_url: &str, media_url: &str, title: &str, mime: &str) -> std::io::Result<()> {
    let media_url = crate::guard::wrap_outgoing(control_url, media_url, mime)?;
    let item = DidlItem {
        title: Some(title.to_string()),
        mime: Some(mime.to_string()),
        uri: Some(media_url.clone()),
    };
    let didl = item.to_xml();
    let inner = format!(
        "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>",
        escape(&media_url),
        escape(&didl)
    );
    soap(control_url, "SetAVTransportURI", &inner)?;
    soap(
        control_url,
        "Play",
        "<InstanceID>0</InstanceID><Speed>1</Speed>",
    )?;
    Ok(())
}

pub fn stop(control_url: &str) -> std::io::Result<()> {
    soap(control_url, "Stop", "<InstanceID>0</InstanceID>")?;
    Ok(())
}

pub fn soap(control_url: &str, action: &str, inner: &str) -> std::io::Result<String> {
    let body = envelope(AV_TRANSPORT, action, inner);
    let soapaction = format!("\"{AV_TRANSPORT}#{action}\"");
    let (status, resp) = http_exchange(
        control_url,
        "POST",
        &[
            ("CONTENT-TYPE", "text/xml; charset=\"utf-8\""),
            ("SOAPACTION", &soapaction),
        ],
        body.as_bytes(),
    )?;
    if status >= 400 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("{action} -> HTTP {status}"),
        ));
    }
    Ok(String::from_utf8_lossy(&resp).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn play_posts_set_uri_then_play() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                // Read the whole request. A single read can return before the body arrives.
                let req = crate::http::read_request(&mut s).unwrap();
                let mut dump = String::new();
                for (k, v) in &req.headers {
                    dump.push_str(k);
                    dump.push_str(": ");
                    dump.push_str(v);
                    dump.push('\n');
                }
                dump.push_str(&String::from_utf8_lossy(&req.body));
                seen.push(dump);
                let body = b"<s:Envelope/>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                s.write_all(resp.as_bytes()).unwrap();
                s.write_all(body).unwrap();
            }
            seen
        });
        let url = format!("http://127.0.0.1:{port}/avtransport/control");
        play(&url, "http://10.0.0.8/p.jpg", "Photo", "image/jpeg").unwrap();
        let seen = handle.join().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].contains("SetAVTransportURI"));
        assert!(seen[0].contains("http://10.0.0.8/p.jpg"));
        assert!(seen[0].contains("SOAPACTION"));
        assert!(seen[1].contains("Play"));
    }
}
