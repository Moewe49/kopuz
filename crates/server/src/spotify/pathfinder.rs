//! Spotify's internal web-player "pathfinder" GraphQL API.
//!
//! The public Web API can't read other users' playlists in full from a personal
//! (development-mode) app — it returns 403 — and the anonymous web token gets
//! 429-rate-limited on the REST API. Pathfinder, the GraphQL endpoint the actual
//! web player talks to, reads ANY public playlist in full using only the
//! anonymous token the embed page already ships — no login, no owning/following
//! the playlist. This is the same path third-party "Spotify downloaders" use.

use serde_json::Value;

use super::{SpotifyPlaylist, SpotifyTrack};

const PATHFINDER: &str = "https://api-partner.spotify.com/pathfinder/v1/query";
/// Persisted-query hash for the web player's `fetchPlaylist` operation. Spotify
/// rotates these occasionally; if it stops matching, the response has no
/// `playlistV2` and the caller falls back to the embed page's inlined ~100.
const FETCH_PLAYLIST_HASH: &str =
    "73a3b3470804983e4d55d83cd6cc99715019228fd999d51429cc69473a18789d";
const PAGE: u32 = 100;
const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";

/// Page the full track list of a public playlist. `token` is the anonymous web
/// token from the embed page (`embed::extract_access_token`).
pub async fn fetch_playlist(token: &str, id: &str) -> Result<SpotifyPlaylist, String> {
    let client = super::api::http_client();
    let mut name = String::from("Spotify import");
    let mut tracks: Vec<SpotifyTrack> = Vec::new();
    let mut offset = 0u32;

    loop {
        let variables =
            format!(r#"{{"uri":"spotify:playlist:{id}","offset":{offset},"limit":{PAGE}}}"#);
        let extensions = format!(
            r#"{{"persistedQuery":{{"version":1,"sha256Hash":"{FETCH_PLAYLIST_HASH}"}}}}"#
        );
        let resp = client
            .get(PATHFINDER)
            .query(&[
                ("operationName", "fetchPlaylist"),
                ("variables", variables.as_str()),
                ("extensions", extensions.as_str()),
            ])
            .bearer_auth(token)
            .header("User-Agent", BROWSER_UA)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("pathfinder HTTP: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("pathfinder HTTP {}", resp.status()));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("pathfinder JSON: {e}"))?;
        let pl = v
            .pointer("/data/playlistV2")
            .ok_or("pathfinder: no playlistV2 (query hash may be stale)")?;
        if offset == 0
            && let Some(n) = pl.pointer("/name").and_then(|x| x.as_str())
        {
            name = n.to_string();
        }
        let total = pl
            .pointer("/content/totalCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32;
        let items = pl
            .pointer("/content/items")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for item in &items {
            let Some(data) = item.pointer("/itemV2/data") else {
                continue;
            };
            if data.pointer("/__typename").and_then(|x| x.as_str()) != Some("Track") {
                continue;
            }
            let title = data
                .pointer("/name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if title.is_empty() {
                continue;
            }
            let artists: Vec<String> = data
                .pointer("/artists/items")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|ar| {
                            ar.pointer("/profile/name")
                                .and_then(|n| n.as_str())
                                .map(String::from)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let duration_secs = data
                .pointer("/trackDuration/totalMilliseconds")
                .and_then(|x| x.as_u64())
                .unwrap_or(0)
                / 1000;
            tracks.push(SpotifyTrack {
                title,
                artists,
                duration_secs,
            });
        }
        offset += PAGE;
        if (total > 0 && offset >= total) || offset > 10_000 {
            break;
        }
    }

    if tracks.is_empty() {
        return Err("pathfinder returned no tracks".into());
    }
    Ok(SpotifyPlaylist { name, tracks })
}

#[cfg(test)]
mod tests {
    /// Live smoke test: full-fetch a big public playlist via the embed →
    /// pathfinder path and assert we get well past the old ~100 cap.
    /// Ignored by default (needs network). Run with:
    ///   cargo test -p server pathfinder_full_fetch -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn pathfinder_full_fetch() {
        // "All Out 2000s" — a large editorial playlist (>100 tracks), so this
        // also exercises offset pagination across the old ~100 boundary.
        let id = "37i9dQZF1DX4o1oenSJRJd";
        let pl = super::super::embed::fetch_public(
            super::super::SpotifyEntityKind::Playlist,
            id,
        )
        .await
        .expect("fetch_public failed");
        eprintln!("name='{}' tracks={}", pl.name, pl.tracks.len());
        eprintln!(
            "first: {} — {}",
            pl.tracks.first().map(|t| t.title.as_str()).unwrap_or(""),
            pl.tracks
                .first()
                .map(|t| t.artists.join(", "))
                .unwrap_or_default()
        );
        assert!(!pl.tracks.is_empty());
    }
}
