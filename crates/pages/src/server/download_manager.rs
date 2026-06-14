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

/// Heartbeat: epoch-ms a worker last did anything. Lets queue_downloads_into
/// tell a genuinely-live session from a `is_running=true` left stuck by a dead
/// worker (panic / aborted task) — without it, one bad session bricked the
/// download button until app restart.
#[cfg(not(target_arch = "wasm32"))]
static LAST_WORKER_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(not(target_arch = "wasm32"))]
fn worker_heartbeat() {
    LAST_WORKER_TICK.store(epoch_ms(), Ordering::Relaxed);
}

#[cfg(not(target_arch = "wasm32"))]
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            // Skip only if it's downloaded AND the file is actually still there.
            // A stale offline_tracks entry pointing at a missing file must NOT
            // block a re-download (the "says downloaded but folder empty" bug).
            let on_disk = conf
                .offline_tracks
                .get(id)
                .map(|p| std::path::Path::new(p).exists())
                .unwrap_or(false);
            if on_disk {
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

        // A session is LIVE only if a worker has ticked recently AND something
        // is actually downloading. `is_running` alone lies: a worker that
        // panicked / had its task aborted leaves `is_running=true` and an item
        // stuck `Downloading` forever, which used to brick the button until
        // restart. The heartbeat catches that.
        let workers_alive = {
            let tick = LAST_WORKER_TICK.load(Ordering::Relaxed);
            tick > 0 && epoch_ms().saturating_sub(tick) < 30_000
        };
        let live = q.is_running
            && workers_alive
            && q.items
                .iter()
                .any(|i| matches!(i.status, DownloadStatus::Downloading));
        if live {
            // Running workers claim any Queued item, including ones just added.
            let _ = added;
            return;
        }
        // Not live → any item still marked Downloading belongs to a dead
        // session. Reclaim it so a fresh worker re-processes it.
        for item in q.items.iter_mut() {
            if matches!(item.status, DownloadStatus::Downloading) {
                item.status = DownloadStatus::Queued;
            }
        }
        let has_queued = q
            .items
            .iter()
            .any(|i| matches!(i.status, DownloadStatus::Queued));
        if !has_queued {
            // Nothing to do. If the caller asked for tracks but every one was
            // skipped, they're already downloaded — say so instead of looking
            // like a dead button.
            if !requests.is_empty() && !added {
                components::toast::show_toast("Already downloaded");
            }
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
    worker_heartbeat();

    let session_start = Instant::now();
    // spawn_forever, NOT spawn: a scoped task dies when the page that queued
    // the downloads unmounts — navigating into the playlist (or anywhere)
    // killed the workers mid-run and left the queue frozen at "N queued".
    // The session must belong to the app, not to whichever page started it.
    dioxus::core::spawn_forever(async move {
        // 3 parallel workers, each a separate yt-dlp process pulling the next
        // queued track. 6-way looked fast on a cold IP, but a real several-
        // hundred-track run tripped YouTube's per-IP 429 throttle and lost
        // 37-51 tracks. Directly tested: 3-way with the tv,web_embedded client
        // set + request spacing completes 8/8 even on an already-throttled IP
        // (~5s/track). Completion beats raw speed here — the user wants every
        // song, and one unattended ~8-min run beats re-chasing dozens of fails.
        tokio::join!(
            download_worker(queue, config, session_start, cancel_flag.clone()),
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
        worker_heartbeat();

        // Atomic claim of the next queued track.
        let claimed = {
            let mut q = queue.write();
            match q
                .items
                .iter_mut()
                .find(|i| matches!(i.status, DownloadStatus::Queued))
            {
                Some(item) => {
                    item.status = DownloadStatus::Downloading;
                    Some((
                        item.id.clone(),
                        item.title.clone(),
                        item.artist.clone(),
                        item.subdir.clone(),
                    ))
                }
                None => None,
            }
        };
        let Some((id, title, artist, subdir)) = claimed else {
            return;
        };

        // Already downloaded AND the file still exists → mark done, skip.
        let on_disk = config
            .read()
            .offline_tracks
            .get(&id)
            .map(|p| std::path::Path::new(p).exists())
            .unwrap_or(false);
        if on_disk {
            if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                item.status = DownloadStatus::Done;
            }
            continue;
        }

        let (service, yt_cookies) = {
            let conf = config.read();
            let s = conf.server.as_ref();
            (s.map(|x| x.service), s.and_then(|x| x.access_token.clone()))
        };

        // `Artist - Title.ext` in the browsable folder; playlist downloads
        // group under a per-playlist sub-folder.
        let dest_no_ext = {
            let conf = config.read();
            let mut dir = super::downloads_dir(&conf);
            if let Some(sub) = subdir.as_deref().filter(|s| !s.trim().is_empty()) {
                dir = dir.join(super::sanitize_filename(sub));
                let _ = std::fs::create_dir_all(&dir);
            }
            super::download_dest_no_ext(&dir, &artist, &title, &id)
        };

        let is_yt = matches!(service, Some(MusicService::YtMusic));
        let ytdlp_available = ::server::ytmusic::ytdlp_resolve::find_ytdlp().is_some();

        // Simple flow, three tries. For YouTube the primary path is yt-dlp
        // end-to-end (anonymous, opus via `-x` — it handles its own retries /
        // bot-check and stays current via auto-update, which is the real fix).
        // Without yt-dlp, or for other servers, resolve a URL and stream it
        // directly. The 3rd try + backoff mops up the rare transient YouTube
        // hiccup ("Did not get any data blocks" / a one-off 403) that survives
        // the m3u8-exclusion fix.
        let mut last_err = String::new();
        let mut done = false;
        for attempt in 1..=3u32 {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let outcome: Result<std::path::PathBuf, String> = if is_yt && ytdlp_available {
                remove_stem_files(&dest_no_ext);
                ytdlp_download_with_progress(&id, &dest_no_ext, None, &mut queue, &session_start)
                    .await
            } else if is_yt {
                let yt = ::server::ytmusic::YouTubeMusicClient::with_cookies(
                    yt_cookies.clone().unwrap_or_default(),
                );
                match yt.get_stream(&id).await {
                    Ok(info) => {
                        download_with_progress(
                            &id,
                            &info.url,
                            info.format.extension(),
                            &dest_no_ext,
                            Some(&info.user_agent),
                            info.content_length,
                            &mut queue,
                            &session_start,
                            &cancel_flag,
                        )
                        .await
                    }
                    Err(e) => Err(e),
                }
            } else {
                let url = {
                    let conf = config.read();
                    super::build_download_url(&id, &conf)
                };
                match url {
                    Some((url, ext)) => {
                        download_with_progress(
                            &id, &url, ext, &dest_no_ext, None, None, &mut queue, &session_start,
                            &cancel_flag,
                        )
                        .await
                    }
                    None => Err("Could not build a download URL for this track.".to_string()),
                }
            };

            match outcome {
                Ok(path) => {
                    // Write title/artist/album tags (keeping yt-dlp's embedded
                    // cover) so the LOCAL library groups + shows the file
                    // correctly. Without tags every download landed in one
                    // folder-album sharing a single cover — the "all artists /
                    // tracks show the same image" bug. album = title gives each
                    // download its own album, so its own cover sticks.
                    let _ = reader::write_tags(
                        &path,
                        &reader::models::TrackEdits {
                            title: title.clone(),
                            artist: artist.clone(),
                            album: title.clone(),
                            track_number: None,
                            disc_number: None,
                            cover: reader::models::CoverChange::Keep,
                        },
                    );
                    config
                        .write()
                        .offline_tracks
                        .insert(id.clone(), path.to_string_lossy().into_owned());
                    if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                        item.status = DownloadStatus::Done;
                    }
                    clear_progress(&id);
                    done = true;
                    break;
                }
                Err(e) => {
                    if e == "cancelled" {
                        last_err = e;
                        break;
                    }
                    eprintln!("Download attempt {attempt}/3 failed for {id}: {e}");
                    last_err = e;
                    // Brief backoff before the next try — gives YouTube a moment
                    // to recover from a transient throttle/fragment hiccup
                    // instead of hammering the same failing endpoint instantly.
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            800 * attempt as u64,
                        ))
                        .await;
                    }
                }
            }
        }
        if !done {
            remove_stem_files(&dest_no_ext);
            if last_err != "cancelled"
                && let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id)
            {
                item.status = DownloadStatus::Failed;
                item.error = Some(if last_err.is_empty() {
                    "Download failed".to_string()
                } else {
                    last_err
                });
            }
            clear_progress(&id);
        }
    }
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
    // Stall watchdog: if the partial file hasn't grown in this long, the
    // download is wedged (yt-dlp's own socket/fragment timeouts should catch
    // most stalls, but this is the backstop) — abandon it so the worker can
    // re-resolve and retry instead of freezing for the full 600s.
    const STALL_SECS: u64 = 90;
    let mut last_size = 0u64;
    let mut last_growth = Instant::now();
    loop {
        tokio::select! {
            r = &mut dl => return r,
            _ = &mut deadline => return Err("yt-dlp download timed out after 600s".to_string()),
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                worker_heartbeat();
                let bytes = largest_stem_file_size(dest_no_ext);
                if bytes > last_size {
                    last_size = bytes;
                    last_growth = Instant::now();
                }
                if bytes > 0 {
                    if let Some(item) = queue.write().items.iter_mut().find(|i| i.id == id) {
                        item.bytes_done = bytes;
                    }
                    publish_progress(id, bytes, 0, session_start.elapsed().as_secs_f64());
                }
                if last_growth.elapsed().as_secs() >= STALL_SECS {
                    return Err(format!("yt-dlp download stalled (no progress for {STALL_SECS}s)"));
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
