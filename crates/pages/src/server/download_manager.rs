use config::{AppConfig, MusicService};
use dioxus::prelude::*;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub use ::server::{DownloadItem, DownloadProgress, DownloadQueue, DownloadStatus};

thread_local! {
    static DOWNLOAD_PROGRESS: Cell<Option<Signal<DownloadProgress>>> = const { Cell::new(None) };
}

/// Epoch-millis until which ALL workers pause. Set on rate-limit-shaped
/// failures and on scheduled batch breathers. Global, not per-worker — once
/// YT throttles the session, every worker pushing on just digs deeper.
#[cfg(not(target_arch = "wasm32"))]
static COOLDOWN_UNTIL_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Completed downloads this session — drives the every-50-tracks breather.
#[cfg(not(target_arch = "wasm32"))]
static SESSION_COMPLETED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(not(target_arch = "wasm32"))]
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Extend the global cooldown to at least `now + secs` (never shortens it).
#[cfg(not(target_arch = "wasm32"))]
fn set_cooldown(secs: u64) {
    let until = epoch_ms() + secs * 1000;
    COOLDOWN_UNTIL_MS.fetch_max(until, Ordering::Relaxed);
}

pub fn register_progress_signal(signal: Signal<DownloadProgress>) {
    DOWNLOAD_PROGRESS.with(|s| s.set(Some(signal)));
}

fn progress_signal() -> Option<Signal<DownloadProgress>> {
    DOWNLOAD_PROGRESS.with(|s| s.get())
}

fn publish_progress(item_id: &str, bytes_done: u64, bytes_delta: u64, elapsed_secs: f64) {
    let Some(mut p) = progress_signal() else {
        return;
    };
    let mut state = p.write();
    state.per_item.insert(item_id.to_string(), bytes_done);
    state.bytes_done_session += bytes_delta;
    state.session_elapsed_secs = elapsed_secs;
}

fn clear_progress(item_id: &str) {
    let Some(mut p) = progress_signal() else {
        return;
    };
    p.write().per_item.remove(item_id);
}

fn reset_progress_session() {
    let Some(mut p) = progress_signal() else {
        return;
    };
    let mut state = p.write();
    state.bytes_done_session = 0;
    state.session_elapsed_secs = 0.0;
}

/// Queue tracks for download into the base downloads folder.
#[cfg(not(target_arch = "wasm32"))]
pub fn queue_downloads(
    requests: Vec<(String, String, String)>,
    config: Signal<AppConfig>,
    queue: Signal<DownloadQueue>,
) {
    queue_downloads_into(requests, None, config, queue);
}

/// Queue tracks for download into `<downloads>/<subdir>/` when `subdir` is
/// set (e.g. a playlist name), else the base downloads folder.
#[cfg(not(target_arch = "wasm32"))]
pub fn queue_downloads_into(
    requests: Vec<(String, String, String)>,
    subdir: Option<String>,
    config: Signal<AppConfig>,
    mut queue: Signal<DownloadQueue>,
) {
    let mut added = false;
    let cancel_flag: Arc<AtomicBool>;
    {
        let mut q = queue.write();
        let conf = config.peek();
        let queued_ids: std::collections::HashSet<String> =
            q.items.iter().map(|i| i.id.clone()).collect();

        for (id, title, artist) in &requests {
            if conf.offline_tracks.contains_key(id) {
                continue;
            }
            if queued_ids.contains(id) {
                continue;
            }
            q.items.push(DownloadItem {
                id: id.clone(),
                title: title.clone(),
                artist: artist.clone(),
                status: DownloadStatus::Queued,
                bytes_done: 0,
                bytes_total: 0,
                error: None,
                subdir: subdir.clone(),
                requeues: 0,
            });
            added = true;
        }

        // A session counts as LIVE only if its workers are demonstrably alive
        // (something is in Downloading state). `is_running` alone is not
        // trustworthy: a session whose task got cancelled (see spawn_forever
        // note below) leaves it stuck `true`, and gating on it permanently
        // bricked the download button — items queued forever, 0 downloading.
        let live = q.is_running
            && q.items
                .iter()
                .any(|i| matches!(i.status, DownloadStatus::Downloading));
        if live {
            // Running workers claim any Queued item, including ones just added.
            let _ = added;
            return;
        }
        let has_queued = q
            .items
            .iter()
            .any(|i| matches!(i.status, DownloadStatus::Queued));
        if !has_queued {
            return;
        }
        // Reset cancel flags only once we're sure we're actually starting
        // a fresh worker session. Replacing the Arc gives any still-living
        // worker from a prior cancelled session its own (still-set) flag
        // so it terminates instead of resuming on the new session's reset
        // signal.
        q.cancel_requested = false;
        q.cancel_flag = Arc::new(AtomicBool::new(false));
        cancel_flag = q.cancel_flag.clone();
        q.is_running = true;
    }

    reset_progress_session();
    SESSION_COMPLETED.store(0, Ordering::Relaxed);

    let session_start = Instant::now();
    // spawn_forever, NOT spawn: a scoped task dies when the page that queued
    // the downloads unmounts — navigating into the playlist (or anywhere)
    // killed the workers mid-run and left the queue frozen at "N queued".
    // The session must belong to the app, not to whichever page started it.
    dioxus::core::spawn_forever(async move {
        // 2 parallel workers. More throughput is tempting, but YouTube
        // bot-checks per IP under load — and once the IP is flagged BOTH
        // downloads AND playback fail (the LOGIN_REQUIRED you hit while a batch
        // ran). 2 workers + yt-dlp's own parallel fragments keep the pipe busy
        // while leaving the IP enough headroom that music keeps playing.
        tokio::join!(
            download_worker(queue, config, session_start, cancel_flag.clone()),
            download_worker(queue, config, session_start, cancel_flag.clone()),
        );

        let mut q = queue.write();
        q.is_running = false;
        q.cancel_requested = false;
    });
}

#[cfg(not(target_arch = "wasm32"))]
async fn download_worker(
    mut queue: Signal<DownloadQueue>,
    mut config: Signal<AppConfig>,
    session_start: Instant,
    cancel_flag: Arc<AtomicBool>,
) {
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }
        // Honor the GLOBAL cooldown (rate-limit recovery / batch breather).
        // All workers pause together — once YT throttles the session, any
        // worker pushing on just extends the throttle.
        let until = COOLDOWN_UNTIL_MS.load(Ordering::Relaxed);
        let now = epoch_ms();
        if until > now {
            tokio::time::sleep(std::time::Duration::from_millis(until - now)).await;
            continue;
        }

        // Atomic claim: find + status flip in one write lock prevents two workers
        // grabbing the same id. Grab the metadata too so the file can be named
        // `Artist - Title.ext` instead of an opaque video id.
        let claimed = {
            let mut q = queue.write();
            let claimed = q
                .items
                .iter_mut()
                .find(|i| matches!(i.status, DownloadStatus::Queued));
            match claimed {
                Some(item) => {
                    item.status = DownloadStatus::Downloading;
                    Some((
                        item.id.clone(),
                        item.title.clone(),
                        item.artist.clone(),
                        item.subdir.clone(),
                        item.requeues,
                    ))
                }
                None => None,
            }
        };
        let Some((id, title, artist, subdir, requeues)) = claimed else {
            // Nothing claimable — but another worker's in-flight track may
            // yet get REQUEUED (rate-limit recovery). Only exit once nothing
            // is downloading anymore; otherwise idle and re-check.
            let anyone_active = queue
                .read()
                .items
                .iter()
                .any(|i| matches!(i.status, DownloadStatus::Downloading));
            if anyone_active {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            return;
        };

        if config.read().offline_tracks.contains_key(&id) {
            if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                item.status = DownloadStatus::Done;
            }
            continue;
        }

        let (service, yt_cookies) = {
            let conf = config.read();
            let s = conf.server.as_ref();
            (
                s.map(|x| x.service),
                s.and_then(|x| x.access_token.clone()),
            )
        };

        // Save into the browsable downloads folder as `Artist - Title.ext`.
        // Playlist downloads carry a `subdir` so they group under a per-playlist
        // folder inside the downloads dir.
        let dest_no_ext = {
            let conf = config.read();
            let mut dir = super::downloads_dir(&conf);
            if let Some(sub) = subdir.as_deref().filter(|s| !s.trim().is_empty()) {
                dir = dir.join(super::sanitize_filename(sub));
                let _ = std::fs::create_dir_all(&dir);
            }
            super::download_dest_no_ext(&dir, &artist, &title, &id)
        };

        // Per-track attempt ladder:
        //   1-2: yt-dlp end-to-end m4a download (when yt-dlp is installed) —
        //        clean, playable, seekable file with embedded cover; its own
        //        bot-check + retries + parallel fragments. Primary path.
        //   3:   native resolve + our chunked transfer, then search-by-title
        //        fallback. Also the path for users without yt-dlp.
        let ytdlp_available = ::server::ytmusic::ytdlp_resolve::find_ytdlp().is_some();
        const MAX_ATTEMPTS: u32 = 3;
        let mut last_err = String::new();
        let mut done = false;
        for attempt in 1..=MAX_ATTEMPTS {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }

            let is_yt = matches!(service, Some(MusicService::YtMusic));
            // yt-dlp end-to-end is the PRIMARY path for YT downloads when it's
            // installed: it fetches a clean m4a (playable/seekable, with a real
            // duration and embedded cover), handles its own bot-check + retries,
            // and downloads fragments in parallel (-N 8). The native chunked
            // path (attempt 3) stays as a fallback for users without yt-dlp.
            let use_ytdlp = is_yt && ytdlp_available && attempt <= 2;
            let outcome: Result<std::path::PathBuf, String> = if use_ytdlp {
                // A partial file from a prior attempt must not fool yt-dlp into
                // thinking the track is already downloaded.
                remove_stem_files(&dest_no_ext);
                // Default ANONYMOUS: hammering googlevideo with many parallel
                // signed-in downloads can bot-flag the ACCOUNT, which then
                // breaks PLAYBACK (LOGIN_REQUIRED) mid-batch. Anonymous keeps
                // the session clean at the cost of 128k vs Premium 256k. The
                // `premium_downloads` opt-in passes cookies to fetch itag 141
                // (256k) for users who accept that trade.
                let dl_cookies = if config.peek().premium_downloads {
                    yt_cookies.clone()
                } else {
                    None
                };
                ytdlp_download_with_progress(
                    &id,
                    &dest_no_ext,
                    dl_cookies.as_deref(),
                    &mut queue,
                    &session_start,
                )
                .await
            } else {
                let resolved: Option<(String, &'static str, Option<String>, Option<u64>)> =
                    if is_yt {
                        let cookies = yt_cookies.clone().unwrap_or_default();
                        let yt = ::server::ytmusic::YouTubeMusicClient::with_cookies(cookies);
                        // HARD timeout on every resolve. The resolve chain has no
                        // internal deadline — when YT throttles a long batch, a
                        // hung resolve froze a worker (the "stops at ~100" bug).
                        const RESOLVE_TIMEOUT: std::time::Duration =
                            std::time::Duration::from_secs(90);
                        let info = if attempt == 1 {
                            tokio::time::timeout(RESOLVE_TIMEOUT, yt.get_stream(&id))
                                .await
                                .ok()
                                .and_then(|r| r.ok())
                        } else {
                            // A URL that 403'd is dead — never retry it from cache.
                            tokio::time::timeout(RESOLVE_TIMEOUT, yt.get_stream_fresh(&id))
                                .await
                                .ok()
                                .and_then(|r| r.ok())
                        };
                        let info = match info {
                            Some(i) => Some(i),
                            // Stored id unavailable (region/age-locked/removed)?
                            // Search title+artist and stream the top playable
                            // alternative — the recovery the user does by hand.
                            None if attempt == MAX_ATTEMPTS => {
                                eprintln!(
                                    "Download resolve failed for {id} (YT) — trying search fallback"
                                );
                                tokio::time::timeout(
                                    std::time::Duration::from_secs(120),
                                    resolve_via_search(&yt, &title, &artist, &id),
                                )
                                .await
                                .ok()
                                .flatten()
                            }
                            None => None,
                        };
                        info.map(|info| {
                            (
                                info.url,
                                info.format.extension(),
                                Some(info.user_agent),
                                info.content_length,
                            )
                        })
                    } else {
                        let conf = config.read();
                        super::build_download_url(&id, &conf).map(|(u, ext)| (u, ext, None, None))
                    };

                match resolved {
                    None => Err(if is_yt {
                        "Could not resolve a stream — the track may be unavailable in your region, age-restricted, or yt-dlp needs updating.".to_string()
                    } else {
                        "Could not build a download URL for this track.".to_string()
                    }),
                    Some((url, ext_hint, user_agent, content_length)) => {
                        download_with_progress(
                            &id,
                            &url,
                            ext_hint,
                            &dest_no_ext,
                            user_agent.as_deref(),
                            content_length,
                            &mut queue,
                            &session_start,
                            &cancel_flag,
                        )
                        .await
                    }
                }
            };

            // Integrity gate: a transfer cut off before the trailing moov atom
            // yields a file that "downloaded" but won't decode/seek at play
            // time. Catch it now (cheap probe) so broken files don't silently
            // pile up — treat a bad file as a failed attempt and retry.
            let outcome = match outcome {
                Ok(path) if !verify_audio_file(&path) => {
                    let _ = std::fs::remove_file(&path);
                    Err("downloaded file failed integrity check (truncated)".to_string())
                }
                other => other,
            };

            match outcome {
                Ok(path) => {
                    config
                        .write()
                        .offline_tracks
                        .insert(id.clone(), path.to_string_lossy().into_owned());
                    if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                        item.status = DownloadStatus::Done;
                    }
                    clear_progress(&id);
                    done = true;
                    SESSION_COMPLETED.fetch_add(1, Ordering::Relaxed);
                    // No scheduled breather and no inter-track delay: anonymous
                    // yt-dlp end-to-end with -N 8 manages its own throttling, so
                    // the heavy custom pauses (which felt like "random pauses"
                    // and stalls) aren't needed. We only cool down REACTIVELY if
                    // a track actually hits a rate limit (below).
                    break;
                }
                Err(e) => {
                    if e == "cancelled" {
                        last_err = e;
                        break;
                    }
                    eprintln!("Download attempt {attempt}/{MAX_ATTEMPTS} failed for {id}: {e}");
                    last_err = e;
                    // Back off before the next attempt — a 403 streak usually
                    // means we're being rate-limited right now.
                    tokio::time::sleep(std::time::Duration::from_millis(
                        1000 * attempt as u64,
                    ))
                    .await;
                }
            }
        }
        if !done {
            // Don't leave a truncated file behind — anything with this stem is
            // ours (download_dest_no_ext picked a collision-free name).
            remove_stem_files(&dest_no_ext);
            // Rate-limit-shaped failures (403/429/timeouts/no-resolve) poison
            // the SESSION, not the track — a track failed during a throttle
            // window would very likely succeed later. Requeue it (capped) and
            // give the whole session a cooldown, instead of permanently
            // failing everything that had the bad luck of being processed
            // during the throttle. That was the "one error and the rest of
            // the download is ruined" behavior.
            let rate_limited = last_err.contains("403")
                || last_err.contains("429")
                || last_err.contains("timed out")
                || last_err.contains("Could not resolve");
            let cancelled = last_err == "cancelled" || cancel_flag.load(Ordering::Relaxed);
            if rate_limited && !cancelled && requeues < 2 {
                eprintln!(
                    "[downloads] {id} hit rate limit ({last_err}) — requeueing (attempt {}), 15s session cooldown",
                    requeues + 1
                );
                set_cooldown(15);
                if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                    item.status = DownloadStatus::Queued;
                    item.requeues += 1;
                    item.error = None;
                }
            } else if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                item.status = DownloadStatus::Failed;
                item.error = Some(last_err);
            }
            clear_progress(&id);
        }
    }
}

/// Cheap validity probe for a freshly downloaded audio file: it must parse and
/// report a non-zero duration. Catches truncated/interrupted transfers (missing
/// trailing moov atom) that "complete" but won't play. Downloads are m4a, which
/// lofty reads; a webm from the rare native fallback that lofty can't parse is
/// rejected and re-downloaded (cheap, and yt-dlp's m4a path usually succeeds).
#[cfg(not(target_arch = "wasm32"))]
fn verify_audio_file(path: &std::path::Path) -> bool {
    use lofty::file::AudioFile;
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) < 4096 {
        return false;
    }
    lofty::probe::Probe::open(path)
        .and_then(|p| p.read())
        .map(|tagged| tagged.properties().duration().as_secs() > 0)
        .unwrap_or(false)
}

/// Largest on-disk size of any file sharing `dest_no_ext`'s stem — the growing
/// `<stem>.<ext>.part` (or final file) yt-dlp is writing. Used to drive a live
/// progress bar for the yt-dlp download path, which otherwise sat at 0 bytes
/// until completion (looked frozen).
#[cfg(not(target_arch = "wasm32"))]
fn largest_stem_file_size(dest_no_ext: &std::path::Path) -> u64 {
    let (Some(dir), Some(stem)) = (
        dest_no_ext.parent(),
        dest_no_ext.file_name().and_then(|f| f.to_str()),
    ) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut max = 0u64;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && name.starts_with(stem)
            && let Ok(meta) = entry.metadata()
        {
            max = max.max(meta.len());
        }
    }
    max
}

/// Run the yt-dlp end-to-end download while polling the partial file so the UI
/// shows live bytes (yt-dlp's own progress isn't wired to our publisher). Caps
/// the whole thing at 600s.
#[cfg(not(target_arch = "wasm32"))]
async fn ytdlp_download_with_progress(
    id: &str,
    dest_no_ext: &std::path::Path,
    cookies: Option<&str>,
    queue: &mut Signal<DownloadQueue>,
    session_start: &Instant,
) -> Result<std::path::PathBuf, String> {
    let dl = ::server::ytmusic::ytdlp_resolve::download(id, dest_no_ext, cookies);
    tokio::pin!(dl);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(600));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            r = &mut dl => return r,
            _ = &mut deadline => return Err("yt-dlp download timed out after 600s".to_string()),
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                let bytes = largest_stem_file_size(dest_no_ext);
                if bytes > 0 {
                    if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                        item.bytes_done = bytes;
                    }
                    publish_progress(id, bytes, 0, session_start.elapsed().as_secs_f64());
                }
            }
        }
    }
}

/// Remove every file sharing `dest_no_ext`'s stem (any extension, including
/// yt-dlp `.part` leftovers). Safe: download_dest_no_ext picked a
/// collision-free stem, so anything matching is ours.
#[cfg(not(target_arch = "wasm32"))]
fn remove_stem_files(dest_no_ext: &std::path::Path) {
    let (Some(dir), Some(stem)) = (
        dest_no_ext.parent(),
        dest_no_ext.file_name().and_then(|f| f.to_str()),
    ) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && (name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name) == stem
                || name.starts_with(&format!("{stem}.")))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// When a stored YT video id won't resolve (unavailable / age- or
/// region-locked / removed), search the catalog for `title artist` and return
/// the first alternative that DOES resolve — the same recovery the user does
/// by hand from the search tab. Skips the id that already failed.
#[cfg(not(target_arch = "wasm32"))]
async fn resolve_via_search(
    yt: &::server::ytmusic::YouTubeMusicClient,
    title: &str,
    artist: &str,
    failed_id: &str,
) -> Option<::server::ytmusic::YtStreamInfo> {
    let query = format!("{title} {artist}");
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let results = yt.search_tracks(query).await.ok()?;
    for t in results.into_iter().take(5) {
        let vid = t
            .path
            .to_string_lossy()
            .strip_prefix("ytmusic:")
            .and_then(|s| s.split(':').next())
            .map(|s| s.to_string());
        let Some(vid) = vid.filter(|v| !v.is_empty() && v != failed_id) else {
            continue;
        };
        if let Ok(info) = yt.get_stream(&vid).await {
            return Some(info);
        }
    }
    None
}

#[cfg(not(target_arch = "wasm32"))]
pub fn delete_downloads(
    ids: Vec<String>,
    mut config: Signal<AppConfig>,
    mut queue: Signal<DownloadQueue>,
) {
    let mut conf = config.write();
    let mut q = queue.write();

    for id in ids {
        if let Some(path_str) = conf.offline_tracks.remove(&id) {
            let path = std::path::Path::new(&path_str);
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
        }
        q.items.retain(|i| i.id != id);
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn download_with_progress(
    item_id: &str,
    url: &str,
    ext_hint: &'static str,
    dest_no_ext: &std::path::Path,
    user_agent: Option<&str>,
    content_length: Option<u64>,
    queue: &mut Signal<DownloadQueue>,
    session_start: &Instant,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<std::path::PathBuf, String> {
    use tokio::io::AsyncWriteExt;

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .tcp_nodelay(true)
        .build()
        .map_err(|e| format!("Client build error: {e}"))?;

    // Destination is `<downloads>/Artist - Title.<ext>`; the stem was already
    // resolved (collision-free) by the caller — we just attach the extension.
    let with_ext = |ext: &str| dest_no_ext.with_extension(ext);
    let file_path_tentative = with_ext(ext_hint);

    // YT googlevideo URLs throttle single sequential GETs to ~1 MB/s; Range-chunking
    // sidesteps the throttle and saturates the link.
    if let (Some(ua), Some(total)) = (user_agent, content_length) {
        let file_path = with_ext(ext_hint);
        let file = tokio::fs::File::create(&file_path)
            .await
            .map_err(|e| format!("Create file: {e}"))?;
        let mut writer = tokio::io::BufWriter::with_capacity(256 * 1024, file);

        {
            let mut q = queue.write();
            if let Some(item) = q.items.iter_mut().find(|i| i.id == item_id) {
                item.bytes_total = total;
            }
        }

        const CHUNK: u64 = 512 * 1024;
        const RANGE_TIMEOUT_SECS: u64 = 60;
        const UI_UPDATE_MS: u128 = 50;

        let mut start = 0u64;
        let mut bytes_done = 0u64;
        let mut last_update_at = Instant::now();
        let mut last_update_bytes = 0u64;
        let mut first_update_done = false;

        // Transient-blip retries per range. A single 403/429/5xx or network
        // hiccup must not kill a download that's 90% done — wait briefly and
        // re-request the same range. If it STILL fails after the retries the
        // URL itself is dead (expired / rate-limited), and we bubble the error
        // up so the worker re-resolves a fresh URL and restarts the track.
        const RANGE_RETRIES: u32 = 3;

        while start < total {
            if cancel_flag.load(Ordering::Relaxed) {
                drop(writer);
                let _ = tokio::fs::remove_file(&file_path).await;
                return Err("cancelled".to_string());
            }

            let end = (start + CHUNK - 1).min(total - 1);
            let mut range_attempt = 0u32;
            let resp = loop {
                range_attempt += 1;
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(RANGE_TIMEOUT_SECS),
                    client
                        .get(url)
                        .header(reqwest::header::USER_AGENT, ua)
                        .header("Range", format!("bytes={start}-{end}"))
                        .send(),
                )
                .await;
                let err = match result {
                    Ok(Ok(resp)) if resp.status().is_success() => break Ok(resp),
                    Ok(Ok(resp)) => format!("HTTP {} on range {start}-{end}", resp.status()),
                    Ok(Err(e)) => format!("Range request failed: {e}"),
                    Err(_) => format!("range request timed out after {RANGE_TIMEOUT_SECS}s"),
                };
                if range_attempt > RANGE_RETRIES || cancel_flag.load(Ordering::Relaxed) {
                    break Err(err);
                }
                tokio::time::sleep(std::time::Duration::from_millis(
                    600 * range_attempt as u64,
                ))
                .await;
            };
            let resp = resp?;
            let status = resp.status();
            // Defensive: a CDN edge ignoring the Range header and
            // returning 200 (full body) plus a CONTENT_LENGTH equal
            // to `total` would otherwise let us write the whole file
            // every iteration (quadratic growth, fills disk). Require
            // 206 Partial Content explicitly.
            if status != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err(format!(
                    "expected 206 Partial Content but got {status} on range {start}-{end} — server ignored Range header"
                ));
            }

            // The send() above is deadline-capped but the BODY read wasn't —
            // a throttled connection could stall here forever.
            let bytes = tokio::time::timeout(
                std::time::Duration::from_secs(RANGE_TIMEOUT_SECS),
                resp.bytes(),
            )
            .await
            .map_err(|_| format!("range body read timed out after {RANGE_TIMEOUT_SECS}s"))?
            .map_err(|e| format!("Range read error: {e}"))?;
            let expected_len = end - start + 1;
            // Defensive: a short read (network hiccup mid-Range)
            // would otherwise advance `start = end + 1` past where
            // bytes actually landed, leaving a zero-filled hole in
            // the output file. Reject and let the retry loop above
            // do its job.
            if bytes.len() as u64 != expected_len {
                return Err(format!(
                    "short read on range {start}-{end}: got {} bytes, expected {expected_len}",
                    bytes.len()
                ));
            }

            writer
                .write_all(&bytes)
                .await
                .map_err(|e| format!("Write: {e}"))?;

            bytes_done += bytes.len() as u64;
            start = end + 1;

            let now = Instant::now();
            let push = !first_update_done
                || now.duration_since(last_update_at).as_millis() >= UI_UPDATE_MS
                || start >= total;
            if push {
                let elapsed = session_start.elapsed().as_secs_f64();
                let trailing = bytes_done - last_update_bytes;
                publish_progress(item_id, bytes_done, trailing, elapsed);
                last_update_at = now;
                last_update_bytes = bytes_done;
                first_update_done = true;
            }
        }

        writer
            .flush()
            .await
            .map_err(|e| format!("Flush: {e}"))?;
        let trailing = bytes_done.saturating_sub(last_update_bytes);
        publish_progress(item_id, bytes_done, trailing, session_start.elapsed().as_secs_f64());
        return Ok(file_path);
    }

    let mut req = client.get(url);
    if let Some(ua) = user_agent {
        req = req.header(reqwest::header::USER_AGENT, ua);
    }
    let mut response = req
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let total_bytes = response.content_length().unwrap_or(0);
    let ext = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(super::content_type_to_ext)
        .unwrap_or(ext_hint);

    let file_path = if ext == ext_hint {
        file_path_tentative
    } else {
        with_ext(ext)
    };

    {
        let mut q = queue.write();
        if let Some(item) = q.items.iter_mut().find(|i| i.id == item_id) {
            item.bytes_total = total_bytes;
        }
    }

    let file = tokio::fs::File::create(&file_path)
        .await
        .map_err(|e| format!("Create file: {e}"))?;
    let mut writer = tokio::io::BufWriter::with_capacity(256 * 1024, file);

    let mut bytes_done = 0u64;
    let mut last_update_at = Instant::now();
    let mut last_update_bytes = 0u64;
    let mut first_update_done = false;
    const UI_UPDATE_MS: u128 = 50;
    const CHUNK_TIMEOUT_SECS: u64 = 120;

    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            drop(writer);
            let _ = tokio::fs::remove_file(&file_path).await;
            return Err("cancelled".to_string());
        }

        let chunk_result = tokio::time::timeout(
            std::time::Duration::from_secs(CHUNK_TIMEOUT_SECS),
            response.chunk(),
        )
        .await
        .map_err(|_| format!("chunk timed out after {CHUNK_TIMEOUT_SECS}s"))?
        .map_err(|e| format!("Read error: {e}"))?;

        let chunk = match chunk_result {
            Some(c) => c,
            None => break,
        };

        writer
            .write_all(&chunk)
            .await
            .map_err(|e| format!("Write: {e}"))?;
        bytes_done += chunk.len() as u64;

        let now = Instant::now();
        let push = !first_update_done
            || now.duration_since(last_update_at).as_millis() >= UI_UPDATE_MS
            || (total_bytes > 0 && bytes_done == total_bytes);
        if push {
            let elapsed = session_start.elapsed().as_secs_f64();
            let trailing = bytes_done - last_update_bytes;
            publish_progress(item_id, bytes_done, trailing, elapsed);
            last_update_at = now;
            last_update_bytes = bytes_done;
            first_update_done = true;
        }
    }

    writer
        .flush()
        .await
        .map_err(|e| format!("Flush: {e}"))?;
    let trailing = bytes_done.saturating_sub(last_update_bytes);
    publish_progress(item_id, bytes_done, trailing, session_start.elapsed().as_secs_f64());
    Ok(file_path)
}
