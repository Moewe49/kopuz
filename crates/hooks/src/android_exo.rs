//! Android playback engine — drives the native ExoPlayer MediaSessionService
//! (see `player::systemint::android` + `PlaybackService.kt`).
//!
//! On Android the real playback runs in ExoPlayer so it survives the Activity
//! being Stopped (the wry/Dioxus loop + the old cpal auto-advance are suspended
//! in the background). Rust keeps the queue model + resolves stream URLs; this
//! module feeds ExoPlayer a rolling look-ahead window (current + next) and
//! refills it as ExoPlayer auto-advances. All resolves run on a DEDICATED
//! OS-thread tokio runtime so they work even while the Dioxus executor is
//! suspended. Nothing here touches a Dioxus Signal — the driver loop reconciles
//! the UI from the `ExoEvent`s this playback produces.
//!
//! See docs/android-exoplayer-background-playback-plan.md.

use player::systemint;
use reader::models::Track;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

/// Canonical queue mirror — plain data, read by the resolver thread (never a Signal).
struct Mirror {
    tracks: Vec<Track>,
    index: usize,
    cookies: Option<String>,
}

fn mirror() -> &'static Mutex<Mirror> {
    static M: OnceLock<Mutex<Mirror>> = OnceLock::new();
    M.get_or_init(|| {
        Mutex::new(Mirror {
            tracks: Vec::new(),
            index: 0,
            cookies: None,
        })
    })
}

enum Cmd {
    /// Resolve the current + next track and start playback at `position_ms`.
    PlayFrom { position_ms: i64 },
    /// ExoPlayer advanced onto `media_id`; resolve the following track and append it.
    RefillAfter { media_id: String },
    /// A URL went stale (403) — re-resolve the current window fresh and restart it.
    Reresolve { position_ms: i64 },
}

fn resolver_tx() -> &'static Sender<Cmd> {
    static TX: OnceLock<Sender<Cmd>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let _ = std::thread::Builder::new()
            .name("kopuz-exo-resolver".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("[exo] resolver runtime: {e}");
                        return;
                    }
                };
                for cmd in rx {
                    rt.block_on(handle(cmd));
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

/// Stable per-track id used as ExoPlayer's `mediaId` and for UI reconciliation.
/// The full source path is unique for both YT (`ytmusic:VID:…`) and local tracks.
fn track_id(track: &Track) -> String {
    track.path.to_string_lossy().to_string()
}

/// Resolve one track into an ExoPlayer MediaItem JSON object, or `None` if it
/// can't be resolved (expired session / network / missing local file).
async fn resolve_item(track: &Track, cookies: &Option<String>) -> Option<serde_json::Value> {
    let id = track_id(track);
    let url = if let Some(vid) = video_id(track) {
        // YT Music: resolve the googlevideo stream URL (ExoPlayer plays it directly).
        let yt =
            ::server::ytmusic::YouTubeMusicClient::with_cookies(cookies.clone().unwrap_or_default());
        yt.get_stream(&vid).await.ok()?.url
    } else if std::path::Path::new(&id).exists() {
        // Local library / offline-downloaded file.
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

async fn handle(cmd: Cmd) {
    match cmd {
        Cmd::PlayFrom { position_ms } | Cmd::Reresolve { position_ms } => {
            let (cur, next, cookies) = {
                let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                (
                    m.tracks.get(m.index).cloned(),
                    m.tracks.get(m.index + 1).cloned(),
                    m.cookies.clone(),
                )
            };
            let Some(cur) = cur else { return };
            let mut items: Vec<serde_json::Value> = Vec::new();
            if let Some(j) = resolve_item(&cur, &cookies).await {
                items.push(j);
            }
            if items.is_empty() {
                eprintln!("[exo] could not resolve current track; not starting");
                return;
            }
            if let Some(next) = next {
                if let Some(j) = resolve_item(&next, &cookies).await {
                    items.push(j);
                }
            }
            let json = serde_json::Value::Array(items).to_string();
            systemint::exo_play(&json, 0, position_ms.max(0));
        }
        Cmd::RefillAfter { media_id } => {
            let (next, cookies) = {
                let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(pos) = m.tracks.iter().position(|t| track_id(t) == media_id) {
                    m.index = pos;
                }
                (m.tracks.get(m.index + 1).cloned(), m.cookies.clone())
            };
            let Some(next) = next else { return };
            if let Some(j) = resolve_item(&next, &cookies).await {
                systemint::exo_set_upcoming(&serde_json::Value::Array(vec![j]).to_string());
            }
        }
    }
}

// --- Public API (called on the Dioxus thread from the controller / driver) ---

/// Start (or restart) playback of `tracks` from `start_index`. Resolves the
/// current + next URL on the resolver thread, then hands them to ExoPlayer.
pub fn play(tracks: Vec<Track>, start_index: usize, cookies: Option<String>, position_ms: i64) {
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        m.tracks = tracks;
        let cap = m.tracks.len().saturating_sub(1);
        m.index = start_index.min(cap);
        m.cookies = cookies;
    }
    let _ = resolver_tx().send(Cmd::PlayFrom { position_ms });
}

/// Keep the mirror in sync after a queue mutation (append / reorder) without
/// restarting playback. `current_index` is the index of the now-playing track.
pub fn update_queue(tracks: Vec<Track>, current_index: usize, cookies: Option<String>) {
    let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
    m.tracks = tracks;
    m.index = current_index;
    m.cookies = cookies;
}

/// ExoPlayer transitioned onto `media_id`; refill the look-ahead window.
pub fn on_transition(media_id: &str) {
    let _ = resolver_tx().send(Cmd::RefillAfter {
        media_id: media_id.to_string(),
    });
}

/// Re-resolve fresh URLs and restart the window (after an expired-URL error).
pub fn reresolve(position_ms: i64) {
    let _ = resolver_tx().send(Cmd::Reresolve { position_ms });
}

/// The queue index currently playing `media_id`, for UI reconciliation.
pub fn queue_index_of(media_id: &str) -> Option<usize> {
    let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
    m.tracks.iter().position(|t| track_id(t) == media_id)
}

pub fn pause() {
    systemint::exo_pause();
}
pub fn resume() {
    systemint::exo_resume();
}
pub fn next() {
    systemint::exo_next();
}
pub fn prev() {
    systemint::exo_prev();
}
pub fn seek_ms(ms: i64) {
    systemint::exo_seek(ms);
}
pub fn set_volume(v: f32) {
    systemint::exo_set_volume(v);
}
pub fn stop() {
    systemint::exo_stop();
}
pub fn position_ms() -> i64 {
    systemint::exo_position()
}
