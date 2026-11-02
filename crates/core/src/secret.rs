use std::sync::Mutex;

static PASS: Mutex<Option<String>> = Mutex::new(None);

/// Set the shared LAN passphrase. Empty disables encryption.
///
/// The same value must be set on the sender and the receiver. Discovery and
/// ordinary DLNA stay unchanged while this is empty.
pub fn set_passphrase(passphrase: Option<String>) {
    let cleaned = passphrase
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Ok(mut slot) = PASS.lock() {
        *slot = cleaned;
    }
}

pub fn passphrase() -> Option<String> {
    PASS.lock().ok().and_then(|g| g.clone())
}
