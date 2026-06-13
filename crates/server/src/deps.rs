//! Self-managed runtime tools so the user never has to install or update
//! anything by hand. Kopuz keeps its own copy of `yt-dlp` in its data dir and
//! refreshes it automatically — an outdated yt-dlp is the #1 cause of
//! YouTube's "sign in to confirm you're not a bot" failures, so keeping it
//! current is what makes downloads/playback "just work" over time.
//!
//! The managed binary is preferred over any system/PATH install (see
//! `ytmusic::ytdlp_resolve::find_ytdlp`), so we fully control the version.

use std::path::PathBuf;
use std::time::Duration;

/// How old the managed yt-dlp may get before we refresh it. yt-dlp ships
/// bot-check fixes constantly; a week keeps us current without re-downloading
/// every launch.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// `<data>/bin` — where Kopuz keeps the tools it manages itself.
pub fn managed_bin_dir() -> PathBuf {
    let base = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("bin");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Path to the Kopuz-managed yt-dlp (may not exist yet).
pub fn managed_ytdlp_path() -> PathBuf {
    managed_bin_dir().join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" })
}

fn ytdlp_release_url() -> &'static str {
    if cfg!(windows) {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos"
    } else {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp"
    }
}

fn is_fresh(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|age| age < MAX_AGE)
        .unwrap_or(false)
}

/// Ensure a fresh Kopuz-managed yt-dlp exists, downloading the latest release
/// if it's missing or older than [`MAX_AGE`]. Returns its path. Safe to call
/// on every startup from a background task — it's a no-op when already fresh,
/// and a single ~17 MB download otherwise. Best-effort: on failure the caller
/// falls back to any system yt-dlp.
pub async fn ensure_ytdlp_fresh() -> Result<PathBuf, String> {
    let path = managed_ytdlp_path();
    if path.is_file() && is_fresh(&path) {
        return Ok(path);
    }

    let url = ytdlp_release_url();
    let resp = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("client: {e}"))?
        .get(url)
        .header("User-Agent", "kopuz")
        .send()
        .await
        .map_err(|e| format!("yt-dlp download HTTP: {e}"))?
        .error_for_status()
        .map_err(|e| format!("yt-dlp download HTTP: {e}"))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("yt-dlp download body: {e}"))?;
    if bytes.len() < 1_000_000 {
        return Err(format!("yt-dlp download too small ({} bytes)", bytes.len()));
    }

    // Write to a temp path then rename, so a half-written file can never be
    // mistaken for a working binary (and the rename replaces the old one).
    let tmp = path.with_extension("download");
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|e| format!("yt-dlp write: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    // On Windows the destination can't be replaced while it's executing; a
    // best-effort remove of the old copy first, then rename.
    let _ = tokio::fs::remove_file(&path).await;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| format!("yt-dlp install: {e}"))?;
    Ok(path)
}
