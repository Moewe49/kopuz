//! Thin Spotify Web API client for the PKCE-connected path. Only the
//! read endpoints the importer needs: the user's playlist list, a
//! playlist's tracks, and Liked Songs — all fully paginated.

use std::sync::OnceLock;

use serde_json::Value;

use super::{SpotifyPlaylist, SpotifyTrack};

const API: &str = "https://api.spotify.com/v1";

/// Shared client for everything Spotify (embed scrape, OAuth, API).
pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().build().expect("reqwest client"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistSummary {
    pub id: String,
    pub name: String,
    pub track_count: u64,
    pub owner: String,
}

async fn get_json(url: &str, token: &str) -> Result<Value, String> {
    let resp = http_client()
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Spotify API HTTP: {e}"))?;
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err("Spotify token expired".into());
    }
    if !status.is_success() {
        return Err(format!("Spotify API {status} for {url}"));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("Spotify API JSON: {e}"))
}

/// All playlists in the user's library (owned + followed).
pub async fn list_user_playlists(token: &str) -> Result<Vec<PlaylistSummary>, String> {
    let mut out = Vec::new();
    let mut url = format!("{API}/me/playlists?limit=50");
    loop {
        let page = get_json(&url, token).await?;
        if let Some(items) = page.get("items").and_then(|v| v.as_array()) {
            for it in items {
                let Some(id) = it.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                out.push(PlaylistSummary {
                    id: id.to_string(),
                    name: it
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Untitled")
                        .to_string(),
                    track_count: it
                        .pointer("/tracks/total")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    owner: it
                        .pointer("/owner/display_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
        match page.get("next").and_then(|v| v.as_str()) {
            Some(next) => url = next.to_string(),
            None => break,
        }
    }
    Ok(out)
}

/// Every track of one album, paginated. Album track objects are simplified
/// (no `track` wrapper), so they parse slightly differently from playlists.
pub async fn fetch_album(token: &str, id: &str) -> Result<SpotifyPlaylist, String> {
    let meta = get_json(&format!("{API}/albums/{id}?market=US"), token).await?;
    let name = meta
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Spotify import")
        .to_string();

    let mut tracks = Vec::new();
    let mut url = format!("{API}/albums/{id}/tracks?limit=50&market=US");
    loop {
        let page = get_json(&url, token).await?;
        if let Some(items) = page.get("items").and_then(|v| v.as_array()) {
            for track in items {
                push_track(track, &mut tracks);
            }
        }
        match page.get("next").and_then(|v| v.as_str()) {
            Some(next) => url = next.to_string(),
            None => break,
        }
    }
    Ok(SpotifyPlaylist { name, tracks })
}

/// Every track of one playlist, paginated 100 at a time.
pub async fn fetch_playlist(token: &str, id: &str) -> Result<SpotifyPlaylist, String> {
    let meta = get_json(&format!("{API}/playlists/{id}?fields=name"), token).await?;
    let name = meta
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Spotify import")
        .to_string();

    let mut tracks = Vec::new();
    let mut url = format!(
        "{API}/playlists/{id}/tracks?limit=100&fields=next,items(track(name,duration_ms,artists(name)))"
    );
    loop {
        let page = get_json(&url, token).await?;
        collect_track_items(&page, &mut tracks);
        match page.get("next").and_then(|v| v.as_str()) {
            Some(next) => url = next.to_string(),
            None => break,
        }
    }
    Ok(SpotifyPlaylist { name, tracks })
}

/// The user's Liked Songs, newest first (Spotify's order).
pub async fn fetch_liked_songs(token: &str) -> Result<SpotifyPlaylist, String> {
    let mut tracks = Vec::new();
    let mut url = format!("{API}/me/tracks?limit=50");
    loop {
        let page = get_json(&url, token).await?;
        collect_track_items(&page, &mut tracks);
        match page.get("next").and_then(|v| v.as_str()) {
            Some(next) => url = next.to_string(),
            None => break,
        }
    }
    Ok(SpotifyPlaylist {
        name: "Liked Songs".to_string(),
        tracks,
    })
}

fn collect_track_items(page: &Value, out: &mut Vec<SpotifyTrack>) {
    let Some(items) = page.get("items").and_then(|v| v.as_array()) else {
        return;
    };
    for it in items {
        // Playlist items wrap the track; local/removed entries are null.
        let Some(track) = it.get("track").filter(|t| !t.is_null()) else {
            continue;
        };
        push_track(track, out);
    }
}

/// Append one Spotify track object (`{name, duration_ms, artists}`) to `out`,
/// skipping unnamed entries. Shared by playlist and album parsing.
fn push_track(track: &Value, out: &mut Vec<SpotifyTrack>) {
    let Some(title) = track.get("name").and_then(|v| v.as_str()) else {
        return;
    };
    if title.is_empty() {
        return;
    }
    let artists: Vec<String> = track
        .get("artists")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    let duration_secs = track
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .map(|ms| ms / 1000)
        .unwrap_or(0);
    out.push(SpotifyTrack {
        title: title.to_string(),
        artists,
        duration_secs,
    });
}
