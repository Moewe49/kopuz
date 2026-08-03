//! Android PoToken minter driver.
//!
//! The desktop wry minter (`pot_minter.rs`) is unavailable on a phone, so on
//! Android we drive a headless System WebView (`android-src/.../PotMinter.kt`, via
//! the JNI bridge in `player::systemint`) that runs the same BgUtils BotGuard JS.
//! This module is the glue: it registers a channel with `server::ytmusic::botguard`
//! so the existing ANDROID_VR + content_pot path in `server::ytmusic::player`
//! lights up, stands the WebView up once, and forwards each mint request to it.

use std::sync::atomic::{AtomicBool, Ordering};

use server::ytmusic::botguard;
use tokio::sync::mpsc;

use crate::pot_minter_script::init_script;

static STARTED: AtomicBool = AtomicBool::new(false);

/// Start the minter driver once: register the botguard channel, bring up the
/// WebView (signed in with `cookies` to skip the consent wall), and drain mint
/// requests to it. Idempotent — safe to call from the reactive trigger on every
/// config change.
pub fn start(cookies: String) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<botguard::MintRequest>();
    if botguard::set_minter(tx).is_err() {
        // Already registered elsewhere — nothing to drive.
        return;
    }
    // Bring the WebView up now so the BotGuard integrity token pre-warms before
    // the first track resolves (mirrors the desktop minter's warm-up).
    player::systemint::pot_minter_init(&init_script(), &cookies);
    // Drain mint requests on a DEDICATED OS thread — never on the Dioxus
    // runtime.
    //
    // Android suspends the wry/winit event loop while the Activity is Stopped,
    // and with it every Dioxus task. A drain task living there stops polling the
    // moment the app is backgrounded, so each mint request sat in the channel
    // untouched until its 2.5s timeout fired: no PO token → `player::resolve`
    // fell all the way through to the bare clients → googlevideo 403s past the
    // first ~1 MB, or LOGIN_REQUIRED. That is the background half of "playback
    // stops after a few minutes". The ExoPlayer engine thread has the same
    // requirement and is built the same way (see `hooks::android_exo`).
    //
    // Serialized — a music queue resolves tracks roughly in order and a warm
    // minter mints sub-second, so one-at-a-time is plenty.
    let spawned = std::thread::Builder::new()
        .name("kopuz-pot-minter".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[pot-minter-android] runtime: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                while let Some(req) = rx.recv().await {
                    let result = player::systemint::mint_pot(&req.video_id).await;
                    let _ = req.reply.send(result);
                }
            });
        });
    if let Err(e) = spawned {
        eprintln!("[pot-minter-android] drain thread: {e}");
        return;
    }
    eprintln!("[pot-minter-android] driver started");
}
