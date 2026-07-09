use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::oneshot;

// Set from the JNI thread when the hardware/gesture back is pressed; drained on the
// runtime by take_back_pressed(). Decoupled because dioxus signals can only be touched
// from the runtime thread, not the JNI thread.
static BACK_PENDING: AtomicBool = AtomicBool::new(false);

/// Returns true once per back press, clearing the pending flag.
pub fn take_back_pressed() -> bool {
    BACK_PENDING.swap(false, Ordering::SeqCst)
}

#[derive(Debug, Clone, Copy)]
pub enum SystemEvent {
    Play,
    Pause,
    Toggle,
    Next,
    Prev,
    Stop,
}

static JVM: OnceLock<JavaVM> = OnceLock::new();
// App classloader cached from main thread so FindClass works from any thread.
static CLASSLOADER: OnceLock<GlobalRef> = OnceLock::new();
static BACKGROUND_HANDLER: OnceLock<Arc<Mutex<Option<Box<dyn Fn(SystemEvent) + Send + Sync>>>>> =
    OnceLock::new();

fn get_bg_handler() -> Arc<Mutex<Option<Box<dyn Fn(SystemEvent) + Send + Sync>>>> {
    BACKGROUND_HANDLER
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

pub fn set_background_handler(handler: impl Fn(SystemEvent) + Send + Sync + 'static) {
    let binding = get_bg_handler();
    let mut guard = binding.lock().unwrap();
    *guard = Some(Box::new(handler));
}

fn dispatch_event(event: SystemEvent) {
    if let Ok(guard) = get_bg_handler().lock() {
        if let Some(ref handler) = *guard {
            handler(event);
        }
    }
}

pub fn init() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let ctx = ndk_context::android_context();
        let vm_ptr = ctx.vm();
        if vm_ptr.is_null() {
            return;
        }
        match unsafe { JavaVM::from_raw(vm_ptr.cast()) } {
            Ok(vm) => {
                let _ = JVM.set(vm);
                cache_classloader();
                // Legacy MediaSessionHelper is unused now that playback runs through
                // the Media3 PlaybackService — a second active MediaSession fights it
                // for the notification/lock-screen, so don't create it.
                // init_media_session();
            }
            Err(e) => eprintln!("[android] Failed to capture JVM: {}", e),
        }
    });
}

// Cache the app classloader from the activity so FindClass works from background threads.
fn cache_classloader() {
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let ctx = ndk_context::android_context();
    let raw = ctx.context();
    if raw.is_null() {
        eprintln!("[android] null activity context; skipping classloader cache");
        return;
    }
    // Transient local only — we immediately turn the resolved classloader into a
    // GlobalRef below and never retain this raw activity pointer.
    let activity = unsafe { JObject::from_raw(raw.cast()) };
    let result: Result<(), jni::errors::Error> = (|| {
        let cl = env
            .call_method(
                &activity,
                "getClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )?
            .l()?;
        let global = env.new_global_ref(&cl)?;
        let _ = CLASSLOADER.set(global);
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] Failed to cache classloader: {}", e);
    }
}

// Resolve an app class using the cached classloader, falling back to FindClass.
fn find_app_class<'a>(env: &mut JNIEnv<'a>, name: &str) -> Result<JClass<'a>, jni::errors::Error> {
    if let Some(cl) = CLASSLOADER.get() {
        let dot_name = env.new_string(name.replace('/', "."))?;
        let class_obj = env
            .call_method(
                cl.as_obj(),
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(&dot_name)],
            )?
            .l()?;
        Ok(JClass::from(class_obj))
    } else {
        env.find_class(name)
    }
}

#[allow(dead_code)] // superseded by the Media3 PlaybackService; kept for reference.
fn init_media_session() {
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[android] attach_current_thread failed: {}", e);
            return;
        }
    };
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result: Result<(), jni::errors::Error> = (|| {
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/MediaSessionHelper")?;
        env.call_static_method(
            &class,
            "init",
            "(Landroid/content/Context;)V",
            &[JValue::Object(&activity)],
        )?
        .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] MediaSessionHelper.init failed: {}", e);
        clear_jni_exception(&mut env);
    }
}

fn dir_via_jni(method: &str) -> Option<String> {
    init();
    let vm = JVM.get()?;
    let mut env = vm.attach_current_thread().ok()?;
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let r: Result<String, jni::errors::Error> = (|| {
        let file = env
            .call_method(&activity, method, "()Ljava/io/File;", &[])?
            .l()?;
        let path = env
            .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        Ok(env.get_string(&JString::from(path))?.into())
    })();
    r.map_err(|_| {
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
    })
    .ok()
}

pub fn get_files_dir() -> Option<String> {
    dir_via_jni("getFilesDir").or_else(|| {
        std::env::var("FILES_DIR").ok().or_else(|| {
            let home = std::env::var("HOME").ok()?;
            if home.contains("com.temidaradev.kopuz") {
                Some(format!("{}/files", home))
            } else {
                None
            }
        })
    })
}

pub fn get_android_music_dir() -> Option<String> {
    init();
    let vm = JVM.get()?;
    let mut env = vm.attach_current_thread().ok()?;
    let result: Result<String, jni::errors::Error> = (|env: &mut JNIEnv| {
        let env_class = env.find_class("android/os/Environment")?;
        let dir_type = env.new_string("Music")?;
        let file = env
            .call_static_method(
                env_class,
                "getExternalStoragePublicDirectory",
                "(Ljava/lang/String;)Ljava/io/File;",
                &[JValue::Object(&dir_type)],
            )?
            .l()?;
        let path = env
            .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        Ok(env.get_string(&JString::from(path))?.into())
    })(&mut env);
    if let Err(e) = result {
        eprintln!("[android] get_android_music_dir failed: {}", e);
        clear_jni_exception(&mut env);
        None
    } else {
        result.ok()
    }
}

/// Normalises an artwork URL to something Kotlin can consume:
/// - `artwork://local?p=…` → decoded absolute file path
/// - `http(s)://…`         → passed through as-is for Kotlin to download
/// - anything else         → None
fn normalize_artwork(url: &str) -> Option<String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(url.to_string());
    }
    let query = url.strip_prefix("artwork://local?")?;
    let encoded = query.split('&').find_map(|kv| {
        let mut parts = kv.splitn(2, '=');
        if parts.next() == Some("p") {
            parts.next()
        } else {
            None
        }
    })?;
    let decoded = percent_decode(encoded);
    let path = if decoded.starts_with("/~") {
        std::env::var("HOME")
            .ok()
            .map(|h| decoded.replacen("/~", &h, 1))
            .unwrap_or(decoded)
    } else if decoded.starts_with('~') {
        std::env::var("HOME")
            .ok()
            .map(|h| decoded.replacen('~', &h, 1))
            .unwrap_or(decoded)
    } else {
        decoded
    };
    Some(path)
}

/// Cache of the last decoded `data:` artwork: (content hash, written file path).
/// The player re-sends the same artwork every position tick (~1s); without this
/// we'd base64-decode and rewrite the file each tick.
static LAST_DATA_ART: Mutex<Option<(u64, String)>> = Mutex::new(None);

/// Resolve an artwork URL to something `MediaSessionHelper` can load: an http(s)
/// URL, a local file path, or — for the base64 `data:` URLs the Android UI uses —
/// a file decoded into app storage (the notification can't render a data URL).
fn resolve_artwork(url: &str) -> Option<String> {
    let resolved = if url.starts_with("data:") {
        data_url_to_file(url)
    } else if let Some(stripped) = url.strip_prefix("file://") {
        Some(stripped.to_string())
    } else if url.starts_with('/') {
        // Bare absolute path (e.g. a downloaded server cover in the temp dir).
        Some(url.to_string())
    } else {
        normalize_artwork(url)
    };
    eprintln!(
        "[android] resolve_artwork in={} -> {:?}",
        &url[..url.len().min(48)],
        resolved
    );
    resolved
}

/// Decode a `data:<mime>;base64,<payload>` URL to a file under the app's files dir
/// and return its path. Cached by content hash so repeated identical updates reuse
/// the same file instead of rewriting it.
fn data_url_to_file(url: &str) -> Option<String> {
    use base64::{Engine as _, engine::general_purpose};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let meta = url.strip_prefix("data:")?;
    let comma = meta.find(',')?;
    let header = &meta[..comma];
    let payload = &meta[comma + 1..];
    if !header.contains("base64") {
        return None;
    }
    let ext = if header.contains("image/png") {
        "png"
    } else if header.contains("image/webp") {
        "webp"
    } else if header.contains("image/gif") {
        "gif"
    } else {
        "jpg"
    };

    let mut hasher = DefaultHasher::new();
    payload.hash(&mut hasher);
    let hash = hasher.finish();

    // Hash is part of the filename so a new track yields a new path — the Kotlin
    // side caches its decoded bitmap by path and would otherwise keep showing the
    // previous track's art when the filename stayed constant.
    if let Ok(guard) = LAST_DATA_ART.lock() {
        if let Some((last_hash, path)) = guard.as_ref() {
            if *last_hash == hash && std::path::Path::new(path).exists() {
                return Some(path.clone());
            }
        }
    }

    let files_dir = get_files_dir()?;
    let path = format!("{files_dir}/np_art_{hash}.{ext}");
    let bytes = general_purpose::STANDARD.decode(payload).ok()?;
    std::fs::write(&path, &bytes).ok()?;
    if let Ok(mut guard) = LAST_DATA_ART.lock() {
        // Remove the previously written art file so they don't accumulate.
        if let Some((_, old_path)) = guard.as_ref() {
            if old_path != &path {
                let _ = std::fs::remove_file(old_path);
            }
        }
        *guard = Some((hash, path.clone()));
    }
    Some(path)
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.bytes().peekable();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next().map(hex_val).unwrap_or(0);
            let h2 = chars.next().map(hex_val).unwrap_or(0);
            out.push(char::from(h1 << 4 | h2));
        } else if b == b'+' {
            out.push(' ');
        } else {
            out.push(char::from(b));
        }
    }
    out
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

// --- Background playback heartbeat ------------------------------------------
// Android suspends the wry/winit event loop while the activity is Stopped, so
// the Dioxus `use_player_task` driver loop (progress + auto-advance) stops
// ticking in the background — the current song plays out on the native cpal
// thread, but the next one never starts until you reopen the app. A low-rate
// native heartbeat pokes the loop (Looper wake + bg-notify) ~5×/s WHILE PLAYING
// so the driver keeps running and advances the queue in the background. This
// mirrors the macOS CFRunLoopTimer heartbeat; it runs on its own OS thread, so
// it's independent of the suspended event loop (the foreground MusicService
// keeps the process — and this thread — alive). Costs nothing when paused.
static HEARTBEAT_PLAYING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static HEARTBEAT_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn ensure_heartbeat() {
    use std::sync::atomic::Ordering;
    if HEARTBEAT_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("kopuz-bg-heartbeat".into())
        .spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(200));
                if HEARTBEAT_PLAYING.load(Ordering::Relaxed) {
                    super::bg_wake();
                    wake_run_loop();
                }
            }
        });
}

/// Set the play/pause state that gates the background heartbeat. Called from
/// `update_now_playing`, which already fires on every play/pause/track change.
pub fn set_playing_heartbeat(playing: bool) {
    HEARTBEAT_PLAYING.store(playing, std::sync::atomic::Ordering::Relaxed);
    if playing {
        ensure_heartbeat();
    }
}

pub fn update_now_playing(
    title: &str,
    artist: &str,
    album: &str,
    duration: f64,
    position: f64,
    playing: bool,
    artwork_path: Option<&str>,
) {
    init();
    // Keep the background driver loop alive while playing (see heartbeat above).
    set_playing_heartbeat(playing);
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let duration_ms = (duration * 1000.0) as i64;
    let position_ms = (position * 1000.0) as i64;
    let resolved_art = artwork_path.and_then(resolve_artwork);
    let result: Result<(), jni::errors::Error> = (|| {
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/MediaSessionHelper")?;
        let j_title = env.new_string(title)?;
        let j_artist = env.new_string(artist)?;
        let j_album = env.new_string(album)?;
        let null_obj = JObject::null();
        let j_art_owned;
        let j_art: &JObject = if let Some(ref path) = resolved_art {
            j_art_owned = env.new_string(path)?;
            &*j_art_owned
        } else {
            &null_obj
        };
        env.call_static_method(
            &class,
            "updateNowPlaying",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJZLjava/lang/String;)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&j_title),
                JValue::Object(&j_artist),
                JValue::Object(&j_album),
                JValue::Long(duration_ms),
                JValue::Long(position_ms),
                JValue::Bool(playing as u8),
                JValue::Object(j_art),
            ],
        )?
        .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!(
            "[android] MediaSessionHelper.updateNowPlaying failed: {}",
            e
        );
        clear_jni_exception(&mut env);
    }
}

pub fn wake_run_loop() {
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let result: Result<(), jni::errors::Error> = (|| {
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/MediaSessionHelper")?;
        env.call_static_method(&class, "wakeMainThread", "()V", &[])?
            .v()?;
        Ok(())
    })();
    if let Err(_) = result {
        clear_jni_exception(&mut env);
    }
}

pub fn stop_session() {
    // Playback is fully torn down — stop poking the background loop.
    set_playing_heartbeat(false);
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result: Result<(), jni::errors::Error> = (|| {
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/MediaSessionHelper")?;
        env.call_static_method(
            &class,
            "stopSession",
            "(Landroid/content/Context;)V",
            &[JValue::Object(&activity)],
        )?
        .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] MediaSessionHelper.stopSession failed: {}", e);
        clear_jni_exception(&mut env);
    }
}

/// Hand a downloaded APK at `path` to the Android package installer (via the
/// Kotlin `Updater` → FileProvider). The system always shows its own install
/// confirmation — a sideloaded app can't update itself silently. If the user
/// hasn't granted "install unknown apps" yet, `Updater.install` deep-links them
/// to that settings page instead of launching the installer.
pub fn install_apk(path: &str) {
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result: Result<(), jni::errors::Error> = (|| {
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/Updater")?;
        let j_path = env.new_string(path)?;
        env.call_static_method(
            &class,
            "install",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(&activity), JValue::Object(&j_path)],
        )?
        .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] Updater.install failed: {}", e);
        clear_jni_exception(&mut env);
    }
}

pub fn request_permissions() {
    init();
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result: Result<(), jni::errors::Error> = (|env: &mut JNIEnv| {
        let class = find_app_class(env, "com/temidaradev/kopuz/MediaSessionHelper")?;
        env.call_static_method(
            &class,
            "requestPermissions",
            "(Landroid/app/Activity;)V",
            &[JValue::Object(&activity)],
        )?
        .v()?;
        Ok(())
    })(&mut env);
    if let Err(e) = result {
        eprintln!(
            "[android] MediaSessionHelper.requestPermissions failed: {}",
            e
        );
        clear_jni_exception(&mut env);
    }
}

fn clear_jni_exception(env: &mut JNIEnv) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
}

// Called from Kotlin: MediaReceiver.nativeOnAction(String) — routes notification button taps to Rust
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_temidaradev_kopuz_MediaReceiver_nativeOnAction(
    mut env: JNIEnv,
    _class: JClass,
    action: JString,
) {
    let action_str: String = match env.get_string(&action) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    match action_str.as_str() {
        "play" => dispatch_event(SystemEvent::Play),
        "pause" => dispatch_event(SystemEvent::Pause),
        "toggle" => dispatch_event(SystemEvent::Toggle),
        "next" => dispatch_event(SystemEvent::Next),
        "prev" => dispatch_event(SystemEvent::Prev),
        "stop" => dispatch_event(SystemEvent::Stop),
        // Hardware/gesture back — handled by the app router, not a media command.
        "back" => {
            BACK_PENDING.store(true, Ordering::SeqCst);
            super::back_wake();
        }
        _ => {}
    }
}

/// Send the app to the background (like Home) instead of finishing it, so playback
/// survives. Delegates to MainActivity.moveToBack(), which marshals onto the UI thread.
pub fn move_task_to_back() {
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let result: Result<(), jni::errors::Error> = (|| {
        let class = find_app_class(&mut env, "dev/dioxus/main/MainActivity")?;
        env.call_static_method(&class, "moveToBack", "()V", &[])?
            .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] MainActivity.moveToBack failed: {}", e);
        clear_jni_exception(&mut env);
    }
}

/// Open the in-app YouTube sign-in WebView (YtLogin.start marshals to the UI
/// thread). The captured cookies come back via `nativeOnYtCookies` below.
pub fn launch_login() {
    init();
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result: Result<(), jni::errors::Error> = (|| {
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/YtLogin")?;
        env.call_static_method(
            &class,
            "start",
            "(Landroid/content/Context;)V",
            &[JValue::Object(&activity)],
        )?
        .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] YtLogin.start failed: {}", e);
        clear_jni_exception(&mut env);
    }
}

// Called from Kotlin: YtLogin.nativeOnYtCookies(String) — the captured cookie
// jar after sign-in, or "" if the user cancelled.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_temidaradev_kopuz_YtLogin_nativeOnYtCookies(
    mut env: JNIEnv,
    _class: JClass,
    cookies: JString,
) {
    let s: String = match env.get_string(&cookies) {
        Ok(v) => v.into(),
        Err(_) => String::new(),
    };
    super::set_yt_login_result(s);
}

// --- Headless PoToken minter (BgUtils in an Android System WebView) --------------
// The desktop wry minter is unavailable on a phone; instead PotMinter.kt hosts an
// offscreen System WebView at the music.youtube.com origin that runs the SAME
// BgUtils BotGuard JS and mints a content PO token per video. These functions are
// the JNI bridge: `pot_minter_init` stands the WebView up, `mint_pot` dispatches a
// per-track mint and awaits the reply, and `nativeOnPot` delivers it back.

static POT_INIT_DONE: AtomicBool = AtomicBool::new(false);
static POT_REQ_ID: AtomicU64 = AtomicU64::new(1);
static POT_PENDING: OnceLock<Mutex<HashMap<u64, oneshot::Sender<Result<String, String>>>>> =
    OnceLock::new();

fn pot_pending() -> &'static Mutex<HashMap<u64, oneshot::Sender<Result<String, String>>>> {
    POT_PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stand up the headless minter WebView once, injecting `script` (the shared
/// BgUtils init script) at document-start and signing it in with `cookies` (so
/// it skips YouTube's EU consent wall and loads music.youtube.com). Idempotent.
pub fn pot_minter_init(script: &str, cookies: &str) {
    if POT_INIT_DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    init();
    let vm = match JVM.get() {
        Some(v) => v,
        None => {
            POT_INIT_DONE.store(false, Ordering::SeqCst);
            return;
        }
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => {
            POT_INIT_DONE.store(false, Ordering::SeqCst);
            return;
        }
    };
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result: Result<(), jni::errors::Error> = (|| {
        let jscript = env.new_string(script)?;
        let jcookies = env.new_string(cookies)?;
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/PotMinter")?;
        env.call_static_method(
            &class,
            "init",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&jscript),
                JValue::Object(&jcookies),
            ],
        )?
        .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] PotMinter.init failed: {}", e);
        clear_jni_exception(&mut env);
        POT_INIT_DONE.store(false, Ordering::SeqCst);
    }
}

/// Mint a content PO token for `video_id` via the WebView; awaits the reply.
pub async fn mint_pot(video_id: &str) -> Result<String, String> {
    let id = POT_REQ_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    if let Ok(mut m) = pot_pending().lock() {
        m.insert(id, tx);
    }
    // Sanitize to the videoId charset before crossing into the JS template.
    let vid: String = video_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    pot_minter_mint_jni(&vid, id);
    match tokio::time::timeout(Duration::from_secs(15), rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => {
            if let Ok(mut m) = pot_pending().lock() {
                m.remove(&id);
            }
            Err("PotMinter dropped the reply".to_string())
        }
        Err(_) => {
            if let Ok(mut m) = pot_pending().lock() {
                m.remove(&id);
            }
            Err("PotMinter mint timed out (webview not ready)".to_string())
        }
    }
}

fn pot_minter_mint_jni(video_id: &str, req_id: u64) {
    let vm = match JVM.get() {
        Some(v) => v,
        None => return,
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(_) => return,
    };
    let result: Result<(), jni::errors::Error> = (|| {
        let jvid = env.new_string(video_id)?;
        let class = find_app_class(&mut env, "com/temidaradev/kopuz/PotMinter")?;
        env.call_static_method(
            &class,
            "mint",
            "(Ljava/lang/String;J)V",
            &[JValue::Object(&jvid), JValue::Long(req_id as i64)],
        )?
        .v()?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("[android] PotMinter.mint failed: {}", e);
        clear_jni_exception(&mut env);
    }
}

// Called from Kotlin: PotMinter.nativeOnPot(reqId, pot, err) — a non-empty pot is
// success; otherwise `err` carries the JS error/stack.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_temidaradev_kopuz_PotMinter_nativeOnPot(
    mut env: JNIEnv,
    _class: JClass,
    req_id: jni::sys::jlong,
    pot: JString,
    err: JString,
) {
    let pot_s: String = env.get_string(&pot).map(|s| s.into()).unwrap_or_default();
    let err_s: String = env.get_string(&err).map(|s| s.into()).unwrap_or_default();
    let result = if !pot_s.is_empty() {
        Ok(pot_s)
    } else if !err_s.is_empty() {
        Err(err_s)
    } else {
        Err("mint failed".to_string())
    };
    if let Some(tx) = pot_pending()
        .lock()
        .ok()
        .and_then(|mut m| m.remove(&(req_id as u64)))
    {
        let _ = tx.send(result);
    }
}

// ============================================================================
// Media3 ExoPlayer bridge (PlaybackService.kt)
//
// On Android the real playback runs in a native MediaSessionService (ExoPlayer)
// so it survives backgrounding, instead of cpal + the suspended Dioxus loop.
// Rust owns the queue model + URL resolution and drives ExoPlayer via these
// command wrappers; ExoPlayer reports back through the `native*` callbacks,
// which push events onto a queue the hooks crate drains to reconcile the UI.
// See docs/android-exoplayer-background-playback-plan.md.
// ============================================================================

/// Playback events reported by ExoPlayer (drained by the hooks driver loop).
#[derive(Debug, Clone)]
pub enum ExoEvent {
    /// ExoPlayer auto-advanced / skipped to a new item.
    Transition { media_id: String, index: i32 },
    /// Play/pause state changed or the ~500ms position tick. `duration_ms` is
    /// ExoPlayer's authoritative media duration (0 while still unknown) —
    /// track metadata often has none (e.g. YT search results).
    State {
        playing: bool,
        position_ms: i64,
        duration_ms: i64,
    },
    /// The whole playlist ended (ExoPlayer ran out of items).
    Ended,
    /// A playback error (usually an expired googlevideo URL → re-resolve).
    Error { media_id: String, code: i32 },
}

static EXO_EVENTS: Mutex<Vec<ExoEvent>> = Mutex::new(Vec::new());

fn push_exo_event(e: ExoEvent) {
    if let Ok(mut v) = EXO_EVENTS.lock() {
        v.push(e);
    }
    // Nudge the UI driver so it reconciles as soon as it next runs (foreground).
    wake_run_loop();
}

/// Drain the pending ExoPlayer events. Called by the hooks driver loop.
pub fn take_exo_events() -> Vec<ExoEvent> {
    EXO_EVENTS
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

const PLAYBACK_SERVICE: &str = "com/temidaradev/kopuz/PlaybackService";

/// Attach + find PlaybackService + run `f` (a static-method call). Errors are
/// swallowed after clearing any pending JNI exception, like the other bridges.
fn call_playback_service<F>(f: F)
where
    F: FnOnce(&mut JNIEnv, &JClass) -> Result<(), jni::errors::Error>,
{
    let Some(vm) = JVM.get() else { return };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Ok(class) = find_app_class(&mut env, PLAYBACK_SERVICE) else {
        clear_jni_exception(&mut env);
        return;
    };
    if f(&mut env, &class).is_err() {
        clear_jni_exception(&mut env);
    }
}

fn activity_obj() -> JObject<'static> {
    let ctx = ndk_context::android_context();
    unsafe { JObject::from_raw(ctx.context().cast()) }
}

/// Start playback of `items_json` (a JSON array of {url,mediaId,title,artist,
/// album,artworkUrl,durationMs}) from `start_index` at `position_ms`.
pub fn exo_play(items_json: &str, start_index: i32, position_ms: i64) {
    call_playback_service(|env, class| {
        let activity = activity_obj();
        let j_items = env.new_string(items_json)?;
        env.call_static_method(
            class,
            "cmdPlay",
            "(Landroid/content/Context;Ljava/lang/String;IJ)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&j_items),
                JValue::Int(start_index),
                JValue::Long(position_ms),
            ],
        )?
        .v()
    });
}

/// Replace the look-ahead window after the current item (rolling preload).
pub fn exo_set_upcoming(items_json: &str) {
    call_playback_service(|env, class| {
        let j_items = env.new_string(items_json)?;
        env.call_static_method(
            class,
            "cmdSetUpcoming",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&j_items)],
        )?
        .v()
    });
}

/// Replace only the upcoming items (everything after the current one), leaving
/// the playing track untouched — used when shuffle is toggled mid-playback.
pub fn exo_replace_upcoming(items_json: &str) {
    call_playback_service(|env, class| {
        let j_items = env.new_string(items_json)?;
        env.call_static_method(
            class,
            "cmdReplaceUpcoming",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&j_items)],
        )?
        .v()
    });
}

fn exo_void(method: &str) {
    call_playback_service(|env, class| env.call_static_method(class, method, "()V", &[])?.v());
}

pub fn exo_clear() {
    exo_void("cmdClear");
}
pub fn exo_pause() {
    exo_void("cmdPause");
}
pub fn exo_resume() {
    exo_void("cmdResume");
}
pub fn exo_next() {
    exo_void("cmdNext");
}
pub fn exo_prev() {
    exo_void("cmdPrev");
}

pub fn exo_seek(position_ms: i64) {
    call_playback_service(|env, class| {
        env.call_static_method(class, "cmdSeek", "(J)V", &[JValue::Long(position_ms)])?
            .v()
    });
}

pub fn exo_set_volume(volume: f32) {
    call_playback_service(|env, class| {
        env.call_static_method(class, "cmdSetVolume", "(F)V", &[JValue::Float(volume)])?
            .v()
    });
}

pub fn exo_stop() {
    call_playback_service(|env, class| {
        let activity = activity_obj();
        env.call_static_method(
            class,
            "cmdStop",
            "(Landroid/content/Context;)V",
            &[JValue::Object(&activity)],
        )?
        .v()
    });
}

/// Current playback position in ms, or -1 if unavailable / not on the UI thread.
pub fn exo_position() -> i64 {
    let mut out = -1i64;
    call_playback_service(|env, class| {
        out = env.call_static_method(class, "cmdPosition", "()J", &[])?.j()?;
        Ok(())
    });
    out
}

// --- Callbacks from PlaybackService (Kotlin) -------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_temidaradev_kopuz_PlaybackService_nativeOnTransition(
    mut env: JNIEnv,
    _class: JClass,
    media_id: JString,
    index: jni::sys::jint,
) {
    let media_id: String = env.get_string(&media_id).map(|s| s.into()).unwrap_or_default();
    push_exo_event(ExoEvent::Transition { media_id, index });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_temidaradev_kopuz_PlaybackService_nativeOnState(
    _env: JNIEnv,
    _class: JClass,
    is_playing: jni::sys::jboolean,
    position_ms: jni::sys::jlong,
    duration_ms: jni::sys::jlong,
) {
    push_exo_event(ExoEvent::State {
        playing: is_playing != 0,
        position_ms,
        duration_ms,
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_temidaradev_kopuz_PlaybackService_nativeOnEnded(
    _env: JNIEnv,
    _class: JClass,
) {
    push_exo_event(ExoEvent::Ended);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_temidaradev_kopuz_PlaybackService_nativeOnError(
    mut env: JNIEnv,
    _class: JClass,
    media_id: JString,
    code: jni::sys::jint,
) {
    let media_id: String = env.get_string(&media_id).map(|s| s.into()).unwrap_or_default();
    push_exo_event(ExoEvent::Error { media_id, code });
}
