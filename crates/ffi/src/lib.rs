//! C ABI plus Android JNI for [`castify_core`].

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::Mutex;

use castify_core::{
    renderers_to_json, search_renderers, sender_play, sender_stop, set_passphrase, FileServer,
    MediaKind, MediaRenderer, ReceivedItem, ReceiverEvent, RendererConfig,
};

struct Session {
    renderer: MediaRenderer,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

#[repr(C)]
pub struct CastifyEvent {
    pub kind: c_int,
    pub media_kind: c_int,
    pub uri: *const c_char,
    pub mime: *const c_char,
    pub title: *const c_char,
}

pub const CASTIFY_EVENT_READY: c_int = 1;
pub const CASTIFY_EVENT_PLAY: c_int = 2;
pub const CASTIFY_EVENT_PAUSE: c_int = 3;
pub const CASTIFY_EVENT_STOP: c_int = 4;
pub const CASTIFY_EVENT_VOLUME: c_int = 5;
pub const CASTIFY_EVENT_ERROR: c_int = 6;

pub type CastifyCallback = Option<extern "C" fn(*const CastifyEvent, *mut std::ffi::c_void)>;

static CALLBACK: Mutex<Option<(CastifyCallback, usize)>> = Mutex::new(None);

fn cptr(held: &mut Vec<CString>, s: &str) -> *const c_char {
    match CString::new(s) {
        Ok(c) => {
            let p = c.as_ptr();
            held.push(c);
            p
        }
        Err(_) => std::ptr::null(),
    }
}

fn dispatch(event: ReceiverEvent) {
    let cb = match CALLBACK.lock() {
        Ok(g) => *g,
        Err(_) => return,
    };
    let Some((Some(func), user)) = cb else {
        return;
    };
    let mut held = Vec::new();
    let (kind, media_kind, uri, mime, title) = match &event {
        ReceiverEvent::Ready {
            location,
            friendly_name,
        } => (
            CASTIFY_EVENT_READY,
            0,
            cptr(&mut held, location),
            std::ptr::null(),
            cptr(&mut held, friendly_name),
        ),
        ReceiverEvent::Play(ReceivedItem {
            uri,
            mime,
            title,
            kind,
        }) => (
            CASTIFY_EVENT_PLAY,
            match kind {
                MediaKind::Image => 1,
                MediaKind::Video => 2,
                MediaKind::Audio => 3,
                MediaKind::Web => 4,
                MediaKind::Unknown => 0,
            },
            cptr(&mut held, uri),
            mime.as_deref()
                .map(|m| cptr(&mut held, m))
                .unwrap_or(std::ptr::null()),
            title
                .as_deref()
                .map(|t| cptr(&mut held, t))
                .unwrap_or(std::ptr::null()),
        ),
        ReceiverEvent::Pause => (
            CASTIFY_EVENT_PAUSE,
            0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        ),
        ReceiverEvent::Stop => (
            CASTIFY_EVENT_STOP,
            0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        ),
        ReceiverEvent::Volume { percent, muted } => (
            CASTIFY_EVENT_VOLUME,
            if *muted { 1 } else { 0 },
            cptr(&mut held, &percent.to_string()),
            std::ptr::null(),
            std::ptr::null(),
        ),
        ReceiverEvent::Error(msg) => (
            CASTIFY_EVENT_ERROR,
            0,
            cptr(&mut held, msg),
            std::ptr::null(),
            std::ptr::null(),
        ),
    };
    let payload = CastifyEvent {
        kind,
        media_kind,
        uri,
        mime,
        title,
    };
    func(&payload, user as *mut _);
    drop(held);
}

/// Start the DLNA renderer. Returns 0 on success.
#[no_mangle]
pub extern "C" fn castify_receiver_start(
    friendly_name: *const c_char,
    uuid: *const c_char,
    bind_ip: *const c_char,
) -> c_int {
    let name = unsafe { cstr(friendly_name) }.unwrap_or_else(|| "castify".into());
    let uuid = unsafe { cstr(uuid) }.unwrap_or_else(castify_core::new_uuid);
    let mut cfg = RendererConfig::new(name, uuid);
    if let Some(ip) = unsafe { cstr(bind_ip) } {
        if !ip.is_empty() {
            cfg.bind_ip = Some(ip);
        }
    }
    match MediaRenderer::start(cfg, dispatch) {
        Ok(renderer) => {
            if let Ok(mut slot) = SESSION.lock() {
                if let Some(prev) = slot.take() {
                    prev.renderer.stop();
                }
                *slot = Some(Session { renderer });
            }
            0
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub extern "C" fn castify_receiver_stop() {
    if let Ok(mut slot) = SESSION.lock() {
        if let Some(session) = slot.take() {
            session.renderer.stop();
        }
    }
}

#[no_mangle]
pub extern "C" fn castify_set_passphrase(passphrase: *const c_char) {
    let text = unsafe { cstr(passphrase) }.unwrap_or_default();
    let text = text.trim().to_string();
    if text.is_empty() {
        set_passphrase(None);
    } else {
        set_passphrase(Some(text));
    }
}

#[no_mangle]
pub extern "C" fn castify_receiver_set_callback(cb: CastifyCallback, user: *mut std::ffi::c_void) {
    if let Ok(mut g) = CALLBACK.lock() {
        *g = Some((cb, user as usize));
    }
}

static SERVE: Mutex<Option<FileServer>> = Mutex::new(None);

fn heap_string(s: String) -> *mut c_char {
    match CString::new(s.replace('\0', "")) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free a string returned by search/serve.
#[no_mangle]
pub extern "C" fn castify_string_free(p: *mut c_char) {
    if !p.is_null() {
        unsafe {
            drop(CString::from_raw(p));
        }
    }
}

/// SSDP search. Returns a JSON array; caller frees with castify_string_free.
#[no_mangle]
pub extern "C" fn castify_sender_search(timeout_ms: c_int) -> *mut c_char {
    let ms = timeout_ms.clamp(500, 15_000) as u64;
    match search_renderers(std::time::Duration::from_millis(ms)) {
        Ok(list) => heap_string(renderers_to_json(&list)),
        Err(_) => heap_string("[]".into()),
    }
}

/// SetAVTransportURI + Play. Returns 0 on success.
#[no_mangle]
pub extern "C" fn castify_sender_play(
    control_url: *const c_char,
    media_url: *const c_char,
    title: *const c_char,
    mime: *const c_char,
) -> c_int {
    let control = unsafe { cstr(control_url) }.unwrap_or_default();
    let media = unsafe { cstr(media_url) }.unwrap_or_default();
    if control.is_empty() || media.is_empty() {
        return -1;
    }
    let title = unsafe { cstr(title) }.unwrap_or_else(|| "Photo".into());
    let mime = unsafe { cstr(mime) }.unwrap_or_else(|| "image/jpeg".into());
    match sender_play(&control, &media, &title, &mime) {
        Ok(()) => 0,
        Err(err) if err.to_string().contains("not castify") => -2,
        Err(_) => -1,
    }
}

#[no_mangle]
pub extern "C" fn castify_sender_stop_play(control_url: *const c_char) -> c_int {
    let control = unsafe { cstr(control_url) }.unwrap_or_default();
    if control.is_empty() {
        return -1;
    }
    match sender_stop(&control) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Serve a local file on the LAN. Returns the public HTTP URL.
#[no_mangle]
pub extern "C" fn castify_sender_serve(file_path: *const c_char, mime: *const c_char) -> *mut c_char {
    let path = unsafe { cstr(file_path) }.unwrap_or_default();
    let mime = unsafe { cstr(mime) }.unwrap_or_else(|| "application/octet-stream".into());
    let pass = castify_core::passphrase();
    match FileServer::start_with(path, mime, pass.as_deref()) {
        Ok(server) => {
            let url = server.public_url.clone();
            if let Ok(mut slot) = SERVE.lock() {
                if let Some(prev) = slot.take() {
                    prev.stop();
                }
                *slot = Some(server);
            }
            heap_string(url)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn castify_sender_serve_stop() {
    if let Ok(mut slot) = SERVE.lock() {
        if let Some(server) = slot.take() {
            server.stop();
        }
    }
    castify_core::stop_relay();
}

unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(|s| s.to_string())
}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
    use jni::sys::{jint, JNI_VERSION_1_6};
    use jni::{JNIEnv, JavaVM};
    use std::sync::Mutex;

    use super::{
        castify_receiver_start, castify_receiver_stop, castify_sender_play, castify_sender_search,
        castify_sender_serve, castify_sender_serve_stop, castify_sender_stop_play, castify_set_passphrase,
        castify_string_free,
    };

    static JVM: Mutex<Option<JavaVM>> = Mutex::new(None);
    static NATIVE_CLASS: Mutex<Option<GlobalRef>> = Mutex::new(None);

    #[no_mangle]
    pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
        android_logger::init_once(
            android_logger::Config::default().with_max_level(log::LevelFilter::Info),
        );
        let mut env = match vm.get_env() {
            Ok(e) => e,
            Err(_) => return JNI_VERSION_1_6,
        };
        if let Ok(class) = env.find_class("com/mmhw/csvtv/receiver/NativeReceiver") {
            if let Ok(gref) = env.new_global_ref(&class) {
                *NATIVE_CLASS.lock().unwrap() = Some(gref);
            }
        } else {
            let _ = env.exception_clear();
        }
        *JVM.lock().unwrap() = Some(vm);
        JNI_VERSION_1_6
    }

    extern "C" fn jni_bridge(ev: *const super::CastifyEvent, _user: *mut std::ffi::c_void) {
        if ev.is_null() {
            return;
        }
        let ev = unsafe { &*ev };
        let vm_guard = match JVM.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let vm = match vm_guard.as_ref() {
            Some(v) => v,
            None => return,
        };
        let mut env = match vm.attach_current_thread() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class_guard = match NATIVE_CLASS.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(class_ref) = class_guard.as_ref() else {
            return;
        };
        let uri = jstring(&mut env, ev.uri);
        let mime = jstring(&mut env, ev.mime);
        let title = jstring(&mut env, ev.title);
        let result = env.call_static_method(
            class_ref,
            "onNativeEvent",
            "(IILjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Int(ev.kind),
                JValue::Int(ev.media_kind),
                JValue::Object(&uri),
                JValue::Object(&mime),
                JValue::Object(&title),
            ],
        );
        if result.is_err() {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
    }

    fn cstring(s: String) -> std::ffi::CString {
        std::ffi::CString::new(s.replace('\0', "")).unwrap_or_else(|_| std::ffi::CString::new("").unwrap())
    }

    fn jstring<'a>(env: &mut JNIEnv<'a>, p: *const std::os::raw::c_char) -> JObject<'a> {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { std::ffi::CStr::from_ptr(p).to_str().unwrap_or("") }
        };
        match env.new_string(s) {
            Ok(js) => JObject::from(js),
            Err(_) => {
                let _ = env.exception_clear();
                JObject::null()
            }
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeReceiver_nativeStart(
        mut env: JNIEnv,
        _class: JClass,
        name: JString,
        uuid: JString,
        bind_ip: JString,
    ) -> jint {
        if JVM.lock().ok().and_then(|g| g.as_ref().map(|_| ())).is_none() {
            if let Ok(vm) = env.get_java_vm() {
                *JVM.lock().unwrap() = Some(vm);
            }
        }
        super::castify_receiver_set_callback(Some(jni_bridge), std::ptr::null_mut());
        let name = env
            .get_string(&name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let uuid = env
            .get_string(&uuid)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bind = env
            .get_string(&bind_ip)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name_c = cstring(name);
        let uuid_c = cstring(uuid);
        let bind_c = cstring(bind);
        castify_receiver_start(name_c.as_ptr(), uuid_c.as_ptr(), bind_c.as_ptr())
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeReceiver_nativeStop(
        _env: JNIEnv,
        _class: JClass,
    ) {
        castify_receiver_stop();
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeReceiver_nativeSetPassphrase(
        mut env: JNIEnv,
        _class: JClass,
        passphrase: JString,
    ) {
        let text = jni_string(&mut env, &passphrase);
        let c = cstring(text);
        castify_set_passphrase(c.as_ptr());
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeSender_nativeSearch(
        env: JNIEnv,
        _class: JClass,
        timeout_ms: jint,
    ) -> jni::sys::jstring {
        let ptr = castify_sender_search(timeout_ms);
        let json = if ptr.is_null() {
            "[]".to_string()
        } else {
            let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_str()
                .unwrap_or("[]")
                .to_string();
            castify_string_free(ptr);
            s
        };
        match env.new_string(json) {
            Ok(js) => js.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeSender_nativePlay(
        mut env: JNIEnv,
        _class: JClass,
        control_url: JString,
        media_url: JString,
        title: JString,
        mime: JString,
    ) -> jint {
        let control = jni_string(&mut env, &control_url);
        let media = jni_string(&mut env, &media_url);
        let title = jni_string(&mut env, &title);
        let mime = jni_string(&mut env, &mime);
        let c = cstring(control);
        let m = cstring(media);
        let t = cstring(title);
        let mi = cstring(mime);
        castify_sender_play(c.as_ptr(), m.as_ptr(), t.as_ptr(), mi.as_ptr())
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeSender_nativeStopPlay(
        mut env: JNIEnv,
        _class: JClass,
        control_url: JString,
    ) -> jint {
        let control = jni_string(&mut env, &control_url);
        let c = cstring(control);
        castify_sender_stop_play(c.as_ptr())
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeSender_nativeServe(
        mut env: JNIEnv,
        _class: JClass,
        file_path: JString,
        mime: JString,
    ) -> jni::sys::jstring {
        let path = jni_string(&mut env, &file_path);
        let mime = jni_string(&mut env, &mime);
        let p = cstring(path);
        let m = cstring(mime);
        let ptr = castify_sender_serve(p.as_ptr(), m.as_ptr());
        if ptr.is_null() {
            return std::ptr::null_mut();
        }
        let url = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .unwrap_or("")
            .to_string();
        castify_string_free(ptr);
        match env.new_string(url) {
            Ok(js) => js.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_mmhw_csvtv_receiver_NativeSender_nativeServeStop(
        _env: JNIEnv,
        _class: JClass,
    ) {
        castify_sender_serve_stop();
    }

    fn jni_string(env: &mut JNIEnv, value: &JString) -> String {
        env.get_string(value)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}
