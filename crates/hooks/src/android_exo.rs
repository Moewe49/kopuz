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
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

/// Upcoming tracks kept resolved ahead of the current one in ExoPlayer's
/// playlist. Bigger = more resilient to a refill hiccup / a background stall
/// (so ExoPlayer has more buffered songs to keep playing through it); smaller =
/// faster start (each resolve is a network call).
///
/// This window is the app's ONLY defence against Android's background rules.
/// Every YouTube stream-resolve path ultimately needs something the OS suspends
/// once the Activity stops — the PO-token minter WebView, and the sig/n decipher
/// solver that runs in the UI WebView — so a refill attempted while backgrounded
/// can simply fail. Pre-resolving deep while still foregrounded (googlevideo
/// URLs stay valid for hours) is what lets a screen-off session play on. 20
/// tracks is roughly an hour of audio; the fill runs behind the fast-started
/// first song and flushes in chunks, so it costs no start latency.
const WINDOW_AHEAD: usize = 20;

/// How many resolved items to hand ExoPlayer at a time while filling the window.
/// Small enough that a deep fill starts feeding the player within seconds, big
/// enough not to cross the JNI boundary per track.
const FLUSH_EVERY: usize = 3;

/// Attempts a single look-ahead slot gets before it is written off as genuinely
/// unplayable (region-locked / removed) and skipped for good.
const MAX_RESOLVE_ATTEMPTS: u32 = 3;

/// How many slots a refill may resolve back-to-back before it starts pacing.
///
/// Enough to get audio out and a couple of tracks queued behind it; everything
/// past that can afford to wait. See [`RESOLVE_SPACING`].
const BURST_FREE: usize = 3;

/// Gap between resolves once a refill is past [`BURST_FREE`].
///
/// A cold start filled the whole look-ahead window as fast as it could — 20
/// tracks, each trying a PO-token mint, ANDROID_VR, TVHTML5 and the bare
/// clients before settling. That is on the order of a hundred requests to
/// YouTube inside five seconds, which reads as exactly what it looks like:
/// YouTube answered "Sign in to confirm you're not a bot" for every single
/// track, and a freshly started app played nothing at all. An instance that had
/// been running for a few minutes was fine on the same account, because tapping
/// a song there costs *one* resolve.
///
/// The window is a latency optimisation. Spending eight seconds on it instead
/// of five costs nothing anyone can hear; tripping the bot check costs
/// playback entirely.
const RESOLVE_SPACING: std::time::Duration = std::time::Duration::from_millis(500);

/// Canonical queue mirror — plain data, owned by the engine thread. Never a Signal.
struct Mirror {
    tracks: Vec<Track>,
    /// Index in `tracks` of the currently-playing track.
    index: usize,
    /// Highest `tracks` index already handed to ExoPlayer.
    resolved_upto: usize,
    cookies: Option<String>,
    /// `offline_tracks` snapshot from the config: id → downloaded file path,
    /// keyed the same way the rest of the app keys it (`path.split(':')[1]`).
    /// A downloaded track resolves to a local file, which needs no network, no
    /// PO token and no decipher — the one playback path Android's background
    /// restrictions cannot touch.
    offline: HashMap<String, String>,
    /// Per-slot failed-resolve counter, so a transient failure is retried on the
    /// next pass while a dead track is eventually skipped. Cleared with `tracks`.
    attempts: HashMap<usize, u32>,
}

fn mirror() -> &'static Mutex<Mirror> {
    static M: OnceLock<Mutex<Mirror>> = OnceLock::new();
    M.get_or_init(|| {
        Mutex::new(Mirror {
            tracks: Vec::new(),
            index: 0,
            resolved_upto: 0,
            cookies: None,
            offline: HashMap::new(),
            attempts: HashMap::new(),
        })
    })
}

/// Bumped whenever the queue is replaced (`play` / `reorder_upcoming`). A deep
/// window fill checks it between tracks and bails when it goes stale, so tapping
/// a new song never waits behind a fill that is already irrelevant.
static QUEUE_GEN: AtomicU64 = AtomicU64::new(0);

/// How far the fast-start scan walks looking for a playable track before it
/// gives up and lets the stall retry handle it.
const MAX_FAST_START_SCAN: usize = 12;
/// Gap between stall retries, and how many to make (≈4 min of trying).
const STALL_RETRY_SECS: u64 = 12;
const MAX_STALL_RETRIES: u32 = 20;

/// `(when to retry, attempt number)` while playback is stalled on a queue whose
/// tracks currently refuse to resolve. `None` = not stalled.
fn stall_retry() -> &'static Mutex<Option<(std::time::Instant, u32)>> {
    static S: OnceLock<Mutex<Option<(std::time::Instant, u32)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

fn next_retry_at() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(STALL_RETRY_SECS)
}

/// Schedule the next stall retry and return its attempt number.
fn arm_stall_retry() -> u32 {
    let mut s = stall_retry().lock().unwrap_or_else(|e| e.into_inner());
    let attempt = match *s {
        Some((_, n)) => n + 1,
        None => 1,
    };
    *s = Some((next_retry_at(), attempt));
    attempt
}

fn clear_stall_retry() {
    *stall_retry().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// True once the pending stall retry is due (and re-arms it, so a retry that
/// itself hangs can't spin the engine loop).
fn stall_retry_due() -> bool {
    let mut s = stall_retry().lock().unwrap_or_else(|e| e.into_inner());
    let Some((at, n)) = *s else { return false };
    if at > std::time::Instant::now() {
        return false;
    }
    *s = Some((next_retry_at(), n));
    true
}

/// (media_id, consecutive-error count) for the currently-failing track, so a
/// dead video is retried once then skipped instead of looping forever.
fn err_tracker() -> &'static Mutex<(String, u32)> {
    static T: OnceLock<Mutex<(String, u32)>> = OnceLock::new();
    T.get_or_init(|| Mutex::new((String::new(), 0)))
}

/// A queue the engine seeded itself (end-of-queue autoradio) — handed to the
/// Dioxus driver so it replaces the UI queue. Behind a lock because it carries
/// owned `Track`s, not a scalar.
fn new_queue_slot() -> &'static Mutex<Option<Vec<Track>>> {
    static Q: OnceLock<Mutex<Option<Vec<Track>>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(None))
}

/// Whether end-of-queue autoradio is enabled (mirrors config.autoradio). Set by
/// the controller so the engine can start the continuation itself — on its own
/// thread, which keeps running in the background (the Dioxus driver doesn't).
static AUTORADIO_ON: AtomicBool = AtomicBool::new(true);

pub fn set_autoradio(on: bool) {
    AUTORADIO_ON.store(on, Ordering::Release);
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
    /// The engine replaced the queue itself (autoradio continuation) — the
    /// driver adopts it as the new UI queue.
    pub new_queue: Option<Vec<Track>>,
}

/// Read the latest playback state (called by the Dioxus driver on its thread).
pub fn take_ui_update() -> UiUpdate {
    let current_index = if INDEX_DIRTY.swap(false, Ordering::AcqRel) {
        Some(CUR_INDEX.load(Ordering::Acquire))
    } else {
        None
    };
    let new_queue = new_queue_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    UiUpdate {
        current_index,
        playing: PLAYING.load(Ordering::Acquire),
        position_ms: POSITION_MS.load(Ordering::Acquire),
        duration_ms: DURATION_MS.load(Ordering::Acquire),
        ended: ENDED_DIRTY.swap(false, Ordering::AcqRel),
        new_queue,
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
                    for ev in coalesce_events(systemint::take_exo_events()) {
                        rt.block_on(handle_event(ev));
                    }
                    // Playback stalled because nothing would resolve (almost
                    // always: the app was backgrounded, so the minter/decipher
                    // WebViews were suspended). Try again — when the app comes
                    // back to the foreground this is what resumes the music by
                    // itself.
                    if stall_retry_due() {
                        eprintln!("[exo] stall retry");
                        rt.block_on(handle_cmd(Cmd::PlayFrom { position_ms: 0 }));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
            });
        tx
    })
}

/// Reduce a batch of buffered ExoPlayer events to the ones that still describe
/// reality.
///
/// While the app is backgrounded Android throttles this engine thread, so
/// ExoPlayer's callbacks pile up unread — a dozen songs' worth of transitions
/// after a long screen-off stretch. Replaying them one by one made the UI race
/// visibly through every track that had already finished ("the old songs skip
/// past when you come back"), and worse, ran a full look-ahead refill per stale
/// event: a dozen pointless resolve storms before the engine caught up.
///
/// Only the LAST transition describes where playback actually is. Anything
/// before it is history. An `Ended` or `Error` that arrived *before* that
/// transition is history too — playback demonstrably moved on past it — while
/// one that arrived *after* is still live and must be handled. `State` is pure
/// position/duration, so only the newest matters.
fn coalesce_events(events: Vec<ExoEvent>) -> Vec<ExoEvent> {
    let last_transition = events
        .iter()
        .rposition(|e| matches!(e, ExoEvent::Transition { .. }));
    let mut out = Vec::new();
    let mut newest_state = None;
    for (i, ev) in events.into_iter().enumerate() {
        match ev {
            ExoEvent::State { .. } => newest_state = Some(ev),
            ExoEvent::Transition { .. } => {
                if Some(i) == last_transition {
                    out.push(ev);
                }
            }
            // Superseded by a later transition → drop; otherwise still current.
            ExoEvent::Ended | ExoEvent::Error { .. } => {
                if last_transition.is_none_or(|t| i > t) {
                    out.push(ev);
                }
            }
        }
    }
    // State last: it only writes atomics the driver reads, and applying it after
    // a transition keeps the freshest position/duration.
    out.extend(newest_state);
    out
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

/// The key a track's downloaded copy is filed under in `config.offline_tracks`
/// — the first path segment after the scheme, for both `ytmusic:<vid>:…` and
/// `soundcloud:<hex>`.
fn offline_key(track: &Track) -> Option<String> {
    let path = track.path.to_string_lossy();
    let (_scheme, rest) = path.split_once(':')?;
    let id = rest.split(':').next()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Resolve one track into an ExoPlayer MediaItem JSON object, or `None`.
/// `index` is the track's position in the mirror — it becomes the ExoPlayer
/// mediaId so a transition maps back to the EXACT queue slot (a track's PATH is
/// not unique: a playlist can hold the same video twice, and matching by path
/// picked the wrong slot → a huge bogus refill range that stalled the engine).
async fn resolve_item(
    track: &Track,
    index: usize,
    cookies: &Option<String>,
    offline: &HashMap<String, String>,
) -> Option<serde_json::Value> {
    let id = track_id(track);
    // A downloaded copy wins over streaming: no network, no PO token, no
    // decipher — so it keeps resolving while the app is backgrounded, where the
    // remote paths can't. (It is also what the desktop controller prefers.)
    let downloaded = offline_key(track)
        .and_then(|k| offline.get(&k).cloned())
        .filter(|p| std::path::Path::new(p).exists());
    let url = if let Some(local) = downloaded {
        format!("file://{local}")
    } else if let Some(vid) = video_id(track) {
        let yt = ::server::ytmusic::YouTubeMusicClient::with_cookies(
            cookies.clone().unwrap_or_default(),
        );
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
        "mediaId": index.to_string(),
        "title": track.title,
        "artist": track.artist,
        "album": track.album,
        "artworkUrl": cover_url(track),
        "durationMs": (track.duration as i64).saturating_mul(1000),
    }))
}

/// How a refill's first batch reaches ExoPlayer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flush {
    /// Append to the existing playlist (normal look-ahead top-up).
    Append,
    /// Drop everything after the current item first (shuffle re-order), then
    /// append. Sent even when nothing resolves, so stale upcoming items go away.
    Replace,
}

/// Fill the look-ahead window with tracks `[from..=to]`, handing them to
/// ExoPlayer in small batches as they resolve and advancing `resolved_upto`
/// as it goes.
///
/// The loop **stops at the first track that fails but still has retries left**,
/// leaving its slot unclaimed so the next pass picks it up again. The previous
/// behaviour advanced `resolved_upto` across the whole range *before* resolving,
/// so any track that failed — a throttled PO mint, a blip — was dropped from the
/// queue for good and the window shrank by one song per hiccup until playback
/// ran dry. A slot that has burned [`MAX_RESOLVE_ATTEMPTS`] is genuinely dead
/// (region-locked, removed) and is skipped permanently instead.
///
/// Bails out early if the queue was replaced underneath it (see [`QUEUE_GEN`]).
async fn refill_window(from: usize, to: usize, cookies: &Option<String>, flush: Flush) {
    let gen_at_start = QUEUE_GEN.load(Ordering::Acquire);
    let (len, offline) = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        (m.tracks.len(), m.offline.clone())
    };
    let to = to.min(len.saturating_sub(1));

    let mut pending: Vec<serde_json::Value> = Vec::new();
    let mut replace_pending = flush == Flush::Replace;
    let mut i = from;
    let mut resolved_in_pass = 0usize;
    while i <= to && i < len {
        // Pace everything past the first few — see [`RESOLVE_SPACING`]. The
        // generation check below then doubles as the post-sleep re-check, since
        // the queue can be replaced while this pass is waiting.
        if resolved_in_pass >= BURST_FREE {
            tokio::time::sleep(RESOLVE_SPACING).await;
        }
        if QUEUE_GEN.load(Ordering::Acquire) != gen_at_start {
            eprintln!("[exo] refill {from}..={to} abandoned — queue changed");
            return;
        }
        let track = {
            let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
            m.tracks.get(i).cloned()
        };
        let Some(track) = track else { break };
        match resolve_item(&track, i, cookies, &offline).await {
            Some(item) => {
                pending.push(item);
                let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                m.attempts.remove(&i);
                m.resolved_upto = m.resolved_upto.max(i);
            }
            None => {
                let tries = {
                    let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                    let n = m.attempts.entry(i).or_insert(0);
                    *n += 1;
                    *n
                };
                if tries < MAX_RESOLVE_ATTEMPTS {
                    eprintln!("[exo] refill: slot {i} unresolved (try {tries}) — retry next pass");
                    break;
                }
                eprintln!("[exo] refill: slot {i} unresolvable after {tries} tries — skipping");
                let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                m.resolved_upto = m.resolved_upto.max(i);
            }
        }
        if pending.len() >= FLUSH_EVERY {
            send_items(std::mem::take(&mut pending), &mut replace_pending);
        }
        resolved_in_pass += 1;
        i += 1;
    }
    if !pending.is_empty() || replace_pending {
        send_items(pending, &mut replace_pending);
    }
}

/// Hand one batch to ExoPlayer. The first batch of a [`Flush::Replace`] refill
/// trims the stale tail; every batch after it appends.
fn send_items(items: Vec<serde_json::Value>, replace_pending: &mut bool) {
    let json = serde_json::Value::Array(items).to_string();
    if *replace_pending {
        *replace_pending = false;
        systemint::exo_replace_upcoming(&json);
    } else {
        systemint::exo_set_upcoming(&json);
    }
}

async fn handle_cmd(cmd: Cmd) {
    let position_ms = match cmd {
        Cmd::PlayFrom { position_ms } | Cmd::Reresolve { position_ms } => position_ms,
        Cmd::ReorderUpcoming => return handle_reorder_upcoming().await,
    };
    let (start, cookies, offline) = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        (m.index, m.cookies.clone(), m.offline.clone())
    };
    // Resolve just the FIRST playable track and start it IMMEDIATELY. Resolving
    // the whole look-ahead window up front meant 6 sequential multi-second YT
    // resolves (~6-40s) before ANY audio — the old song kept playing and the
    // timer didn't reset. Play track 1 after a single resolve, then fill the
    // window in the background.
    let len = mirror()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tracks
        .len();
    let mut first_item = None;
    let mut i = start;
    // Bounded scan: a run this long of unresolvable tracks is a *systemic*
    // failure (backgrounded minter, dead network), not a patch of bad videos.
    // Walking the rest of a 300-track playlist at seconds per failed resolve
    // would wedge the engine for many minutes; the stall retry below recovers
    // instead.
    let scan_end = len.min(start + MAX_FAST_START_SCAN);
    while i < scan_end {
        let track = {
            let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
            m.tracks.get(i).cloned()
        };
        let Some(track) = track else { break };
        if let Some(j) = resolve_item(&track, i, &cookies, &offline).await {
            first_item = Some((j, i));
            break;
        }
        i += 1;
    }
    let Some((item, first)) = first_item else {
        // Nothing resolved. On Android this is usually *temporary*: while the
        // Activity is stopped both the PO-token minter WebView and the decipher
        // solver are suspended, so every remote resolve fails until the app is
        // foregrounded again. Schedule a retry rather than declaring the queue
        // over — that is what makes playback pick itself back up (previously it
        // stayed dead until the user opened the app and pressed play).
        let attempt = arm_stall_retry();
        PLAYING.store(false, Ordering::Release);
        if attempt > MAX_STALL_RETRIES {
            eprintln!("[exo] nothing resolvable from {start} after {attempt} retries; ending");
            clear_stall_retry();
            ENDED_DIRTY.store(true, Ordering::Release);
        } else {
            eprintln!(
                "[exo] nothing resolvable from {start}; retry {attempt} in {STALL_RETRY_SECS}s"
            );
        }
        return;
    };
    clear_stall_retry();
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        m.index = first;
        m.resolved_upto = first;
    }
    eprintln!("[exo] fast-start from {first}");
    systemint::exo_play(
        &serde_json::Value::Array(vec![item]).to_string(),
        0,
        position_ms.max(0),
    );
    set_current_index(first);
    PLAYING.store(true, Ordering::Release);
    // Now fill the look-ahead window behind the playing track. This is the deep
    // pre-resolve that carries a backgrounded session, so it runs to the full
    // WINDOW_AHEAD; it flushes in batches, so ExoPlayer gets its next tracks
    // within seconds rather than at the end of the whole fill.
    eprintln!(
        "[exo] filling window {}..={}",
        first + 1,
        first + WINDOW_AHEAD
    );
    refill_window(first + 1, first + WINDOW_AHEAD, &cookies, Flush::Append).await;
}

/// Shuffle toggled while playing: leave the current ExoPlayer item alone and
/// replace ONLY the upcoming items with the new play order — no re-buffer.
async fn handle_reorder_upcoming() {
    let (from, cookies) = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        (m.index + 1, m.cookies.clone())
    };
    eprintln!("[exo] reorder upcoming from {from}");
    refill_window(from, from + WINDOW_AHEAD, &cookies, Flush::Replace).await;
}

/// End-of-queue continuation: seed a YT radio from the WHOLE finished playlist,
/// on the engine thread (so it works while backgrounded — the Dioxus driver's
/// autoradio doesn't). Publishes the radio as the new UI queue and starts it.
/// Returns true if a radio actually started.
async fn seed_autoradio() -> bool {
    // The finished queue itself, not just its ids: the artist-graph half of the
    // blend works from artist names. Seed selection now lives in
    // `server::recommend`, so both engines spread their seeds the same way
    // instead of each having its own index arithmetic.
    let (finished, exclude, cookies) = {
        let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        if m.tracks.is_empty() {
            return false;
        }
        // Exclude everything that played, keyed for every source (videoId for
        // YouTube, the whole path for SoundCloud / local) rather than only
        // YouTube ids — otherwise a SoundCloud queue seeds nothing and stops
        // dead at the end, which is the "no autoradio after SoundCloud" bug.
        let exclude: std::collections::HashSet<String> = m
            .tracks
            .iter()
            .map(|t| ::server::recommend::track_key(&t.path))
            .collect();
        (m.tracks.clone(), exclude, m.cookies.clone())
    };
    let cookies = cookies.unwrap_or_default();
    // Same blend as the desktop engine: YouTube's radio woven with the
    // ListenBrainz artist graph, both fetched concurrently, the graph on a
    // deadline so a slow lookup never delays the music.
    let radio = ::server::recommend::blended_continuation(&finished, &cookies, &exclude).await;
    if radio.is_empty() {
        return false;
    }
    *new_queue_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(radio.clone());
    QUEUE_GEN.fetch_add(1, Ordering::AcqRel);
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        m.tracks = radio;
        m.index = 0;
        m.resolved_upto = 0;
        m.attempts.clear();
    }
    DURATION_MS.store(0, Ordering::Release);
    let _ = engine_tx().send(Cmd::PlayFrom { position_ms: 0 });
    true
}

async fn handle_event(ev: ExoEvent) {
    match ev {
        ExoEvent::Transition { media_id, .. } => {
            // New track → the previous track's duration must not leak onto it
            // (ExoPlayer reports the fresh one within ~a tick once loaded).
            DURATION_MS.store(0, Ordering::Release);
            let (qidx, refill_from, refill_to, cookies) = {
                let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                // mediaId IS the mirror index (unique, unlike a duplicate path).
                let qidx = media_id
                    .parse::<usize>()
                    .ok()
                    .filter(|&q| q < m.tracks.len());
                if let Some(q) = qidx {
                    m.index = q;
                }
                let target = (m.index + WINDOW_AHEAD).min(m.tracks.len().saturating_sub(1));
                // Refill only the window AHEAD of the current track. Never
                // resolve a big gap behind (a stale index once made this resolve
                // 47 tracks in a row and froze the engine for ~40s → the "random
                // pauses"). `resolved_upto` is advanced by the refill itself,
                // per track that actually resolved.
                let from = (m.resolved_upto + 1).max(m.index + 1);
                (qidx, from, target, m.cookies.clone())
            };
            if let Some(q) = qidx {
                set_current_index(q);
            }
            // Clear the failure run only when we actually landed somewhere ELSE.
            //
            // The intent — a track that hiccuped once and then played fine
            // shouldn't be skipped later — is right, but "a transition happened"
            // does not mean "it played fine". A re-resolve produces a transition
            // of its own, back into the very item that just failed, and clearing
            // here reset the counter between every failure: the retry-once guard
            // never reached two, so a track whose URL 403s immediately looped
            // forever at roughly one attempt every five seconds, showing the user
            // nothing but a player that refuses to start.
            //
            // Landing on a different item is the honest signal that the previous
            // run is over.
            {
                let mut t = err_tracker().lock().unwrap_or_else(|e| e.into_inner());
                let landed_on = qidx.map(|q| q.to_string()).unwrap_or_default();
                if t.0 != landed_on {
                    *t = (String::new(), 0);
                }
            }
            eprintln!("[exo] transition -> qidx {qidx:?}, refill [{refill_from}..={refill_to}]");
            if refill_to >= refill_from {
                refill_window(refill_from, refill_to, &cookies, Flush::Append).await;
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
            let (idx, len) = {
                let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                (m.index, m.tracks.len())
            };
            // Say what the decision was made on. Without these two numbers an
            // `Ended` is unattributable: the queue collapsing into autoradio and
            // restarting at 0 looked from the outside like the display being
            // stuck on one song.
            eprintln!("[exo] ended at idx {idx} of {len}");

            // Is ExoPlayer out of items because the QUEUE ended, or because the
            // look-ahead window hasn't been handed over yet?
            //
            // A fast-start gives ExoPlayer the current track alone and appends
            // the rest as each one resolves. Reaching the end of that one-item
            // queue is not the end of the playlist — but it produces a real
            // `Ended`, and reading it as "last track" seeds autoradio and
            // restarts at 0, which fast-starts again, forever.
            //
            // `resolved_upto` answers this exactly, with no clock involved: it
            // is the highest index already given to ExoPlayer. If the mirror
            // holds tracks past it, the window is simply still filling.
            //
            // (The first attempt at this used a five-second grace window after
            // the last fast-start. The store that recorded that timestamp never
            // made it into the file, so the sentinel stayed at i64::MIN and
            // `now_ms() - i64::MIN` overflowed to a negative number — the guard
            // then swallowed EVERY end-of-song and playback simply stopped.
            // A condition that reads real state cannot fail that way.)
            let window_incomplete = {
                let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                m.resolved_upto + 1 < m.tracks.len()
            };
            if window_incomplete {
                eprintln!("[exo] ended while the window was still filling — recovering");
                let _ = engine_tx().send(Cmd::PlayFrom { position_ms: 0 });
                return;
            }

            let has_more = idx + 1 < len;
            if has_more {
                {
                    let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                    m.index += 1;
                }
                eprintln!("[exo] ended early with tracks remaining — recovering");
                let _ = engine_tx().send(Cmd::PlayFrom { position_ms: 0 });
            } else if AUTORADIO_ON.load(Ordering::Acquire) && seed_autoradio().await {
                // Seeded a radio continuation ON THIS THREAD — works even while
                // the app is backgrounded (the Dioxus driver, which runs the
                // desktop autoradio, is suspended then). The driver adopts the
                // new queue into the UI via take_ui_update().new_queue.
                eprintln!("[exo] queue ended — autoradio continuation started");
            } else {
                PLAYING.store(false, Ordering::Release);
                ENDED_DIRTY.store(true, Ordering::Release);
                eprintln!("[exo] queue truly ended");
            }
        }
        ExoEvent::Error { media_id, code } => {
            // Retry the SAME track once (a transient 403 / stale URL re-resolves
            // fine); if it errors again the track is genuinely broken (region-
            // locked / removed) → SKIP it. Without this a single dead video
            // looped forever (re-resolve → error → re-resolve …), which is the
            // real "playback stops after a few songs" — it got stuck on a bad
            // track. The counter keys on media_id, so a different failing track
            // starts fresh.
            let tries = {
                let mut t = err_tracker().lock().unwrap_or_else(|e| e.into_inner());
                if t.0 == media_id {
                    t.1 += 1;
                } else {
                    t.0 = media_id.clone();
                    t.1 = 1;
                }
                t.1
            };
            if tries <= 1 {
                eprintln!("[exo] error {code} on {media_id} — reresolve (try {tries})");
                // Drop the cached URL first. A playback error means THAT url is
                // dead (403 on a deep range, expired signature); without this the
                // 2h stream cache handed the retry the identical broken URL, so
                // the "retry once" never actually retried anything and the track
                // was skipped on the second error.
                {
                    let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(vid) = media_id
                        .parse::<usize>()
                        .ok()
                        .and_then(|q| m.tracks.get(q))
                        .and_then(video_id)
                    {
                        ::server::ytmusic::invalidate_stream(&vid);
                    }
                }
                let pos = POSITION_MS.load(Ordering::Acquire);
                let _ = engine_tx().send(Cmd::Reresolve { position_ms: pos });
            } else {
                let (skip_from, has_more) = {
                    let m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                    // mediaId is the mirror index.
                    let idx = media_id
                        .parse::<usize>()
                        .ok()
                        .filter(|&q| q < m.tracks.len())
                        .unwrap_or(m.index);
                    (idx + 1, idx + 1 < m.tracks.len())
                };
                if has_more {
                    eprintln!("[exo] error {code} on {media_id} — skipping to {skip_from}");
                    {
                        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
                        m.index = skip_from;
                    }
                    let _ = engine_tx().send(Cmd::PlayFrom { position_ms: 0 });
                } else {
                    eprintln!("[exo] error {code} on {media_id} — last track, ending");
                    PLAYING.store(false, Ordering::Release);
                    ENDED_DIRTY.store(true, Ordering::Release);
                }
            }
        }
    }
}

// --- Public API (called on the Dioxus thread from the controller / driver) ---

/// Start (or restart) playback of `tracks` from `start_index`. `offline` is the
/// config's `offline_tracks` snapshot so the engine can prefer downloaded files.
pub fn play(
    tracks: Vec<Track>,
    start_index: usize,
    cookies: Option<String>,
    offline: HashMap<String, String>,
    position_ms: i64,
) {
    QUEUE_GEN.fetch_add(1, Ordering::AcqRel);
    clear_stall_retry();
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        let cap = tracks.len().saturating_sub(1);
        m.tracks = tracks;
        m.index = start_index.min(cap);
        m.resolved_upto = m.index;
        m.cookies = cookies;
        m.offline = offline;
        m.attempts.clear();
    }
    DURATION_MS.store(0, Ordering::Release);
    // Stop the OLD track instantly so it doesn't keep playing (and the timer
    // resets) while the new one resolves (~seconds). cmdPlay repopulates it.
    if position_ms <= 0 {
        POSITION_MS.store(0, Ordering::Release);
        systemint::exo_clear();
    }
    let _ = engine_tx().send(Cmd::PlayFrom { position_ms });
}

/// Shuffle toggled during playback: swap the mirror to the new play order but
/// keep the CURRENT track playing and only rebuild the upcoming ExoPlayer items
/// (no re-buffer / no gap). `current_idx` is the current track's play-order
/// index in `tracks` (so `tracks[current_idx]` is the now-playing song).
pub fn reorder_upcoming(
    tracks: Vec<Track>,
    current_idx: usize,
    cookies: Option<String>,
    offline: HashMap<String, String>,
) {
    QUEUE_GEN.fetch_add(1, Ordering::AcqRel);
    {
        let mut m = mirror().lock().unwrap_or_else(|e| e.into_inner());
        let cap = tracks.len().saturating_sub(1);
        m.tracks = tracks;
        m.index = current_idx.min(cap);
        m.resolved_upto = m.index;
        m.cookies = cookies;
        m.offline = offline;
        m.attempts.clear();
    }
    let _ = engine_tx().send(Cmd::ReorderUpcoming);
}

pub fn pause() {
    PLAYING.store(false, Ordering::Release);
    // A paused user is not a stalled queue — stop retrying behind their back.
    clear_stall_retry();
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
    clear_stall_retry();
    systemint::exo_stop();
}
