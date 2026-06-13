//! Last-resort stream resolution via a local `yt-dlp` binary.
//!
//! The native InnerTube path ([`super::player::resolve`]) is primary and
//! keeps kopuz binary-free where it can. But YouTube increasingly escalates
//! a signed-in-but-potless session to a "Sign in to confirm you're not a
//! bot" (`LOGIN_REQUIRED`) bot check after a handful of `/player` calls — at
//! which point every InnerTube client fails. yt-dlp carries the full
//! bot-check machinery (and uses our cookies), so when it's installed it's a
//! dependable escape hatch. Flatpak / no-binary users are unaffected: this
//! only runs when `yt-dlp` is actually on PATH and the native path already
//! failed.

use std::io::Write;
use std::path::PathBuf;

use super::player::{AudioFormat, YtStreamInfo};

/// Resolve `video_id` to a playable stream by shelling out to yt-dlp.
/// `cookies` is the same `Cookie:` header the rest of the YT backend uses;
/// it's written to a temp Netscape cookies.txt so yt-dlp authenticates as
/// the signed-in user (and clears the bot check). Returns `Err` if yt-dlp
/// isn't installed or produced nothing usable.
pub async fn resolve(video_id: &str, cookies: Option<&str>) -> Result<YtStreamInfo, String> {
    let url = format!("https://music.youtube.com/watch?v={video_id}");
    resolve_url(&url, cookies).await
}

/// Resolve an arbitrary media URL (YouTube, SoundCloud, …) to a playable
/// audio stream via yt-dlp. SoundCloud and other sites that don't need the
/// YT bot-check just pass `cookies = None`.
pub async fn resolve_url(url: &str, cookies: Option<&str>) -> Result<YtStreamInfo, String> {
    let Some(binary) = find_ytdlp() else {
        return Err("yt-dlp not installed".to_string());
    };
    let url = url.to_string();
    let cookies = cookies.map(|c| c.to_string());

    tokio::task::spawn_blocking(move || resolve_blocking(&binary, &url, cookies.as_deref()))
        .await
        .map_err(|e| format!("yt-dlp task: {e}"))?
}

fn resolve_blocking(
    binary: &str,
    url: &str,
    cookies: Option<&str>,
) -> Result<YtStreamInfo, String> {
    // Hold the temp cookie file alive for the whole yt-dlp run; dropping it
    // deletes the file.
    let _cookie_guard = match cookies {
        Some(c) if !c.is_empty() => Some(write_cookie_file(c)?),
        _ => None,
    };

    let mut cmd = std::process::Command::new(binary);
    cmd.arg("--no-playlist")
        .arg("--no-warnings")
        // NOTE: do NOT add `player_skip=webpage,configs` here. It speeds up
        // resolves but strips the very page yt-dlp uses to mint the PO token /
        // visitor data that clears YouTube's "confirm you're not a bot" check —
        // skipping it makes EVERY resolve fail with LOGIN_REQUIRED. (Regression
        // fixed in v0.7.10.)
        // SoundCloud (and YT under load) intermittently 403 the metadata
        // fetch; retry the extractor + transport a few times, and present a
        // real browser UA so the client-id negotiation isn't blocked.
        .args(["--extractor-retries", "3"])
        .args(["--retries", "5"])
        .args([
            "--user-agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
        ])
        .arg("-f")
        // Force a PROGRESSIVE (range-seekable) stream — exclude HLS m3u8,
        // which the player's range source can't decode. SoundCloud's
        // http_mp3_128 and YT's progressive/dash audio all qualify. Prefer
        // opus/webm (symphonia+libopus fast path) when present.
        .arg(
            "bestaudio[protocol!*=m3u8][acodec=opus]/\
             bestaudio[protocol!*=m3u8]/bestaudio[protocol^=http]/bestaudio/best",
        )
        // One field per --print line, after format selection so these reflect
        // the chosen format.
        .args(["--print", "%(url)s"])
        .args(["--print", "%(ext)s"])
        .args(["--print", "%(http_headers.User-Agent)s"])
        .args(["--print", "%(filesize,filesize_approx)s"])
        .args(["--print", "%(abr)s"])
        .args(["--print", "%(format_id)s"])
        .args(["--print", "%(duration)s"]);
    if let Some(guard) = &_cookie_guard {
        cmd.arg("--cookies").arg(&guard.path);
    }
    cmd.arg(url);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW — don't flash a console on each resolve.
        cmd.creation_flags(0x0800_0000);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("yt-dlp spawn: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "yt-dlp exited {}: {}",
            output.status,
            first_line(&err).or_else(|| first_line(&stderr)).unwrap_or("no output")
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let url = lines.next().unwrap_or("").trim().to_string();
    if url.is_empty() || url == "NA" {
        return Err("yt-dlp returned no stream URL".to_string());
    }
    let ext = lines.next().unwrap_or("").trim();
    let ua = lines.next().unwrap_or("").trim();
    let filesize = parse_na(lines.next());
    let abr = parse_na(lines.next()); // audio bitrate in kbit/s (float)
    let format_id = lines.next().unwrap_or("").trim();
    let duration = parse_na(lines.next()); // seconds (float)

    let format = match ext {
        "webm" | "opus" => AudioFormat::Webm,
        "m4a" | "mp4" => AudioFormat::M4a,
        "mp3" | "mpeg" => AudioFormat::Mp3,
        _ if url.contains("mime=audio%2Fwebm") => AudioFormat::Webm,
        _ if url.contains(".mp3") => AudioFormat::Mp3,
        _ => AudioFormat::M4a,
    };
    let user_agent = if ua.is_empty() || ua == "NA" {
        // ANDROID_VR-ish default; googlevideo URLs are UA-tolerant for GETs.
        "com.google.android.apps.youtube.music/".to_string()
    } else {
        ua.to_string()
    };

    eprintln!(
        "[yt-dlp] resolved {url} (format_id={format_id} ext={ext} abr={abr:?})"
    );

    Ok(YtStreamInfo {
        url,
        format,
        user_agent,
        content_length: filesize.map(|f| f as u64),
        duration_secs: duration.map(|d| d as u64),
        bitrate: abr.map(|kbps| (kbps * 1000.0) as u32),
        itag: format_id.parse::<u32>().ok(),
    })
}

/// Download `video_id`'s best audio END-TO-END with yt-dlp, writing to
/// `<dest_no_ext>.<ext>`. Returns the final file path.
///
/// This exists because resolve-then-download-ourselves has a failure mode the
/// resolve can't see: the googlevideo URL yt-dlp hands back is bound to
/// yt-dlp's client context, and fetching it with a different HTTP stack can
/// 403. Letting yt-dlp do the whole transfer — with its own bot-check
/// handling, retries and fragment recovery — is the most reliable path there
/// is, so the download worker uses it as the primary fallback. Anonymous on
/// purpose: bot-flagged signed-in sessions resolve fine anonymously.
pub async fn download(
    video_id: &str,
    dest_no_ext: &std::path::Path,
    cookies: Option<&str>,
) -> Result<PathBuf, String> {
    let Some(binary) = find_ytdlp() else {
        return Err("yt-dlp not installed".to_string());
    };
    let url = format!("https://music.youtube.com/watch?v={video_id}");
    let dest = dest_no_ext.to_path_buf();
    let cookies = cookies.map(|c| c.to_string());
    tokio::task::spawn_blocking(move || download_blocking(&binary, &url, &dest, cookies.as_deref()))
        .await
        .map_err(|e| format!("yt-dlp task: {e}"))?
}

fn download_blocking(
    binary: &str,
    url: &str,
    dest_no_ext: &std::path::Path,
    cookies: Option<&str>,
) -> Result<PathBuf, String> {
    // Pass the signed-in cookies so a Premium account can fetch the 256 kbps
    // AAC stream (itag 141); free accounts just get itag 140 (128 kbps).
    let _cookie_guard = match cookies {
        Some(c) if !c.is_empty() => Some(write_cookie_file(c)?),
        _ => None,
    };
    let template = format!("{}.%(ext)s", dest_no_ext.display());
    let mut cmd = std::process::Command::new(binary);
    cmd.arg("--no-playlist")
        .arg("--no-warnings")
        .arg("--no-simulate")
        // A leftover partial from an earlier attempt must not be mistaken
        // for a finished download.
        .arg("--force-overwrites")
        .args(["--extractor-retries", "5"])
        .args(["--retries", "10"])
        .args(["--fragment-retries", "10"])
        .args(["--socket-timeout", "30"])
        // 4 fragments saturate the link without the per-IP hammering that 8
        // caused (which bot-flagged the IP and broke playback mid-batch).
        .args(["-N", "4"])
        // Space the extractor requests. The bot check keys on request RATE
        // more than volume — a ~1s gap between the player/metadata calls cuts
        // the LOGIN_REQUIRED rate a lot at almost no throughput cost (the media
        // transfer itself is unaffected).
        .args(["--sleep-requests", "1"])
        .args([
            "--user-agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
        ])
        .arg("-f")
        // Prefer m4a/AAC for DOWNLOADS — unlike opus-in-webm it carries a clean
        // duration header and decodes/seeks reliably in the local player
        // (symphonia has no Opus seeker and webm has no duration, which is why
        // downloaded opus files showed 0:00 and wouldn't play). These are
        // YouTube's source AAC streams — no re-encode, no quality loss:
        //   itag 141 = 256 kbps AAC (Premium, needs cookies)
        //   itag 140 = 128 kbps AAC (everyone)
        // Take the highest-bitrate m4a available, then any non-HLS audio.
        .arg(
            "141/140/bestaudio[ext=m4a][protocol!*=m3u8]/\
             bestaudio[protocol!*=m3u8][acodec*=mp4a]/\
             bestaudio[protocol!*=m3u8]/bestaudio/best",
        )
        .args(["-o", &template]);
    // Embed the cover art so the local library shows artwork. We deliberately
    // do NOT --embed-metadata: yt-dlp would write YouTube's raw video title
    // ("… (Official Video) (4K Remaster)") which is uglier than the clean
    // "Artist - Title" we already named the file, and the library prefers
    // embedded tags over the filename. Needs ffmpeg — only add when present,
    // else the postprocessor would abort the download.
    if ffmpeg_available() {
        cmd.args(["--embed-thumbnail"]);
    }
    cmd
        // Prints the final path (after any move) once the download finishes.
        .args(["--print", "after_move:filepath"])
        .arg(url);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }

    let output = cmd.output().map_err(|e| format!("yt-dlp spawn: {e}"))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "yt-dlp download exited {}: {}",
            output.status,
            first_line(&stderr).or_else(|| first_line(&stdout)).unwrap_or("no output")
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .next_back()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .ok_or("yt-dlp finished but reported no output file")?;
    Ok(path)
}

struct CookieFile {
    path: PathBuf,
}

impl Drop for CookieFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Write the `Cookie:` header out as a Netscape cookies.txt yt-dlp accepts.
fn write_cookie_file(header: &str) -> Result<CookieFile, String> {
    let mut path = std::env::temp_dir();
    path.push(format!("kopuz-yt-cookies-{}.txt", std::process::id()));
    let mut f = std::fs::File::create(&path).map_err(|e| format!("temp cookie file: {e}"))?;
    writeln!(f, "# Netscape HTTP Cookie File").map_err(|e| e.to_string())?;
    for pair in header.split(';') {
        let Some((k, v)) = pair.trim().split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() || v.is_empty() {
            continue;
        }
        // domain  include_subdomains  path  secure  expiry  name  value
        writeln!(f, ".youtube.com\tTRUE\t/\tTRUE\t0\t{k}\t{v}")
            .map_err(|e| e.to_string())?;
    }
    Ok(CookieFile { path })
}

fn parse_na(s: Option<&str>) -> Option<f64> {
    let s = s?.trim();
    if s.is_empty() || s == "NA" {
        return None;
    }
    s.parse::<f64>().ok()
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

/// True if `ffmpeg` is on PATH. yt-dlp needs it to embed metadata/thumbnails;
/// without it those postprocessors would abort the whole download.
fn ffmpeg_available() -> bool {
    let exe = if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| dir.join(exe).is_file())
        })
        .unwrap_or(false)
}

/// Locate the yt-dlp binary. Prefers Kopuz's self-managed copy (kept current
/// automatically — see [`crate::deps`]) so the version is always fresh, then
/// falls back to any system/PATH install. Returns the resolved path, or `None`.
pub fn find_ytdlp() -> Option<String> {
    let managed = crate::deps::managed_ytdlp_path();
    if managed.is_file() {
        return Some(managed.to_string_lossy().into_owned());
    }
    let exe = if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_na_handles_sentinels() {
        assert_eq!(parse_na(Some("128.5")), Some(128.5));
        assert_eq!(parse_na(Some("NA")), None);
        assert_eq!(parse_na(Some("")), None);
        assert_eq!(parse_na(None), None);
    }

    #[test]
    fn cookie_file_is_netscape_formatted() {
        let guard = write_cookie_file("SID=abc; SAPISID=def").unwrap();
        let body = std::fs::read_to_string(&guard.path).unwrap();
        assert!(body.starts_with("# Netscape HTTP Cookie File"));
        assert!(body.contains("\tSID\tabc"));
        assert!(body.contains("\tSAPISID\tdef"));
    }
}
