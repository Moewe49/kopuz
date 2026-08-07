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

/// Spotify answers every error with a JSON body explaining it. Swallowing that
/// and reporting only the status turned "limit=24 is above the new maximum of
/// 10" into a bare "400 Bad Request" that took a web search to decode.
pub(super) fn describe_error(status: reqwest::StatusCode, body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let field = |name: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.pointer(&format!("/error/{name}")))
            .and_then(|m| m.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    // `reason` is where Spotify puts the machine-readable cause when `message`
    // is a useless "Forbidden" — e.g. PREMIUM_REQUIRED, QUOTA_EXCEEDED. It is
    // the difference between a dead end and knowing what to change.
    let detail = match (field("message"), field("reason")) {
        (Some(m), Some(r)) if m != r => format!("{m} ({r})"),
        (Some(m), _) => m,
        (None, Some(r)) => r,
        (None, None) => body.chars().take(200).collect::<String>().trim().to_string(),
    };
    if detail.is_empty() {
        format!("Spotify API {status}")
    } else {
        format!("Spotify API {status}: {detail}")
    }
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
        let body = resp.text().await.unwrap_or_default();
        return Err(describe_error(status, &body));
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
///
/// Spotify's February/March 2026 Dev Mode migration renamed
/// `/playlists/{id}/tracks` to `/playlists/{id}/items` and, with it, the entry
/// key inside each page from `track` to `item`. Development Mode apps get 403
/// on the old path — that was the "Spotify API 403 Forbidden" on import. The
/// legacy path is still tried as a fallback because the anonymous web-player
/// token used for public imports is not a Dev Mode app and may still serve it.
pub async fn fetch_playlist(token: &str, id: &str) -> Result<SpotifyPlaylist, String> {
    let meta = get_json(&format!("{API}/playlists/{id}?fields=name"), token).await?;
    let name = meta
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Spotify import")
        .to_string();

    // (path, per-entry field) — current shape first.
    const SHAPES: [(&str, &str); 2] = [("items", "item"), ("tracks", "track")];
    let mut last_err = String::new();
    for (path, field) in SHAPES {
        let mut tracks = Vec::new();
        let mut url = format!(
            "{API}/playlists/{id}/{path}?limit=100&fields=next,items({field}(name,duration_ms,artists(name)))"
        );
        let mut ok = true;
        loop {
            match get_json(&url, token).await {
                Ok(page) => {
                    collect_track_items(&page, &mut tracks);
                    match page.get("next").and_then(|v| v.as_str()) {
                        Some(next) => url = next.to_string(),
                        None => break,
                    }
                }
                Err(e) => {
                    last_err = e;
                    ok = false;
                    break;
                }
            }
        }
        if ok && !tracks.is_empty() {
            return Ok(SpotifyPlaylist { name, tracks });
        }
        if ok {
            // Reached the end with nothing in it — an empty playlist reads the
            // same as a shape mismatch, so try the other shape before giving up.
            last_err = "Spotify returned no tracks for this playlist".to_string();
        }
    }
    Err(last_err)
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
        // Each entry wraps the track; local/removed entries are null. The
        // February 2026 migration renamed that wrapper from `track` to `item` —
        // reading only the old name is what made Liked Songs come back empty
        // and report "no tracks could be matched".
        let Some(track) = it
            .get("item")
            .or_else(|| it.get("track"))
            .filter(|t| !t.is_null())
        else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_both_the_old_and_the_new_entry_wrapper() {
        // February 2026 renamed the per-entry wrapper from `track` to `item`.
        // Reading only `track` is what made Liked Songs come back empty.
        let new_shape = json!({ "items": [
            { "item": { "name": "New", "duration_ms": 60000, "artists": [{ "name": "A" }] } }
        ]});
        let old_shape = json!({ "items": [
            { "track": { "name": "Old", "duration_ms": 60000, "artists": [{ "name": "A" }] } }
        ]});

        let mut out = Vec::new();
        collect_track_items(&new_shape, &mut out);
        collect_track_items(&old_shape, &mut out);

        let titles: Vec<&str> = out.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["New", "Old"]);
    }

    #[test]
    fn skips_null_and_unnamed_entries_in_either_shape() {
        let page = json!({ "items": [
            { "item": null },
            { "track": null },
            {},
            { "item": { "duration_ms": 1000 } },
            { "item": { "name": "Kept", "duration_ms": 90000, "artists": [] } }
        ]});
        let mut out = Vec::new();
        collect_track_items(&page, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Kept");
        assert_eq!(out[0].duration_secs, 90);
    }

    #[test]
    fn surfaces_spotifys_own_explanation_instead_of_a_bare_status() {
        let status = reqwest::StatusCode::BAD_REQUEST;
        let body = r#"{"error":{"status":400,"message":"Invalid limit: max 10"}}"#;
        let msg = describe_error(status, body);
        assert!(
            msg.contains("Invalid limit: max 10"),
            "the reason must reach the user, got: {msg}",
        );
    }

    #[test]
    fn falls_back_to_the_raw_body_when_it_is_not_the_usual_json() {
        let msg = describe_error(reqwest::StatusCode::FORBIDDEN, "<html>nope</html>");
        assert!(msg.contains("403"), "got: {msg}");
        assert!(msg.contains("nope"), "got: {msg}");
    }

    #[test]
    fn surfaces_the_machine_readable_reason_when_the_message_says_nothing() {
        // Exactly what Spotify answers for a Dev Mode playlist read: a message
        // that only repeats the status. The `reason` is the part worth showing.
        let body = r#"{"error":{"status":403,"message":"Forbidden","reason":"PREMIUM_REQUIRED"}}"#;
        let msg = describe_error(reqwest::StatusCode::FORBIDDEN, body);
        assert!(msg.contains("PREMIUM_REQUIRED"), "got: {msg}");
    }

    #[test]
    fn does_not_repeat_itself_when_message_and_reason_agree() {
        let body = r#"{"error":{"status":403,"message":"Forbidden","reason":"Forbidden"}}"#;
        let msg = describe_error(reqwest::StatusCode::FORBIDDEN, body);
        assert_eq!(msg.matches("Forbidden").count(), 2, "status + one detail, got: {msg}");
    }

    #[test]
    fn an_empty_body_still_names_the_status() {
        let msg = describe_error(reqwest::StatusCode::FORBIDDEN, "");
        assert!(msg.contains("403"), "got: {msg}");
    }
}
