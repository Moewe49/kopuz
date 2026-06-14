//! Self-managed runtime tools so the user never has to install or update
//! anything by hand. Kopuz keeps its own copies of `yt-dlp` and `ffmpeg` in its
//! data dir:
//!   - `yt-dlp` is refreshed automatically — an outdated yt-dlp is the #1 cause
//!     of YouTube's "sign in to confirm you're not a bot" failures, so keeping
//!     it current is what makes downloads/playback "just work" over time.
//!   - `ffmpeg` is fetched once on first run (it rarely needs updating). It's
//!     what lets downloads extract to .opus and embed cover art.
//!
//! This is why the app needs no separate installer script: the single setup.exe
//! installs Kopuz, and Kopuz bootstraps its own tools on launch. The managed
//! binaries are preferred over any system/PATH install (see
//! `ytmusic::ytdlp_resolve::{find_ytdlp, ffmpeg_available}`).

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

/// Path to the Kopuz-managed ffmpeg (may not exist yet).
pub fn managed_ffmpeg_path() -> PathBuf {
    managed_bin_dir().join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" })
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

/// Where to fetch a static ffmpeg build per platform. Windows/macOS get a zip
/// we can extract; Linux is left to the system package manager (its static
/// builds ship as tar.xz, which isn't worth bundling an xz decoder for — and
/// ffmpeg is one `apt/dnf/pacman install` away there).
///
/// Windows points at BtbN's GitHub release (the rolling `latest` tag, a
/// permanent alias) — served from GitHub's CDN, so it's fast and never
/// rate-limited. (gyan.dev's single host throttles repeat/bulk downloads, which
/// would hang the fetch for real users; the static GitHub build is the reliable
/// choice for something every install runs once.)
fn ffmpeg_zip_url() -> Option<&'static str> {
    if cfg!(windows) {
        // The SHARED build: tiny ffmpeg.exe/ffprobe.exe + shared DLLs. ~90 MB
        // zip vs ~210 MB for the static `-gpl` one, and ~150 MB on disk vs
        // ~390 MB (each static exe is ~195 MB). We extract the whole bin/ —
        // the DLLs must sit next to the exes for them to run.
        Some("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl-shared.zip")
    } else if cfg!(target_os = "macos") {
        Some("https://evermeet.cx/ffmpeg/getrelease/zip")
    } else {
        None
    }
}

/// Ensure a Kopuz-managed ffmpeg exists, downloading a static build on first
/// run if it's missing. Unlike yt-dlp there's no freshness check — ffmpeg only
/// remuxes/extracts audio, so download-once is plenty. Returns its path.
///
/// Safe to call on every startup from a background task: a no-op once present,
/// a single ~40 MB download + extract otherwise. Best-effort — on any failure
/// the caller falls back to a system ffmpeg on PATH, or (if there's none) to
/// the directly-playable m4a download path. Only auto-managed on Windows/macOS.
pub async fn ensure_ffmpeg() -> Result<PathBuf, String> {
    use tokio::io::AsyncWriteExt;

    let ffmpeg_path = managed_ffmpeg_path();
    if ffmpeg_path.is_file() {
        return Ok(ffmpeg_path);
    }
    let Some(url) = ffmpeg_zip_url() else {
        return Err("no managed ffmpeg build for this platform".to_string());
    };
    let dir = managed_bin_dir();
    let zip_tmp = dir.join("ffmpeg-download.zip");

    // Timeouts so a stalled/throttled CDN can never hang the fetch forever
    // (the old no-timeout client wedged indefinitely when gyan.dev throttled).
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("client: {e}"))?;
    let mut resp = client
        .get(url)
        .header("User-Agent", "kopuz")
        .send()
        .await
        .map_err(|e| format!("ffmpeg download HTTP: {e}"))?
        .error_for_status()
        .map_err(|e| format!("ffmpeg download HTTP: {e}"))?;

    // Stream the (large, ~90 MB) archive straight to disk instead of buffering
    // it all in RAM. `chunk()` needs no extra reqwest feature.
    let mut file = tokio::fs::File::create(&zip_tmp)
        .await
        .map_err(|e| format!("ffmpeg temp create: {e}"))?;
    let mut total: u64 = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("ffmpeg download body: {e}"))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("ffmpeg temp write: {e}"))?;
        total += chunk.len() as u64;
    }
    file.flush()
        .await
        .map_err(|e| format!("ffmpeg temp flush: {e}"))?;
    drop(file);
    if total < 1_000_000 {
        let _ = tokio::fs::remove_file(&zip_tmp).await;
        return Err(format!("ffmpeg download too small ({total} bytes)"));
    }

    // Zip parse + extract is blocking CPU/IO work — keep it off the async runtime.
    let dir2 = dir.clone();
    let zip_for_extract = zip_tmp.clone();
    let result = tokio::task::spawn_blocking(move || {
        extract_ffmpeg_from_zip(&zip_for_extract, &dir2)
    })
    .await
    .map_err(|e| format!("ffmpeg extract task: {e}"))?;
    let _ = tokio::fs::remove_file(&zip_tmp).await;
    result
}

/// Pull the ffmpeg/ffprobe binaries (and, for the shared Windows build, the
/// DLLs they link against) out of a downloaded release zip on disk and write
/// them flat into `dir`. Returns the ffmpeg path. Each file is written to a
/// temp name then renamed, so a half-extracted binary is never mistaken for a
/// working one.
fn extract_ffmpeg_from_zip(zip_path: &std::path::Path, dir: &std::path::Path) -> Result<PathBuf, String> {
    let f = std::fs::File::open(zip_path).map_err(|e| format!("open ffmpeg zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| format!("read ffmpeg zip: {e}"))?;

    let mut ffmpeg_out: Option<PathBuf> = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("ffmpeg zip entry: {e}"))?;
        if !entry.is_file() {
            continue;
        }
        // `enclosed_name` rejects path-traversal entries.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let Some(base) = path.file_name().and_then(|f| f.to_str()).map(str::to_owned) else {
            continue;
        };
        if !is_wanted_ffmpeg_file(&path, &base) {
            continue;
        }

        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf)
            .map_err(|e| format!("read {base}: {e}"))?;

        let dest = dir.join(&base);
        let tmp = dest.with_extension("download");
        std::fs::write(&tmp, &buf).map_err(|e| format!("write {base}: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        }
        let _ = std::fs::remove_file(&dest);
        std::fs::rename(&tmp, &dest).map_err(|e| format!("install {base}: {e}"))?;

        if base == "ffmpeg" || base == "ffmpeg.exe" {
            ffmpeg_out = Some(dest);
        }
    }
    ffmpeg_out.ok_or_else(|| "ffmpeg not found in downloaded archive".to_string())
}

/// Which archive entries we keep: the ffmpeg/ffprobe executables, plus — for
/// the Windows shared build — the DLLs that sit next to them under `bin/` (the
/// exes won't run without their shared libs in the same folder). The macOS zip
/// is a single static `ffmpeg` at the root, so there it's just the basename.
fn is_wanted_ffmpeg_file(path: &std::path::Path, base: &str) -> bool {
    if cfg!(windows) {
        let parent_is_bin = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|f| f == "bin")
            .unwrap_or(false);
        parent_is_bin
            && (base == "ffmpeg.exe"
                || base == "ffprobe.exe"
                || base.to_ascii_lowercase().ends_with(".dll"))
    } else {
        base == "ffmpeg" || base == "ffprobe"
    }
}
