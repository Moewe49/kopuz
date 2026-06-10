//! SoundCloud search + stream resolution via a local `yt-dlp`.
//!
//! SoundCloud has no free public search API anymore (client-id gating), so we
//! lean on yt-dlp — already a kopuz dependency for downloads, and the most
//! reliable SoundCloud client there is. `scsearch` returns tracks; each
//! resolves to a playable stream through the shared yt-dlp resolver.
//!
//! Tracks carry a synthetic path `soundcloud:<hexUrl>[:urlhex_<hexThumb>]`,
//! mirroring the `ytmusic:` scheme so the rest of the app (queue, playlists,
//! cover decoding) treats them uniformly. The hex-encoded permalink URL is
//! what the player feeds back to yt-dlp at play time.

use std::path::PathBuf;

use reader::models::Track;

use crate::ytmusic::player::YtStreamInfo;
use crate::ytmusic::ytdlp_resolve;

pub const SOURCE_PREFIX: &str = "soundcloud";

/// True once a usable yt-dlp binary is on PATH — the UI gates the SoundCloud
/// toggle on this so it doesn't offer search that can't resolve.
pub fn is_available() -> bool {
    ytdlp_resolve::find_ytdlp().is_some()
}

/// Search SoundCloud for up to `limit` tracks. Uses yt-dlp's `scsearch`
/// extractor with `--flat-playlist` so it returns quickly (metadata only, no
/// per-track resolve). Returns `Err` if yt-dlp isn't installed.
pub async fn search(query: &str, limit: usize) -> Result<Vec<Track>, String> {
    let Some(binary) = ytdlp_resolve::find_ytdlp() else {
        return Err("yt-dlp not installed".to_string());
    };
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let query = query.to_string();
    let limit = limit.clamp(1, 50);

    tokio::task::spawn_blocking(move || search_blocking(&binary, &query, limit))
        .await
        .map_err(|e| format!("yt-dlp search task: {e}"))?
}

fn search_blocking(binary: &str, query: &str, limit: usize) -> Result<Vec<Track>, String> {
    let target = format!("scsearch{limit}:{query}");
    let mut cmd = std::process::Command::new(binary);
    cmd.arg("--flat-playlist")
        .arg("--no-warnings")
        // One tab-joined record per track; --flat-playlist keeps it cheap.
        .args([
            "--print",
            "%(url)s\t%(title)s\t%(uploader)s\t%(duration)s\t%(thumbnail)s",
        ])
        .arg(&target);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let output = cmd.output().map_err(|e| format!("yt-dlp spawn: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "yt-dlp scsearch exited {}: {}",
            output.status,
            err.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output")
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tracks = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        let url = f.next().unwrap_or("").trim();
        if url.is_empty() || url == "NA" {
            continue;
        }
        let title = na(f.next()).unwrap_or_else(|| "Unknown".to_string());
        let uploader = na(f.next()).unwrap_or_default();
        let duration = na(f.next())
            .and_then(|s| s.parse::<f64>().ok())
            .map(|s| s as u64)
            .unwrap_or(0);
        let thumbnail = na(f.next());
        tracks.push(build_track(url, &title, &uploader, duration, thumbnail.as_deref()));
    }
    Ok(tracks)
}

/// Resolve a SoundCloud track (its synthetic path) to a playable stream.
/// `track_path` is the `soundcloud:<hexUrl>…` path the search produced.
pub async fn resolve_path(track_path: &str) -> Result<YtStreamInfo, String> {
    let url = decode_url_from_path(track_path)
        .ok_or_else(|| "not a soundcloud path".to_string())?;
    ytdlp_resolve::resolve_url(&url, None).await
}

/// Permalink URL behind a SoundCloud track path, for the yt-dlp downloader.
pub fn permalink_from_path(track_path: &str) -> Option<String> {
    decode_url_from_path(track_path)
}

fn build_track(
    url: &str,
    title: &str,
    uploader: &str,
    duration: u64,
    thumbnail: Option<&str>,
) -> Track {
    let hex_url = hex::encode(url.as_bytes());
    let path = match thumbnail.filter(|t| !t.is_empty() && *t != "NA") {
        Some(thumb) => PathBuf::from(format!(
            "{SOURCE_PREFIX}:{hex_url}:urlhex_{}",
            hex::encode(thumb.as_bytes())
        )),
        None => PathBuf::from(format!("{SOURCE_PREFIX}:{hex_url}")),
    };
    Track {
        path,
        album_id: format!("{SOURCE_PREFIX}:set:{}", hex::encode(uploader.as_bytes())),
        title: title.to_string(),
        artist: uploader.to_string(),
        album: String::new(),
        duration,
        khz: 0,
        bitrate: 0,
        track_number: None,
        disc_number: None,
        musicbrainz_release_id: None,
        musicbrainz_recording_id: None,
        musicbrainz_track_id: None,
        playlist_item_id: None,
        artists: if uploader.is_empty() {
            Vec::new()
        } else {
            vec![uploader.to_string()]
        },
    }
}

/// Decode the permalink URL from a `soundcloud:<hexUrl>[:…]` path.
fn decode_url_from_path(track_path: &str) -> Option<String> {
    let rest = track_path.strip_prefix(SOURCE_PREFIX)?.strip_prefix(':')?;
    let hex_url = rest.split(':').next()?;
    let bytes = hex::decode(hex_url).ok()?;
    String::from_utf8(bytes).ok()
}

fn na(s: Option<&str>) -> Option<String> {
    let s = s?.trim();
    (!s.is_empty() && s != "NA").then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_url_through_path() {
        let t = build_track(
            "https://soundcloud.com/artist/track",
            "My Track",
            "Artist",
            180,
            Some("https://i1.sndcdn.com/x.jpg"),
        );
        let path = t.path.to_string_lossy();
        assert!(path.starts_with("soundcloud:"));
        assert_eq!(
            decode_url_from_path(&path).as_deref(),
            Some("https://soundcloud.com/artist/track")
        );
        assert_eq!(t.title, "My Track");
        assert_eq!(t.artist, "Artist");
        assert_eq!(t.duration, 180);
    }

    #[test]
    fn handles_missing_thumbnail() {
        let t = build_track("https://soundcloud.com/a/b", "T", "U", 0, None);
        let path = t.path.to_string_lossy();
        assert_eq!(
            decode_url_from_path(&path).as_deref(),
            Some("https://soundcloud.com/a/b")
        );
    }

    #[test]
    fn rejects_non_soundcloud_path() {
        assert_eq!(decode_url_from_path("ytmusic:abc:def"), None);
    }
}
