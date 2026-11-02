//! castify sends and receives media on the local network over DLNA/UPnP.
//!
//! A receiver advertises a MediaRenderer and reports play, pause, stop, and
//! volume to the host. A sender discovers renderers, can host a local file,
//! and asks a renderer to play an image, audio clip, video, or web URL.
//! The host opens that URL and plays it.

mod control;
mod crypto;
mod device;
mod didl;
mod discover;
mod guard;
mod http;
mod media;
mod net;
mod renderer;
mod secret;
mod serve;
mod soap;
mod ssdp;
mod xml;

pub use control::{play as sender_play, stop as sender_stop};
pub use guard::stop_relay;
pub use secret::{passphrase, set_passphrase};
pub use device::DeviceIdentity;
pub use discover::{
    fetch_renderer, parse_device_xml, renderers_to_json, search_renderers, DiscoveredRenderer,
};
pub use media::{MediaKind, ReceivedItem, ReceiverEvent};
pub use net::local_ipv4;
pub use renderer::{MediaRenderer, RendererConfig};
pub use serve::FileServer;

/// Stable UUID string (no `uuid:` prefix). Host should persist this.
pub fn new_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    format!(
        "6ba7b810-9dad-11d1-80b4-{:012x}",
        nanos & 0xffff_ffff_ffff
    )
}
