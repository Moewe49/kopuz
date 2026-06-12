pub mod activity;
pub mod album;
pub mod artist;
pub mod discover;
pub mod download_manager;
pub mod favorites;
pub mod home;
pub mod library;
pub mod playlists;
pub mod search;
pub mod subsonic_sync;
pub mod unsupported;

use config::{AppConfig, MusicService};
use dioxus::prelude::{ReadableExt, WritableExt};
use std::path::PathBuf;

pub(super) fn offline_cache_dir() -> PathBuf {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let base = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
            .map(|dirs| dirs.cache_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("./cache"));
        let dir = base.join("offline_tracks");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
    #[cfg(target_arch = "wasm32")]
    PathBuf::from("./cache/offline_tracks")
}

/// The user-facing, browsable downloads folder. Uses
/// `config.download_directory` when set, otherwise defaults to
/// `<Music>/Kopuz` (falling back to `<home>/Music/Kopuz`, then `./downloads`).
/// Unlike [`offline_cache_dir`] this holds readable `Artist - Title.ext`
/// files the user can open in a file manager or import into another player.
pub fn downloads_dir(config: &AppConfig) -> PathBuf {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = config.resolved_download_dir();
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = config;
        PathBuf::from("./downloads")
    }
}

/// Strip characters that are illegal in filenames on Windows/macOS/Linux and
/// collapse whitespace, so a track title can become a safe filename.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn sanitize_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches([' ', '.']).to_string();
    if trimmed.is_empty() {
        "track".to_string()
    } else {
        // Keep filenames well under the 255-byte limit even with long titles.
        trimmed.chars().take(120).collect()
    }
}

/// Build the destination path (without extension) for a download under `dir`,
/// named `Artist - Title`. Appends ` (n)` to dodge collisions with existing
/// files of any extension. Falls back to the item id when there's no metadata.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn download_dest_no_ext(
    dir: &std::path::Path,
    artist: &str,
    title: &str,
    item_id: &str,
) -> PathBuf {
    let artist = sanitize_filename(artist);
    let title = sanitize_filename(title);
    let base = match (artist.as_str(), title.as_str()) {
        ("track", "track") => sanitize_filename(item_id),
        (_, t) if t != "track" && artist != "track" => format!("{artist} - {title}"),
        (_, t) if t != "track" => title.clone(),
        _ => sanitize_filename(item_id),
    };
    let mut candidate = dir.join(&base);
    let mut n = 1;
    // Collide on the stem regardless of extension (we don't yet know the
    // final ext here): check for any file starting with the same stem.
    while stem_taken(dir, candidate.file_name().and_then(|f| f.to_str()).unwrap_or(&base)) {
        n += 1;
        candidate = dir.join(format!("{base} ({n})"));
    }
    candidate
}

#[cfg(not(target_arch = "wasm32"))]
fn stem_taken(dir: &std::path::Path, stem: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            let entry_stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
            if entry_stem.eq_ignore_ascii_case(stem) {
                return true;
            }
        }
    }
    false
}

pub fn build_download_url(item_id: &str, config: &AppConfig) -> Option<(String, &'static str)> {
    let server = config.server.as_ref()?;
    let quality = config.offline_quality;
    let ext = quality.file_extension();

    let url = match server.service {
        MusicService::Jellyfin => {
            let token = server.access_token.as_deref().unwrap_or("");
            match quality.jellyfin_bitrate_bps() {
                Some(bps) => format!(
                    "{}/Audio/{}/stream?audioBitRate={}&audioCodec=mp3&api_key={}",
                    server.url, item_id, bps, token
                ),
                None => format!(
                    "{}/Audio/{}/stream?static=true&api_key={}",
                    server.url, item_id, token
                ),
            }
        }
        MusicService::Subsonic | MusicService::Custom => {
            let username = server.user_id.as_deref()?;
            let password_or_token = server.access_token.as_deref()?;
            let resolved_password = ::server::provider::resolve_subsonic_secret(password_or_token)?;
            let client =
                ::server::subsonic::SubsonicClient::new(&server.url, username, &resolved_password);
            let kbps = quality.subsonic_max_bitrate_kbps();
            client.stream_url_with_bitrate(item_id, Some(kbps)).ok()?
        }
        MusicService::YtMusic => return None,
    };
    Some((url, ext))
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn content_type_to_ext(content_type: &str) -> Option<&'static str> {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    match ct {
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/mp4" | "audio/x-m4a" | "video/mp4" => Some("m4a"),
        "audio/ogg" | "audio/opus" => Some("ogg"),
        "audio/webm" | "video/webm" => Some("webm"),
        "audio/aac" => Some("aac"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/aiff" | "audio/x-aiff" => Some("aiff"),
        _ => None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn download_track_to_cache(
    item_id: &str,
    url: &str,
    ext_hint: &str,
) -> Result<PathBuf, String> {
    let response = reqwest::get(url)
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    let ext = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(content_type_to_ext)
        .unwrap_or(ext_hint);

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let dir = offline_cache_dir();
    let file_path = dir.join(format!("{item_id}.{ext}"));
    tokio::fs::write(&file_path, &bytes)
        .await
        .map_err(|e| format!("Failed to save file: {e}"))?;

    Ok(file_path)
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn download_tracks_batch(
    item_ids: Vec<String>,
    mut config: dioxus::prelude::Signal<AppConfig>,
) {
    for id in item_ids {
        let is_downloaded = if let Some(path_str) = config.read().offline_tracks.get(&id) {
            std::path::Path::new(path_str).exists()
        } else {
            false
        };
        if is_downloaded {
            continue;
        }
        let result = {
            let conf = config.read();
            build_download_url(&id, &conf)
        };
        if let Some((url, ext)) = result {
            match download_track_to_cache(&id, &url, ext).await {
                Ok(path) => {
                    config
                        .write()
                        .offline_tracks
                        .insert(id.clone(), path.to_string_lossy().into_owned());
                }
                Err(e) => eprintln!("Batch download failed for {id}: {e}"),
            }
        }
    }
}
