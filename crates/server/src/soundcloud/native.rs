//! Native SoundCloud resolver — no yt-dlp, pure HTTP via reqwest.
//!
//! yt-dlp can't run on Android (wrong ABI, no exec on app dirs), so SoundCloud
//! playback there needs a native path. This mirrors what yt-dlp does under the
//! hood: scrape a public `client_id` from soundcloud.com's JS bundles, hit the
//! public `api-v2.soundcloud.com` endpoints, and pick a **progressive mp3**
//! stream (a plain range-serving CDN URL that both the desktop cpal decoder and
//! Android ExoPlayer can play directly). HLS is only surfaced when the caller
//! opts in (`allow_hls`) — the desktop cpal path can't demux m3u8, so it falls
//! back to yt-dlp for the rare HLS-only track; ExoPlayer plays HLS natively.

use std::sync::{Mutex, OnceLock};

use reader::models::Track;
use serde_json::Value;

use crate::ytmusic::player::{AudioFormat, YtStreamInfo};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/125.0 Safari/537.36";
const API: &str = "https://api-v2.soundcloud.com";
const HOME: &str = "https://soundcloud.com/";

/// Cached public client_id. Cleared + re-scraped on a 401 (SoundCloud rotates
/// it periodically).
static CLIENT_ID: Mutex<Option<String>> = Mutex::new(None);

fn http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default()
        })
        .clone()
}

fn cached_client_id() -> Option<String> {
    CLIENT_ID.lock().ok().and_then(|g| g.clone())
}

fn store_client_id(id: &str) {
    if let Ok(mut g) = CLIENT_ID.lock() {
        *g = Some(id.to_string());
    }
}

fn invalidate_client_id() {
    if let Ok(mut g) = CLIENT_ID.lock() {
        *g = None;
    }
}

/// Public client_id, scraping soundcloud.com's JS bundles on first use.
async fn client_id() -> Result<String, String> {
    if let Some(id) = cached_client_id() {
        return Ok(id);
    }
    let id = scrape_client_id().await?;
    store_client_id(&id);
    Ok(id)
}

async fn scrape_client_id() -> Result<String, String> {
    let html = http()
        .get(HOME)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("soundcloud home: {e}"))?
        .text()
        .await
        .map_err(|e| format!("soundcloud home body: {e}"))?;

    // The client_id lives in one of the numbered asset bundles — usually a
    // later one, so scan from the end.
    let mut bundles = asset_bundles(&html);
    bundles.reverse();
    for js in bundles {
        let Ok(resp) = http()
            .get(&js)
            .header(reqwest::header::USER_AGENT, UA)
            .send()
            .await
        else {
            continue;
        };
        let Ok(body) = resp.text().await else {
            continue;
        };
        if let Some(id) = extract_client_id(&body) {
            return Ok(id);
        }
    }
    Err("could not find a SoundCloud client_id".to_string())
}

/// All `https://a-v2.sndcdn.com/assets/*.js` URLs referenced by the home page,
/// in document order, de-duplicated.
fn asset_bundles(html: &str) -> Vec<String> {
    const NEEDLE: &str = "https://a-v2.sndcdn.com/assets/";
    let mut out: Vec<String> = Vec::new();
    let mut from = 0;
    while let Some(rel) = html[from..].find(NEEDLE) {
        let start = from + rel;
        let Some(end_rel) = html[start..].find(".js") else {
            break;
        };
        let end = start + end_rel + 3; // include ".js"
        let url = &html[start..end];
        if !url.contains(['"', '\'', ' ', '\\', '<', '>']) && !out.iter().any(|u| u == url) {
            out.push(url.to_string());
        }
        from = end;
    }
    out
}

/// Pull `client_id:"..."` (or `client_id="..."`) out of a JS bundle.
fn extract_client_id(js: &str) -> Option<String> {
    for pat in ["client_id:\"", "client_id=\"", "clientId:\""] {
        if let Some(i) = js.find(pat) {
            let id: String = js[i + pat.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if id.len() >= 20 {
                return Some(id);
            }
        }
    }
    None
}

/// Resolve a SoundCloud permalink URL to a playable stream. `allow_hls` lets
/// the caller (Android/ExoPlayer) accept an HLS m3u8 when no progressive mp3
/// exists; desktop callers pass `false` so they can fall back to yt-dlp.
pub async fn native_resolve(permalink_url: &str, allow_hls: bool) -> Result<YtStreamInfo, String> {
    // Two attempts: a stale cached client_id yields 401, so drop it and re-scrape once.
    for attempt in 0..2 {
        let cid = client_id().await?;
        let resp = http()
            .get(format!("{API}/resolve"))
            .query(&[("url", permalink_url), ("client_id", cid.as_str())])
            .header(reqwest::header::USER_AGENT, UA)
            .send()
            .await
            .map_err(|e| format!("sc resolve: {e}"))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            invalidate_client_id();
            continue;
        }
        let track: Value = resp
            .error_for_status()
            .map_err(|e| format!("sc resolve http: {e}"))?
            .json()
            .await
            .map_err(|e| format!("sc resolve json: {e}"))?;
        return transcode_to_stream(&track, &cid, allow_hls).await;
    }
    Err("sc resolve failed after client_id refresh".to_string())
}

/// Turn a resolved track JSON into a stream: prefer progressive mp3, else (only
/// when `allow_hls`) an HLS playlist.
async fn transcode_to_stream(
    track: &Value,
    client_id: &str,
    allow_hls: bool,
) -> Result<YtStreamInfo, String> {
    let transcodings = track
        .pointer("/media/transcodings")
        .and_then(|v| v.as_array())
        .ok_or("sc: no transcodings (track may be private/geoblocked)")?;

    let is_progressive =
        |t: &Value| t.pointer("/format/protocol").and_then(|v| v.as_str()) == Some("progressive");
    let is_mp3 = |t: &Value| {
        t.pointer("/format/mime_type")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.contains("mpeg"))
    };

    let pick = transcodings
        .iter()
        .find(|t| is_progressive(t) && is_mp3(t))
        .or_else(|| transcodings.iter().find(|t| is_progressive(t)))
        .or_else(|| {
            allow_hls
                .then(|| {
                    transcodings.iter().find(|t| {
                        t.pointer("/format/protocol").and_then(|v| v.as_str()) == Some("hls")
                    })
                })
                .flatten()
        })
        .ok_or("sc: no playable transcoding (HLS-only)")?;

    let hls = pick.pointer("/format/protocol").and_then(|v| v.as_str()) == Some("hls");
    let transcoding_url = pick
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("sc: transcoding has no url")?;

    // The transcoding endpoint returns { url: <actual stream/playlist url> }.
    let media: Value = http()
        .get(transcoding_url)
        .query(&[("client_id", client_id)])
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("sc transcoding: {e}"))?
        .error_for_status()
        .map_err(|e| format!("sc transcoding http: {e}"))?
        .json()
        .await
        .map_err(|e| format!("sc transcoding json: {e}"))?;

    let stream_url = media
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("sc: no stream url")?
        .to_string();

    let duration_secs = track
        .get("full_duration")
        .or_else(|| track.get("duration"))
        .and_then(|v| v.as_u64())
        .map(|ms| ms / 1000);

    // A progressive CDN URL serves Range requests; grab the total length so the
    // download path can range-chunk and the player can seek. HLS has no single
    // length.
    let content_length = if hls {
        None
    } else {
        probe_content_length(&stream_url).await
    };

    Ok(YtStreamInfo {
        url: stream_url,
        format: if hls {
            AudioFormat::M4a
        } else {
            AudioFormat::Mp3
        },
        user_agent: UA.to_string(),
        content_length,
        duration_secs,
        bitrate: None,
        itag: None,
        deep_range_safe: true,
    })
}

/// Total byte length of a range-serving URL via a 1-byte probe (`Content-Range:
/// bytes 0-0/TOTAL`). `None` if the server doesn't report it.
async fn probe_content_length(url: &str) -> Option<u64> {
    let resp = http()
        .get(url)
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await
        .ok()?;
    let cr = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    cr.rsplit('/').next()?.trim().parse::<u64>().ok()
}

/// Search SoundCloud natively (api-v2 `/search/tracks`).
pub async fn native_search(query: &str, limit: usize) -> Result<Vec<Track>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, 50).to_string();
    let mut last_status = None;
    for attempt in 0..2 {
        let cid = client_id().await?;
        let resp = http()
            .get(format!("{API}/search/tracks"))
            .query(&[
                ("q", query),
                ("limit", limit.as_str()),
                ("client_id", cid.as_str()),
            ])
            .header(reqwest::header::USER_AGENT, UA)
            .send()
            .await
            .map_err(|e| format!("sc search: {e}"))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            invalidate_client_id();
            continue;
        }
        last_status = Some(resp.status());
        let val: Value = resp
            .error_for_status()
            .map_err(|e| format!("sc search http: {e}"))?
            .json()
            .await
            .map_err(|e| format!("sc search json: {e}"))?;
        return Ok(tracks_from_collection(&val));
    }
    Err(format!("sc search failed (status {last_status:?})"))
}

/// The tracks SoundCloud considers related to the one at `permalink_url` — its
/// own "station"/autoplay seed (`/tracks/{id}/related`).
///
/// This is what makes end-of-queue autoradio work for a SoundCloud queue: the
/// ListenBrainz artist graph only knows artists MusicBrainz has heard of, and
/// most SoundCloud uploaders it has not. SoundCloud's own co-listening graph
/// does, so it is the reliable seed here.
///
/// The synthetic path stores only the permalink, not the numeric id, so this
/// resolves the permalink first (one call) and then reads the related list
/// (a second). Two round-trips per seed is why callers seed from only a few.
pub async fn native_related(permalink_url: &str, limit: usize) -> Result<Vec<Track>, String> {
    let limit = limit.clamp(1, 50).to_string();
    for attempt in 0..2 {
        let cid = client_id().await?;
        // Resolve the permalink to the numeric track id the related endpoint needs.
        let resolved = http()
            .get(format!("{API}/resolve"))
            .query(&[("url", permalink_url), ("client_id", cid.as_str())])
            .header(reqwest::header::USER_AGENT, UA)
            .send()
            .await
            .map_err(|e| format!("sc related resolve: {e}"))?;
        if resolved.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            invalidate_client_id();
            continue;
        }
        let track: Value = resolved
            .error_for_status()
            .map_err(|e| format!("sc related resolve http: {e}"))?
            .json()
            .await
            .map_err(|e| format!("sc related resolve json: {e}"))?;
        let Some(id) = track.get("id").and_then(|v| v.as_u64()) else {
            return Err("sc related: resolved track has no id".to_string());
        };
        let resp = http()
            .get(format!("{API}/tracks/{id}/related"))
            .query(&[("client_id", cid.as_str()), ("limit", limit.as_str())])
            .header(reqwest::header::USER_AGENT, UA)
            .send()
            .await
            .map_err(|e| format!("sc related: {e}"))?;
        let val: Value = resp
            .error_for_status()
            .map_err(|e| format!("sc related http: {e}"))?
            .json()
            .await
            .map_err(|e| format!("sc related json: {e}"))?;
        return Ok(tracks_from_collection(&val));
    }
    Err("sc related failed after client_id refresh".to_string())
}

fn tracks_from_collection(val: &Value) -> Vec<Track> {
    let Some(cols) = val.get("collection").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for t in cols {
        // Skip non-track items (playlists) that can appear in mixed endpoints.
        if t.get("kind").and_then(|v| v.as_str()) == Some("playlist") {
            continue;
        }
        let Some(permalink) = t.get("permalink_url").and_then(|v| v.as_str()) else {
            continue;
        };
        if permalink.is_empty() {
            continue;
        }
        let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown");
        let user = t
            .pointer("/user/username")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let duration = t
            .get("duration")
            .and_then(|v| v.as_u64())
            .map(|ms| ms / 1000)
            .unwrap_or(0);
        let artwork = t
            .get("artwork_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(upgrade_artwork);
        out.push(super::build_track(
            permalink,
            title,
            user,
            duration,
            artwork.as_deref(),
        ));
    }
    out
}

/// SoundCloud artwork URLs come back at `-large` (100px). Bump to `-t500x500`
/// so covers aren't blurry in the now-playing bar / fullscreen.
fn upgrade_artwork(url: &str) -> String {
    url.replace("-large.", "-t500x500.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_client_id_colon_form() {
        let js = r#"...,api:{client_id:"abc123DEF456ghi789JKL0"},x:1"#;
        assert_eq!(
            extract_client_id(js).as_deref(),
            Some("abc123DEF456ghi789JKL0")
        );
    }

    #[test]
    fn rejects_short_client_id() {
        assert_eq!(extract_client_id(r#"client_id:"short""#), None);
    }

    #[test]
    fn finds_asset_bundles_in_order_deduped() {
        let html = r#"<script src="https://a-v2.sndcdn.com/assets/2-abc.js"></script>
            <script crossorigin src="https://a-v2.sndcdn.com/assets/55-def.js"></script>
            <script src="https://a-v2.sndcdn.com/assets/2-abc.js"></script>"#;
        let b = asset_bundles(html);
        assert_eq!(
            b,
            vec![
                "https://a-v2.sndcdn.com/assets/2-abc.js".to_string(),
                "https://a-v2.sndcdn.com/assets/55-def.js".to_string(),
            ]
        );
    }

    #[test]
    fn upgrades_artwork_resolution() {
        assert_eq!(
            upgrade_artwork("https://i1.sndcdn.com/artworks-xyz-large.jpg"),
            "https://i1.sndcdn.com/artworks-xyz-t500x500.jpg"
        );
    }
}
