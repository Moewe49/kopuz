//! Android playback engine — drives the native ExoPlayer MediaSessionService
//! (see `player::systemint::android` + `PlaybackService.kt`).
//!
//! On Android the real playback runs in ExoPlayer so it survives the Activity
//! being Stopped (the wry/Dioxus loop + the old cpal auto-advance are suspended
//! in the background). Rust keeps the queue model + resolves stream URLs.
//!
//! A DEDICATED OS-thread ("exo engine") owns everything that must keep working
//! regardless of the Dioxus executor: it resolves URLs, feeds ExoPlayer a rolling
//! look-ahead window, and — crucially — **drains ExoPlayer's own events and
//! refills the window itself**, so auto-advance never dead-stops even if the
//! Dioxus driver loop is asleep. It also caches playback state (index / playing /
//! position) into lock-free atomics. The Dioxus driver only *reads* that state
//! (`take_ui_update`) to reconcile the UI Signals — it is never on the critical
//! path for playback. Nothing here touches a Dioxus Signal.
//!
//! See docs/android-exoplayer-background-playback-plan.md.

use player::systemint::{self, ExoEvent};
use reader::models::Track;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

/// Upcoming tracks kept resolved ahead of the current one in ExoPlayer's
/// playlist. Bigger = more resilient to a refill hiccup; smaller = faster start
/// (each resolve is a network call).
const WINDOW_AHEAD: usize = 3;

/// Canonical queue mirror — plain data, owned by the engine thread. Never a Signal.
struct Mirror {
    tracks: Vec<Track>,
    /// Index in `tracks` of the currently-playing track.
    index: usize,
    /// Highest `tracks` index already handed to ExoPlayer.
    resolved_upto: usize,
    cookies: Option<String>,
}

fn mirror() -> &'static Mutex<Mirror> {
    static M: OnceLock<Mutex<Mirror>> = OnceLock::new();
    M.get_or_init(|| {
        Mutex::new(Mirror {
            tracks: Vec::new(),
            index: 0,
            resolved_upto: 0,
            cookies: None,
        })
    })
}

// --- Shared playback state: engine writes, the Dioxus driver reads. ----------
static CUR_INDEX: AtomicUsize = AtomicUsize::new(0);
static INDEX_DIRTY: AtomicBool = AtomicBool::new(false);
static PLAYING: AtomicBool = AtomicBool::new(false);
static POSITION_MS: AtomicI64 = AtomicI64::new(0);

/// A snapshot for the UI to reconcile. `current_index` is `Some` only when the
/// playing track changed since the last read.
pub struct UiUpdate {
    pub current_index: Option<usize>,
    pub playing: bool,
    pub position_ms: i64,
}

/// Read the latest playback state (called by the Dioxus driver on its thread).
pub fn take_ui_update() -> UiUpdate {
    let current_index = if INDEX_DIRTY.swap(false, Ordering::AcqRel) {
        Some(CUR_INDEX.load(Ordering::Acquire))
    } else {
        None
    };
    UiUpdate {
        current_index,
        playing: PLAYING.load(Ordering::Acquire),
        position_ms: POSITION_MS.load(Ordering::Acquire),
    }
}

fn set_current_index(i: usize) {
    CUR_INDEX.store(i, Ordering::Release);
    INDEX_DIRTY.store(true, Ordering::Release);
}

enum Cmd {
    /// Resolve the current + look-ahead window and start playback at `position_ms`.
    PlayFrom { position_ms: i64 },
    /// A URL went stale (403) — re-resolve the window fresh and restart it.
    Reresolve { position_ms: i64 },
}

fn engine_tx() -> &'static Sender<Cmd> {
    static TX: OnceLock<Sender<Cmd>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let _ = std::thread::Builder::new()
            .name("kopuz-exo-engine".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("[exo] engine runtime: {e}");
                        return;
                    }
                };
                eprintln!("[exo] engine thread started");
                loop {
                    while let Ok(cmd) = rx.try_recv() {
                        rt.block_on(handle_cmd(cmd));
                    }
                    for ev in systemint::take_exo_events() {
                        rt.block_on(handle_event(ev));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
            });
        tx
    })
}

fn video_id(track: &Track) -> Option<String> {
    track
        .path
        .to_string_lossy()
        .strip_prefix("ytmusic:")
        .and_then(|rest| rest.split(':').next())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Stable per-track id used as ExoPlayer's `mediaId` and for reconciliation.
fn track_id(track: &Track) -> String {
    track.path.to_string_lossy().to_string()
}

fn cover_url(track: &Track) -> String {
    let path = track.path.to_string_lossy();
    utils::jellyfin_image::track_cover_url_with_album_fallback(
        &path,
        &track.album_id,
        "",
        None,
        800,
        90,
    )
    .unwrap_or_default()
}

/// Resolve one track into an ExoPlayer MediaItem JSON object, or `None`.
async fn resolve_item(track: &Track, cookies: &Option<String>) -> Option<serde_json::Value> {
    let id = track_id(track);
    let url = if let Some(vid) = video_id(track) {
        let yt =
            ::server::ytmusic::YouTubeMusicClient::with_cookies(cookies.clone().unwrap_or_default());
        match yt.get_stream(&vid).await {
            Ok(info) => info.url,
            Err(e) => {
                eprintln!("[exo] resolve failed for {vid}: {e}");
                return None;
            }
        }
    } else if std::path::Path::new(&id).exists() {
        format!("file://{id}")
    } else {
        return None;
    };
    Some(serde_json::json!({
        "url": url,
        "mediaId": id,
        "title": track.title,
        "artist": track.artist,
        "album": track.album,
        "artworkUrl": cover_url(track),
        "durationMs": (track.duration as i64).saturating_mul(1000),
    }))
}

/// Resolve tracks `[from..=to]` from the mirror into a JSON array (skipping
/// unresolvable ones). Returns the JSON + how many resolved.
async fn resolve_range(from: usize, to: usize, cookies: &Option<String>) -> Vec<serde_json::Value> {
    let slice: Vec<Track> = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        if from > to || from >= m.tracks.len() {
            Vec::new()
        } else {
            m.tracks[from..=to.min(m.tracks.len() - 1)].to_vec()
        }
    };
    let mut items = Vec::new();
    for t in &slice {
        if let Some(j) = resolve_item(t, cookies).await {
            items.push(j);
        }
    }
    items
}

async fn handle_cmd(cmd: Cmd) {
    let position_ms = match cmd {
        Cmd::PlayFrom { position_ms } | Cmd::Reresolve { position_ms } => position_ms,
    };
    let (index, end, cookies) = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        let end = (m.index + WINDOW_AHEAD).min(m.tracks.len().saturating_sub(1));
        (m.index, end, m.cookies.clone())
    };
    let items = resolve_range(index, end, &cookies).await;
    if items.is_empty() {
        eprintln!("[exo] no resolvable tracks at {index}; not starting");
        return;
    }
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        m.resolved_upto = index + items.len() - 1;
    }
    eprintln!("[exo] play from {index}: {} item(s)", items.len());
    systemint::exo_play(
        &serde_json::Value::Array(items).to_string(),
        0,
        position_ms.max(0),
    );
    set_current_index(index);
    PLAYING.store(true, Ordering::Release);
}

async fn handle_event(ev: ExoEvent) {
    match ev {
        ExoEvent::Transition { media_id, .. } => {
            let (qidx, refill_from, refill_to, cookies) = {
                let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                let qidx = m.tracks.iter().position(|t| track_id(t) == media_id);
                if let Some(q) = qidx {
                    m.index = q;
                }
                let target = (m.index + WINDOW_AHEAD).min(m.tracks.len().saturating_sub(1));
                let (from, to) = if target > m.resolved_upto {
                    let from = m.resolved_upto + 1;
                    m.resolved_upto = target;
                    (from, target)
                } else {
                    (1, 0) // empty range
                };
                (qidx, from, to, m.cookies.clone())
            };
            if let Some(q) = qidx {
                set_current_index(q);
            }
            eprintln!("[exo] transition -> qidx {qidx:?}, refill [{refill_from}..={refill_to}]");
            let items = resolve_range(refill_from, refill_to, &cookies).await;
            if !items.is_empty() {
                systemint::exo_set_upcoming(&serde_json::Value::Array(items).to_string());
            }
        }
        ExoEvent::State {
            playing,
            position_ms,
        } => {
            PLAYING.store(playing, Ordering::Release);
            if position_ms >= 0 {
                POSITION_MS.store(position_ms, Ordering::Release);
            }
        }
        ExoEvent::Ended => {
            PLAYING.store(false, Ordering::Release);
            eprintln!("[exo] playlist ended");
        }
        ExoEvent::Error { media_id, code } => {
            eprintln!("[exo] player error {code} on {media_id} — reresolving");
            let pos = POSITION_MS.load(Ordering::Acquire);
            let _ = engine_tx().send(Cmd::Reresolve { position_ms: pos });
        }
    }
}

// --- Public API (called on the Dioxus thread from the controller / driver) ---

/// Start (or restart) playback of `tracks` from `start_index`.
pub fn play(tracks: Vec<Track>, start_index: usize, cookies: Option<String>, position_ms: i64) {
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        let cap = tracks.len().saturating_sub(1);
        m.tracks = tracks;
        m.index = start_index.min(cap);
        m.resolved_upto = m.index;
        m.cookies = cookies;
    }
    let _ = engine_tx().send(Cmd::PlayFrom { position_ms });
}

pub fn pause() {
    PLAYING.store(false, Ordering::Release);
    systemint::exo_pause();
}
pub fn resume() {
    PLAYING.store(true, Ordering::Release);
    systemint::exo_resume();
}
pub fn next() {
    systemint::exo_next();
}
pub fn prev() {
    systemint::exo_prev();
}
pub fn seek_ms(ms: i64) {
    POSITION_MS.store(ms.max(0), Ordering::Release);
    systemint::exo_seek(ms);
}
pub fn set_volume(v: f32) {
    systemint::exo_set_volume(v);
}
pub fn stop() {
    PLAYING.store(false, Ordering::Release);
    systemint::exo_stop();
}
