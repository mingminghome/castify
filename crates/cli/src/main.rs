use std::io::{self, Write};
use std::sync::mpsc;
use std::time::Duration;

use castify_core::{new_uuid, set_passphrase, MediaRenderer, ReceiverEvent, RendererConfig};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "castify".to_string());
    let uuid = new_uuid();
    if let Ok(pass) = std::env::var("CASTIFY_PASSPHRASE") {
        let pass = pass.trim().to_string();
        if !pass.is_empty() {
            set_passphrase(Some(pass));
        }
    }

    let cfg = RendererConfig::new(name, uuid);

    let (tx, rx) = mpsc::channel();
    let renderer = MediaRenderer::start(cfg, move |ev| {
        let _ = tx.send(ev);
    })
    .expect("start DLNA renderer");

    eprintln!("castify listening. Ctrl-C to stop.");
    eprintln!("A DLNA controller can play a photo, audio clip, video, or web URL to this name.");

    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ReceiverEvent::Ready {
                location,
                friendly_name,
            }) => {
                eprintln!("ready  name={friendly_name}  {location}");
            }
            Ok(ReceiverEvent::Play(item)) => {
                println!(
                    "PLAY kind={:?} mime={} title={} uri={}",
                    item.kind,
                    item.mime.as_deref().unwrap_or("-"),
                    item.title.as_deref().unwrap_or("-"),
                    item.uri
                );
                let _ = io::stdout().flush();
            }
            Ok(ReceiverEvent::Pause) => eprintln!("PAUSE"),
            Ok(ReceiverEvent::Stop) => eprintln!("STOP"),
            Ok(ReceiverEvent::Volume { percent, muted }) => {
                eprintln!("VOLUME {percent}% muted={muted}");
            }
            Ok(ReceiverEvent::Error(msg)) => eprintln!("error: {msg}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    renderer.stop();
}
