# castify

castify is a Rust library for sending and receiving media on a local network with DLNA/UPnP. It can run inside a Rust program, or other languages can load `libcastify` through the C ABI. An Android build exposes the same API with JNI.

The library advertises a receiver, discovers receivers, hosts a file, and exchanges the play URL. The host application opens that URL and plays the image, audio, video, or web page.

## What it can do

### Receive

Start a MediaRenderer with a friendly name and a stable UUID. Controllers on the same network can find it and play a URL to it.

The host receives these events:

| `kind` | Meaning | Fields |
|---|---|---|
| 1 | Ready | `uri` is the device description URL, `title` is the friendly name |
| 2 | Play | `uri`, `mime`, `title`. `media_kind` is 0 unknown, 1 image, 2 video, 3 audio, 4 web |
| 3 | Pause | |
| 4 | Stop | |
| 5 | Volume | `uri` is the percent from 0 to 100, `media_kind` is 1 when muted |
| 6 | Error | `uri` is the message |

A play event carries the URL the host should open. The MIME type and the URL extension choose the kind: image, video (including HLS `.m3u8` and DASH `.mpd`), audio, or a web page (`text/html`, `application/xhtml+xml`, or `text/x-castify-web`). An `http` or `https` URL with none of those media types is reported as a web page.

Pause, stop, and volume are reported to the host. The host applies them to its own player. `Seek` is accepted on the wire and left to that player.

When media was encrypted for this library, the play URL is a localhost proxy that serves the decrypted bytes. The host still opens a normal HTTP URL.

### Send

Search for MediaRenderers, then tell one of them to play.

- `search` uses SSDP and returns each renderer's friendly name, UUID, description URL, AVTransport control URL, and whether its model name is `castify`.
- A local file can be hosted on the LAN. The server binds an ephemeral port, advertises a LAN IPv4 address, and accepts HTTP `Range` requests so a player can seek.
- An existing `http://` or `https://` URL can be played directly, with no local file server.
- Play sends `SetAVTransportURI` and `Play`. Stop sends `Stop`.

Search waits at least 500 ms and at most 15000 ms. A search with no answers returns an empty JSON array (`"[]"`).

### Encryption

Encryption is off until the sender and the receiver share a passphrase. With no passphrase, a normal DLNA controller can play to this renderer, and this sender can play to any MediaRenderer.

With a passphrase:

- The passphrase is stretched with PBKDF2-HMAC-SHA256.
- Local files and clear `http://` URLs are encrypted with AES-256-GCM.
- A receiver that has a passphrase accepts an encrypted URL or `https://` and refuses a clear `http://` cast.
- `https://` stays a direct TLS fetch. With a passphrase set, the sender offers that URL only to a renderer whose `modelName` is `castify`.
- Encrypted media is offered only to a renderer whose `modelName` is `castify`. `castify_sender_play` and `nativePlay` return `-2` for any other renderer.
- SSDP, the device description, SOAP, the media URL, and the title travel in the clear.
- A missing or different passphrase fails that cast.

## What to keep in mind

- Save one UUID per device and pass it on every start. In C, a null `uuid` generates one for that process. An empty string is kept as-is, including from JNI.
- `bind_ip` may be null or empty. The library then picks a local address.
- One renderer and one file server run at a time. Starting another stops the previous one.
- Strings returned by search and serve stay valid until `castify_string_free`. Pointers inside a callback are valid only for that call.
- Integer C functions return `0` on success and `-1` on failure, except the `-2` case above.
- The receiver listens for SSDP on UDP port 1900 and multicast `239.255.255.250`. The host allows that traffic on the local network.

## Layout

| Crate | Role |
|---|---|
| `crates/core` | SSDP, device description, SOAP AVTransport, file server, optional encryption |
| `crates/ffi` | `libcastify` C ABI and Android JNI |
| `crates/cli` | Desktop receiver that prints events |

## Desktop receiver

```bash
cargo run -p castify -- "Living Room"
```

The argument is the friendly name on the network. The default name is `castify`. The process prints `ready`, then `PLAY`, `PAUSE`, `STOP`, and `VOLUME` as controllers send them. Stop it with Ctrl-C.

```bash
CASTIFY_PASSPHRASE='shared secret' cargo run -p castify -- "Living Room"
```

Set `CASTIFY_PASSPHRASE` when this receiver should decrypt media from a castify sender that uses the same passphrase.

## Android library

`scripts/build-android.sh` cross-compiles `libcastify.so` for `arm64-v8a`, `armeabi-v7a`, and `x86_64`. The optional argument is the output directory.

```bash
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/<version>"
./scripts/build-android.sh /path/to/jniLibs
```

Load `libcastify.so` and call the C functions or the JNI methods below.

## How to use

### Rust

```toml
castify-core = { git = "https://github.com/mingminghome/castify", tag = "v0.1.0" }
```

```rust
use std::time::Duration;

use castify_core::{
    new_uuid, search_renderers, sender_play, sender_stop, set_passphrase, FileServer,
    MediaRenderer, ReceiverEvent, RendererConfig,
};

fn main() -> std::io::Result<()> {
    // Omit this call, or pass None, to leave encryption off.
    set_passphrase(Some("shared secret".into()));

    let renderer = MediaRenderer::start(
        RendererConfig::new("Living Room", new_uuid()),
        |event| {
            if let ReceiverEvent::Play(item) = event {
                println!("{}", item.uri);
            }
        },
    )?;

    let devices = search_renderers(Duration::from_secs(3))?;
    if let Some(device) = devices.first() {
        let file = FileServer::start("clip.mp4", "video/mp4")?;
        sender_play(&device.control_url, &file.public_url, "clip", "video/mp4")?;
        // `file` keeps serving until `file.stop()` or drop.
        sender_stop(&device.control_url)?;
    }

    renderer.stop();
    Ok(())
}
```

`FileServer::start` serves the file as plaintext. `sender_play` wraps an `http://` URL when a passphrase is set. `FileServer::start_with(path, mime, Some(passphrase))` encrypts the file server itself. An existing `http://` or `https://` URL can be passed to `sender_play` with no `FileServer`.

`new_uuid()` makes a new id. Store the result and pass that same string to `RendererConfig::new` on later starts.

### C

Link `libcastify`. C++, Python (`ctypes` or `cffi`), .NET (`P/Invoke`), Go, and Swift can call these functions. This repository ships the C ABI and the Android JNI layer.

```c
typedef struct CastifyEvent {
    int kind;
    int media_kind;
    const char *uri;
    const char *mime;
    const char *title;
} CastifyEvent;

int castify_receiver_start(const char *friendly_name, const char *uuid, const char *bind_ip);
void castify_receiver_stop(void);
void castify_receiver_set_callback(void (*cb)(const CastifyEvent *ev, void *user), void *user);
void castify_set_passphrase(const char *passphrase);

char *castify_sender_search(int timeout_ms);
int castify_sender_play(const char *control_url, const char *media_url,
                        const char *title, const char *mime);
int castify_sender_stop_play(const char *control_url);
char *castify_sender_serve(const char *file_path, const char *mime);
void castify_sender_serve_stop(void);
void castify_string_free(char *p);
```

```c
static void on_event(const CastifyEvent *ev, void *user) {
    if (ev->kind == 2 && ev->uri) play_url(ev->uri);
}

castify_set_passphrase(NULL); /* or a shared passphrase; "" also turns encryption off */
castify_receiver_set_callback(on_event, NULL);
castify_receiver_start("Living Room", saved_uuid, NULL);

char *json = castify_sender_search(3000);
/* [{"name","uuid","location","controlUrl","castify"}] */
char *url = castify_sender_serve("/path/clip.mp4", "video/mp4");
castify_sender_play(control_url, url, "clip", "video/mp4");
castify_sender_stop_play(control_url);
castify_sender_serve_stop();
castify_string_free(url);
castify_string_free(json);
castify_receiver_stop();
```

`castify_sender_serve` uses the current passphrase. Call `castify_set_passphrase` before serve and play. `castify_sender_serve_stop` also stops an encrypted relay started by play.

### Android JNI

The Android library calls `com.mmhw.csvtv.receiver.NativeReceiver.onNativeEvent`. Use that package and those class names. Kotlin `object` methods map to the static methods below.

```java
package com.mmhw.csvtv.receiver;

public final class NativeReceiver {
    static { System.loadLibrary("castify"); }

    public static native int nativeStart(String name, String uuid, String bindIp);
    public static native void nativeStop();
    public static native void nativeSetPassphrase(String passphrase);

    static void onNativeEvent(int kind, int mediaKind, String uri, String mime, String title) {
        if (kind == 2) play(uri);
    }
}

public final class NativeSender {
    public static native String nativeSearch(int timeoutMs);
    public static native int nativePlay(String controlUrl, String mediaUrl, String title, String mime);
    public static native int nativeStopPlay(String controlUrl);
    public static native String nativeServe(String filePath, String mime);
    public static native void nativeServeStop();
}
```

```java
NativeReceiver.nativeSetPassphrase(""); // or the shared passphrase
NativeReceiver.nativeStart("Living Room", savedUuid, "");
String json = NativeSender.nativeSearch(3000);
String url = NativeSender.nativeServe("/path/clip.mp4", "video/mp4");
NativeSender.nativePlay(controlUrl, url, "clip", "video/mp4");
```

`nativeSearch` and `nativePlay` block, so call them off the UI thread. `nativeSearch` returns a JSON array of `name`, `uuid`, `location`, `controlUrl`, and `castify`.

## Protocol

The receiver advertises `urn:schemas-upnp-org:device:MediaRenderer:1` on `239.255.255.250:1900`. It answers AVTransport (`SetAVTransportURI`, `Play`, `Pause`, `Stop`), RenderingControl volume and mute, and ConnectionManager protocol queries. The device description is served at `/device.xml`. Manufacturer and model name are `castify`.

MIT licensed.
