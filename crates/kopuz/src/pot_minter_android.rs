//! Android PoToken minter driver.
//!
//! The desktop wry minter (`pot_minter.rs`) is unavailable on a phone, so on
//! Android we drive a headless System WebView (`android-src/.../PotMinter.kt`, via
//! the JNI bridge in `player::systemint`) that runs the same BgUtils BotGuard JS.
//! This module is the glue: it registers a channel with `server::ytmusic::botguard`
//! so the existing ANDROID_VR + content_pot path in `server::ytmusic::player`
//! lights up, stands the WebView up once, and forwards each mint request to it.

use std::sync::atomic::{AtomicBool, Ordering};

use dioxus::prelude::spawn;
use server::ytmusic::botguard;
use tokio::sync::mpsc;

use crate::pot_minter_script::init_script;

static STARTED: AtomicBool = AtomicBool::new(false);

/// Start the minter driver once: register the botguard channel, bring up the
/// WebView, and drain mint requests to it. Idempotent — safe to call from the
/// reactive trigger on every config change.
pub fn start() {
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
    player::systemint::pot_minter_init(&init_script());
    // Drain mint requests on the Dioxus runtime (tokio-backed, so the bridge's
    // mint timeout works). Serialized — a music queue resolves tracks roughly in
    // order and a warm minter mints sub-second, so one-at-a-time is plenty.
    spawn(async move {
        while let Some(req) = rx.recv().await {
            let result = player::systemint::mint_pot(&req.video_id).await;
            let _ = req.reply.send(result);
        }
    });
    eprintln!("[pot-minter-android] driver started");
}
