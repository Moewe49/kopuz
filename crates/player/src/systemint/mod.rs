#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{
    SystemEvent, init, refresh_now_playing, set_background_handler, set_tokio_waker,
    update_now_playing, wake_run_loop,
};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{SystemEvent, poll_event, update_now_playing, update_position};

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::{SystemEvent, init, poll_event, update_now_playing, wait_event};

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "android")]
pub use android::{
    SystemEvent, get_android_music_dir, get_files_dir, init, move_task_to_back,
    request_permissions, set_background_handler, stop_session, take_back_pressed,
    update_now_playing, wake_run_loop,
};

#[cfg(not(target_os = "android"))]
pub fn request_permissions() {}

#[cfg(not(target_os = "android"))]
pub fn get_android_music_dir() -> Option<String> {
    None
}

// --- In-app YouTube sign-in (Android WebView) -----------------------------------
// A phone has no managed desktop browser to drive over CDP, so on Android we host
// our own WebView (see android-src/.../YtLogin.kt) for the one-time Google/YT
// sign-in and read the cookies out of Android's CookieManager. The captured jar
// feeds the same `persist_yt_session` path the desktop browser-login uses.
use std::sync::Mutex;

static YT_LOGIN_RESULT: Mutex<Option<String>> = Mutex::new(None);

/// Begin the in-app YouTube sign-in. On Android this opens the WebView sign-in
/// dialog; poll [`take_yt_login_result`] for the captured cookies. No-op on
/// desktop, which uses the managed-browser flow instead.
pub fn start_yt_login() {
    if let Ok(mut slot) = YT_LOGIN_RESULT.lock() {
        *slot = None;
    }
    #[cfg(target_os = "android")]
    android::launch_login();
}

/// Poll the in-app sign-in result: `None` while the user is still signing in,
/// `Some(cookies)` once captured, or `Some("")` if they backed out.
pub fn take_yt_login_result() -> Option<String> {
    YT_LOGIN_RESULT.lock().ok().and_then(|mut s| s.take())
}

/// Delivered from the Android WebView via JNI when sign-in completes/cancels.
#[cfg(target_os = "android")]
pub(crate) fn set_yt_login_result(cookies: String) {
    if let Ok(mut slot) = YT_LOGIN_RESULT.lock() {
        *slot = Some(cookies);
    }
}

// --- Event-driven wakes for the background loops ---------------------------------
// Let the player/back loops sleep on a long interval while idle instead of busy-polling
// at 10Hz, then wake them the instant something happens (media command, track finished,
// back press). notify_one stores one permit, so a wake fired before the loop re-awaits
// is never lost. Compiled for all native targets (systemint is excluded on wasm).
use std::sync::OnceLock;
use tokio::sync::Notify;

fn bg_notify() -> &'static Notify {
    static N: OnceLock<Notify> = OnceLock::new();
    N.get_or_init(Notify::new)
}

/// Wake the player task loop now (media command or track finished). Sync, any thread.
pub fn bg_wake() {
    bg_notify().notify_one();
}

/// Awaited by the player task loop's adaptive sleep.
pub async fn bg_wait() {
    bg_notify().notified().await;
}

fn back_notify() -> &'static Notify {
    static N: OnceLock<Notify> = OnceLock::new();
    N.get_or_init(Notify::new)
}

/// Wake the Android back-handling loop now. Sync, any thread.
pub fn back_wake() {
    back_notify().notify_one();
}

/// Awaited by the back-handling loop's adaptive sleep.
pub async fn back_wait() {
    back_notify().notified().await;
}
