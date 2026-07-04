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
/// ExoPlayer's authoritative media duration — the ONLY reliable source when
/// track metadata has none (e.g. YT search results). 0 = not yet known.
static DURATION_MS: AtomicI64 = AtomicI64::new(0);
/// One-shot: the whole queue TRULY ended (not an under-fill the engine can
/// recover). The driver drains it to start autoradio on the Dioxus thread.
static ENDED_DIRTY: AtomicBool = AtomicBool::new(false);

/// A snapshot for the UI to reconcile. `current_index` is `Some` only when the
/// playing track changed since the last read; `ended` is `true` once when the
/// queue finished (→ the driver starts autoradio).
pub struct UiUpdate {
    pub current_index: Option<usize>,
    pub playing: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub ended: bool,
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
        duration_ms: DURATION_MS.load(Ordering::Acquire),
        ended: ENDED_DIRTY.swap(false, Ordering::AcqRel),
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
    /// Shuffle toggled: keep the current track playing, only swap the UPCOMING
    /// items to the new play order (no re-buffer of the current song).
    ReorderUpcoming,
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
    } else if id.starts_with("soundcloud:") {
        // Native SoundCloud resolve (no yt-dlp on Android) → a progressive mp3
        // or HLS URL ExoPlayer can play directly.
        match ::server::soundcloud::resolve_path(&id).await {
            Ok(info) => info.url,
            Err(e) => {
                eprintln!("[exo] soundcloud resolve failed: {e}");
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

/// Resolve forward from `start`, SKIPPING tracks that fail to resolve (dead YT
/// videos, bot-checks, transient errors) until `WINDOW_AHEAD + 1` items are
/// collected or the mirror ends. Returns the resolved item JSONs, the index of
/// the FIRST playable track, and the index of the LAST resolved track. This is
/// what keeps a single bad track from dead-stopping the whole queue.
async fn resolve_forward(
    start: usize,
    cookies: &Option<String>,
) -> (Vec<serde_json::Value>, Option<usize>, usize) {
    let len = mirror().lock().unwrap_or_else(|e| e.into_inner()).tracks.len();
    let mut items = Vec::new();
    let mut first = None;
    let mut last = start;
    let mut i = start;
    while i < len && items.len() <= WINDOW_AHEAD {
        let track = {
            let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
            m.tracks.get(i).cloned()
        };
        let Some(track) = track else { break };
        if let Some(j) = resolve_item(&track, cookies).await {
            if first.is_none() {
                first = Some(i);
            }
            items.push(j);
            last = i;
        }
        i += 1;
    }
    (items, first, last)
}

async fn handle_cmd(cmd: Cmd) {
    let position_ms = match cmd {
        Cmd::PlayFrom { position_ms } | Cmd::Reresolve { position_ms } => position_ms,
        Cmd::ReorderUpcoming => return handle_reorder_upcoming().await,
    };
    let (start, cookies) = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        (m.index, m.cookies.clone())
    };
    let (items, first, last) = resolve_forward(start, &cookies).await;
    let Some(first) = first else {
        // Nothing from here on is playable → treat as end so autoradio kicks in.
        eprintln!("[exo] nothing resolvable from {start}; ending");
        PLAYING.store(false, Ordering::Release);
        ENDED_DIRTY.store(true, Ordering::Release);
        return;
    };
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        m.index = first;
        m.resolved_upto = last;
    }
    eprintln!("[exo] play from {first}: {} item(s)", items.len());
    systemint::exo_play(
        &serde_json::Value::Array(items).to_string(),
        0,
        position_ms.max(0),
    );
    set_current_index(first);
    PLAYING.store(true, Ordering::Release);
}

/// Shuffle toggled while playing: leave the current ExoPlayer item alone and
/// replace ONLY the upcoming items with the new play order — no re-buffer.
async fn handle_reorder_upcoming() {
    let (from, cookies) = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        (m.index + 1, m.cookies.clone())
    };
    let (items, _first, last) = resolve_forward(from, &cookies).await;
    if !items.is_empty() {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        m.resolved_upto = last;
    }
    eprintln!("[exo] reorder upcoming from {from}: {} item(s)", items.len());
    systemint::exo_replace_upcoming(&serde_json::Value::Array(items).to_string());
}

async fn handle_event(ev: ExoEvent) {
    match ev {
        ExoEvent::Transition { media_id, .. } => {
            // New track → the previous track's duration must not leak onto it
            // (ExoPlayer reports the fresh one within ~a tick once loaded).
            DURATION_MS.store(0, Ordering::Release);
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
            duration_ms,
        } => {
            PLAYING.store(playing, Ordering::Release);
            if position_ms >= 0 {
                POSITION_MS.store(position_ms, Ordering::Release);
            }
            if duration_ms > 0 {
                DURATION_MS.store(duration_ms, Ordering::Release);
            }
        }
        ExoEvent::Ended => {
            // ExoPlayer ran out of items. If the mirror still has tracks ahead,
            // the look-ahead window under-filled (a resolve failed / a refill
            // lagged) — recover by resolving+playing from the next track instead
            // of dead-stopping mid-playlist. Only a TRUE end (index at the last
            // track) flags autoradio for the driver.
            let has_more = {
                let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                m.index + 1 < m.tracks.len()
            };
            if has_more {
                {
                    let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                    m.index += 1;
                }
                eprintln!("[exo] ended early with tracks remaining — recovering");
                let _ = engine_tx().send(Cmd::PlayFrom { position_ms: 0 });
            } else {
                PLAYING.store(false, Ordering::Release);
                ENDED_DIRTY.store(true, Ordering::Release);
                eprintln!("[exo] queue truly ended");
            }
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
    DURATION_MS.store(0, Ordering::Release);
    let _ = engine_tx().send(Cmd::PlayFrom { position_ms });
}

/// Shuffle toggled during playback: swap the mirror to the new play order but
/// keep the CURRENT track playing and only rebuild the upcoming ExoPlayer items
/// (no re-buffer / no gap). `current_idx` is the current track's play-order
/// index in `tracks` (so `tracks[current_idx]` is the now-playing song).
pub fn reorder_upcoming(tracks: Vec<Track>, current_idx: usize, cookies: Option<String>) {
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        let cap = tracks.len().saturating_sub(1);
        m.tracks = tracks;
        m.index = current_idx.min(cap);
        m.resolved_upto = m.index;
        m.cookies = cookies;
    }
    let _ = engine_tx().send(Cmd::ReorderUpcoming);
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
