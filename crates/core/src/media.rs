/// Kind of media a controller asked us to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Web,
    Unknown,
}

impl MediaKind {
    pub fn from_mime_and_uri(mime: Option<&str>, uri: &str) -> Self {
        if let Some(kind) = mime.and_then(Self::from_mime) {
            return kind;
        }
        Self::from_uri(uri)
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        let m = mime.trim().to_ascii_lowercase();
        let base = m.split(';').next().unwrap_or(&m).trim();
        if base.starts_with("image/") {
            return Some(Self::Image);
        }
        if base.starts_with("video/")
            || base.contains("mpegurl")
            || base == "application/dash+xml"
            || base == "application/vnd.apple.mpegurl"
            || base == "application/x-mpegurl"
        {
            return Some(Self::Video);
        }
        if base.starts_with("audio/") {
            return Some(Self::Audio);
        }
        if base == "text/html"
            || base == "application/xhtml+xml"
            || base == "text/x-castify-web"
            || base == "text/x-castlib-web"
            || base == "text/x-csvtv-web"
        {
            return Some(Self::Web);
        }
        None
    }

    pub fn from_uri(uri: &str) -> Self {
        let path = uri
            .split('?')
            .next()
            .unwrap_or(uri)
            .split('#')
            .next()
            .unwrap_or(uri)
            .to_ascii_lowercase();
        const IMAGES: &[&str] = &[
            ".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".heic", ".heif", ".avif",
        ];
        const VIDEOS: &[&str] = &[
            ".mp4", ".mkv", ".webm", ".mov", ".m3u8", ".mpd", ".ts", ".avi", ".m4v",
        ];
        const AUDIOS: &[&str] = &[".mp3", ".m4a", ".aac", ".flac", ".ogg", ".wav", ".wma"];
        if IMAGES.iter().any(|e| path.ends_with(e)) {
            return Self::Image;
        }
        if VIDEOS.iter().any(|e| path.ends_with(e)) {
            return Self::Video;
        }
        if AUDIOS.iter().any(|e| path.ends_with(e)) {
            return Self::Audio;
        }
        if path.starts_with("http://") || path.starts_with("https://") {
            return Self::Web;
        }
        Self::Unknown
    }
}

/// Payload delivered to the host app when a controller plays something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedItem {
    pub uri: String,
    pub mime: Option<String>,
    pub title: Option<String>,
    pub kind: MediaKind,
}

/// Events the renderer emits to the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverEvent {
    Ready {
        location: String,
        friendly_name: String,
    },
    Play(ReceivedItem),
    Pause,
    Stop,
    Volume {
        percent: u8,
        muted: bool,
    },
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_photo_and_hls() {
        assert_eq!(
            MediaKind::from_mime_and_uri(Some("image/jpeg"), "http://x/a"),
            MediaKind::Image
        );
        assert_eq!(
            MediaKind::from_mime_and_uri(None, "http://x/clip.m3u8?token=1"),
            MediaKind::Video
        );
        assert_eq!(
            MediaKind::from_mime_and_uri(None, "http://x/song.mp3"),
            MediaKind::Audio
        );
        assert_eq!(
            MediaKind::from_mime_and_uri(Some("text/html"), "https://example.com/news"),
            MediaKind::Web
        );
    }
}
