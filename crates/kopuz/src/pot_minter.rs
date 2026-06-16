//! In-app anonymous PoToken minter (issue #349).
//!
//! Stands up a hidden `wry` WebView at the music.youtube.com origin (same-origin
//! so BgUtils' WAA `fetch` isn't CORS-blocked), injects BgUtils, and mints
//! content pots on demand — feeding `server::ytmusic::botguard`'s channel. No
//! external binary, no yt-dlp. Desktop (Linux/macOS/Windows): only the webview
//! attach differs per platform (GTK vbox vs raw NSView/HWND); the mint channel
//! is drained from the event-loop tick on the main thread, so there's no
//! platform-specific async runtime.
//!
//! Proven standalone in /tmp/yt-wry-minter. Tricks: same-origin page for the
//! WAA fetch, `trustedTypes.createPolicy` to `new Function` the BotGuard VM
//! past YouTube's CSP, and capturing `BG` at document-start before the page
//! clobbers `window.module`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use dioxus::desktop::tao::dpi::{LogicalPosition, LogicalSize};
use dioxus::desktop::tao::event_loop::EventLoopWindowTarget;
use dioxus::desktop::tao::window::{Window, WindowBuilder};
use dioxus::desktop::wry::{WebView, WebViewBuilder};
use server::ytmusic::botguard::{self, MintRequest};
use tokio::sync::mpsc;

use crate::pot_minter_script::init_script;

#[cfg(target_os = "linux")]
use dioxus::desktop::tao::platform::unix::WindowExtUnix;
#[cfg(target_os = "linux")]
use dioxus::desktop::wry::WebViewBuilderExtUnix;

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                  Chrome/131.0.0.0 Safari/537.36";

static REQ_ID: AtomicU64 = AtomicU64::new(1);
/// Set by the app when an anonymous YouTube Music server is active; the
/// event-loop handler then installs the minter on the next tick.
static WANT: AtomicBool = AtomicBool::new(false);

/// Called from the app (anon YT Music selected) to request the minter.
pub fn request() {
    WANT.store(true, Ordering::Relaxed);
}

type Pending = Rc<RefCell<HashMap<u64, tokio::sync::oneshot::Sender<Result<String, String>>>>>;

/// The live minter, kept on the main thread for the app's lifetime.
struct Minter {
    _window: Window,
    webview: WebView,
    pending: Pending,
    rx: mpsc::UnboundedReceiver<MintRequest>,
}

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    static STATE: RefCell<Option<Minter>> = const { RefCell::new(None) };
}


/// Create the minter WebView once an anon YT Music server is active and register
/// its channel with `botguard`. Called every event-loop tick from
/// `Config::with_custom_event_handler`; no-ops until `request()` is set.
pub fn install_if_wanted<T: 'static>(target: &EventLoopWindowTarget<T>) {
    if !WANT.load(Ordering::Relaxed) {
        return;
    }
    // Only mark installed once setup actually succeeds — otherwise a transient
    // failure on the way down would wedge the minter off for the whole session.
    if INSTALLED.with(|c| c.get()) {
        return;
    }

    // Hidden, undecorated, parked off-screen — the webview runs (JS, fetch)
    // regardless of visibility. On Linux wry attaches to the window's GTK vbox
    // container (the generic raw-handle `build` isn't supported here).
    let window = match WindowBuilder::new()
        .with_title("kopuz pot minter")
        .with_inner_size(LogicalSize::new(1.0, 1.0))
        .with_position(LogicalPosition::new(-32000.0, -32000.0))
        .with_decorations(false)
        .with_visible(false)
        .build(target)
    {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[pot-minter] window build failed: {e}");
            return;
        }
    };
    let pending: Pending = Rc::new(RefCell::new(HashMap::new()));
    let pending_ipc = pending.clone();

    let builder = WebViewBuilder::new()
        .with_url("https://music.youtube.com/")
        .with_user_agent(UA)
        .with_initialization_script(&init_script())
        .with_ipc_handler(move |req| {
            let body = req.into_body();
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let Some(id) = v.get("id").and_then(|i| i.as_u64()) else {
                return;
            };
            let result = match v.get("pot").and_then(|p| p.as_str()) {
                Some(pot) => Ok(pot.to_string()),
                None => Err(v
                    .get("err")
                    .and_then(|e| e.as_str())
                    .unwrap_or("mint failed")
                    .to_string()),
            };
            if let Some(reply) = pending_ipc.borrow_mut().remove(&id) {
                let _ = reply.send(result);
            }
        });

    // The only platform-specific bit: Linux wry attaches to the window's GTK
    // vbox; macOS/Windows take the raw NSView/HWND via the generic `build`.
    #[cfg(target_os = "linux")]
    let built = match window.default_vbox() {
        Some(vbox) => builder.build_gtk(vbox),
        None => {
            eprintln!("[pot-minter] no GTK vbox on window");
            return;
        }
    };
    #[cfg(not(target_os = "linux"))]
    let built = builder.build(&window);

    let webview = match built {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[pot-minter] webview build failed: {e}");
            return;
        }
    };

    let (tx, rx) = mpsc::unbounded_channel::<MintRequest>();
    if botguard::set_minter(tx).is_err() {
        eprintln!("[pot-minter] minter already registered");
    }

    STATE.with(|s| {
        *s.borrow_mut() = Some(Minter {
            _window: window,
            webview,
            pending,
            rx,
        });
    });
    INSTALLED.with(|c| c.set(true));
    eprintln!("[pot-minter] installed (anon PoToken minting via webview)");
}

/// Drain queued mint requests and dispatch each to the webview. Called every
/// event-loop tick from the custom event handler — runs on the main thread on
/// all platforms, replacing the old Linux-only glib async drain.
pub fn pump() {
    STATE.with(|s| {
        let mut guard = s.borrow_mut();
        let Some(state) = guard.as_mut() else {
            return;
        };
        while let Ok(req) = state.rx.try_recv() {
            let id = REQ_ID.fetch_add(1, Ordering::Relaxed);
            state.pending.borrow_mut().insert(id, req.reply);
            let vid: String = req
                .video_id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            let _ = state.webview.evaluate_script(&format!(
                "window.__kopuzMint && window.__kopuzMint('{vid}', {id})"
            ));
        }
    });
}
