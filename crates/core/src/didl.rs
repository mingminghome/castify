use crate::media::MediaKind;
use crate::xml::{attr, escape, tag_text, unescape};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DidlItem {
    pub title: Option<String>,
    pub mime: Option<String>,
    pub uri: Option<String>,
}

impl DidlItem {
    pub fn parse(raw: &str) -> Self {
        if raw.trim().is_empty() {
            return Self::default();
        }
        let xml = if raw.contains("&lt;") && raw.contains("DIDL") {
            unescape(raw)
        } else {
            raw.to_string()
        };
        let title = tag_text(&xml, "dc:title").or_else(|| tag_text(&xml, "title"));
        let (uri, mime) = parse_res(&xml);
        Self { title, mime, uri }
    }

    pub fn kind(&self, fallback_uri: &str) -> MediaKind {
        MediaKind::from_mime_and_uri(self.mime.as_deref(), self.uri.as_deref().unwrap_or(fallback_uri))
    }

    pub fn to_xml(&self) -> String {
        let uri = self.uri.as_deref().unwrap_or("");
        let title = escape(self.title.as_deref().unwrap_or("Media"));
        let mime = escape(
            &self
                .mime
                .as_deref()
                .unwrap_or("application/octet-stream")
                .chars()
                .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
                .collect::<String>(),
        );
        let class = match self.kind(uri) {
            MediaKind::Image => "object.item.imageItem.photo",
            MediaKind::Video => "object.item.videoItem",
            MediaKind::Audio => "object.item.audioItem.musicTrack",
            MediaKind::Web => "object.item.textItem",
            MediaKind::Unknown => "object.item",
        };
        format!(
            concat!(
                "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" ",
                "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
                "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">",
                "<item id=\"0\" parentID=\"-1\" restricted=\"1\">",
                "<dc:title>{title}</dc:title>",
                "<upnp:class>{class}</upnp:class>",
                "<res protocolInfo=\"http-get:*:{mime}:*\">{uri}</res>",
                "</item></DIDL-Lite>"
            ),
            title = title,
            class = class,
            mime = mime,
            uri = escape(uri)
        )
    }
}

fn parse_res(xml: &str) -> (Option<String>, Option<String>) {
    let lower = xml.to_ascii_lowercase();
    let start = match lower.find("<res") {
        Some(i) => i,
        None => return (None, None),
    };
    let after = &xml[start..];
    let gt = match after.find('>') {
        Some(i) => i,
        None => return (None, None),
    };
    let open_tag = &after[..=gt];
    let rest = &after[gt + 1..];
    let close_at = rest
        .to_ascii_lowercase()
        .find("</res>")
        .unwrap_or(rest.len());
    let uri = rest[..close_at].trim();
    let mime = attr(open_tag, "protocolInfo").and_then(|info| {
        // http-get:*:image/jpeg:DLNA.ORG_PN=JPEG_LRG
        info.split(':').nth(2).map(|s| s.trim().to_string())
    });
    let uri = if uri.is_empty() {
        None
    } else {
        Some(unescape(uri))
    };
    (uri, mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_escaped_didl_photo() {
        let raw = r#"&lt;DIDL-Lite&gt;&lt;item&gt;&lt;dc:title&gt;IMG_1.jpg&lt;/dc:title&gt;&lt;res protocolInfo=&quot;http-get:*:image/jpeg:*&quot;&gt;http://192.168.1.8:8080/p.jpg&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;"#;
        let item = DidlItem::parse(raw);
        assert_eq!(item.title.as_deref(), Some("IMG_1.jpg"));
        assert_eq!(item.mime.as_deref(), Some("image/jpeg"));
        assert_eq!(item.uri.as_deref(), Some("http://192.168.1.8:8080/p.jpg"));
        assert_eq!(item.kind(""), MediaKind::Image);
    }

    #[test]
    fn builds_photo_didl() {
        let xml = DidlItem {
            title: Some("IMG.jpg".into()),
            mime: Some("image/jpeg".into()),
            uri: Some("http://10.0.0.2/p.jpg".into()),
        }
        .to_xml();
        assert!(xml.contains("object.item.imageItem.photo"));
        assert!(xml.contains("http://10.0.0.2/p.jpg"));
        assert_eq!(DidlItem::parse(&xml).title.as_deref(), Some("IMG.jpg"));
    }
}
