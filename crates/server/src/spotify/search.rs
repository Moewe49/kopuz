//! Spotify catalogue search — songs, playlists and albums.
//!
//! Spotify's audio is DRM-protected and cannot be streamed by a third-party
//! client, so this is a *discovery* surface: it searches Spotify's catalogue
//! (which is often better curated than YouTube's for albums and editorial
//! playlists) and every result is played through the existing
//! [`matcher`](super::matcher) — the Spotify track is matched to its YouTube
//! Music equivalent, exactly like the import path already does. That keeps one
//! playback engine, one download path and one offline cache.
//!
//! `/v1/search` needs a bearer token, so this requires the account connection
//! from [`auth`](super::auth) (the same one playlist import uses). There is no
//! anonymous search path: Spotify's public web-player token endpoint is
//! deliberately hostile to non-browser clients.

use serde_json::Value;

use super::api::http_client;
use super::{SpotifyEntityKind, SpotifyTrack};

const API: &str = "https://api.spotify.com/v1";

/// One track from a search, carrying the extras the UI needs on top of what
/// the matcher consumes (cover art, album, a stable id for keys).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTrack {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_secs: u64,
    pub cover_url: Option<String>,
}

impl SearchTrack {
    /// The matcher's view of this track.
    pub fn as_spotify_track(&self) -> SpotifyTrack {
        SpotifyTrack {
            title: self.title.clone(),
            artists: self.artists.clone(),
            duration_secs: self.duration_secs,
        }
    }

    /// The open.spotify.com URL — what the import dialog accepts.
    pub fn share_url(&self) -> String {
        format!("https://open.spotify.com/track/{}", self.id)
    }
}

/// A playlist or album hit. Both render as a card and both open into a track
/// list, so they share a type; [`kind`](Self::kind) picks the fetch path.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchEntity {
    pub kind: SpotifyEntityKind,
    pub id: String,
    pub name: String,
    /// Owner (playlist) or artists (album).
    pub subtitle: String,
    pub cover_url: Option<String>,
    /// Playlists report their length; albums report their track count.
    pub track_count: Option<u64>,
}

impl SearchEntity {
    pub fn share_url(&self) -> String {
        format!(
            "https://open.spotify.com/{}/{}",
            self.kind.as_path(),
            self.id
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub tracks: Vec<SearchTrack>,
    pub playlists: Vec<SearchEntity>,
    pub albums: Vec<SearchEntity>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.playlists.is_empty() && self.albums.is_empty()
    }
}

/// Maximum `limit` `/v1/search` accepts. Was 50 until Spotify's February 2026
/// Dev Mode migration cut it to 10; sending more is a flat `400 Bad Request`.
const MAX_SEARCH_LIMIT: u32 = 10;

/// Search Spotify for `query`. `limit` applies per result type and is clamped
/// to [`MAX_SEARCH_LIMIT`].
pub async fn search(token: &str, query: &str, limit: u32) -> Result<SearchResults, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(SearchResults::default());
    }
    let url = format!(
        "{API}/search?q={}&type=track,playlist,album&limit={}",
        urlenc(query),
        limit.clamp(1, MAX_SEARCH_LIMIT),
    );
    let resp = http_client()
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Spotify search HTTP: {e}"))?;
    let status = resp.status();
    if status.as_u16() == 401 {
        // The caller refreshes and retries; make the reason unambiguous.
        super::auth::forget_access();
        return Err("Spotify token expired".into());
    }
    if !status.is_success() {
        // Spotify explains itself in the body; reporting only the status is how
        // "limit above the maximum" reached the user as a bare 400.
        let body = resp.text().await.unwrap_or_default();
        return Err(super::api::describe_error(status, &body));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| format!("Spotify search JSON: {e}"))?;
    Ok(parse_results(&json))
}

fn parse_results(json: &Value) -> SearchResults {
    SearchResults {
        tracks: items(json, "/tracks/items")
            .iter()
            .filter_map(parse_track)
            .collect(),
        playlists: items(json, "/playlists/items")
            .iter()
            .filter_map(|v| parse_entity(v, SpotifyEntityKind::Playlist))
            .collect(),
        albums: items(json, "/albums/items")
            .iter()
            .filter_map(|v| parse_entity(v, SpotifyEntityKind::Album))
            .collect(),
    }
}

fn items<'a>(json: &'a Value, pointer: &str) -> &'a [Value] {
    json.pointer(pointer)
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

fn parse_track(v: &Value) -> Option<SearchTrack> {
    // Spotify occasionally pads search pages with nulls.
    if v.is_null() {
        return None;
    }
    let id = v.get("id").and_then(|v| v.as_str())?.to_string();
    let title = v.get("name").and_then(|v| v.as_str())?.to_string();
    if title.is_empty() {
        return None;
    }
    Some(SearchTrack {
        id,
        title,
        artists: names(v.get("artists")),
        album: v
            .pointer("/album/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        duration_secs: v
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .map(|ms| ms / 1000)
            .unwrap_or(0),
        cover_url: smallest_image(v.pointer("/album/images")),
    })
}

fn parse_entity(v: &Value, kind: SpotifyEntityKind) -> Option<SearchEntity> {
    if v.is_null() {
        return None;
    }
    let id = v.get("id").and_then(|v| v.as_str())?.to_string();
    let name = v.get("name").and_then(|v| v.as_str())?.to_string();
    if name.is_empty() {
        return None;
    }
    let subtitle = match kind {
        SpotifyEntityKind::Playlist => v
            .pointer("/owner/display_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        SpotifyEntityKind::Album => names(v.get("artists")).join(", "),
    };
    Some(SearchEntity {
        kind,
        id,
        name,
        subtitle,
        cover_url: smallest_image(v.get("images")),
        track_count: v
            .pointer("/tracks/total")
            .or_else(|| v.get("total_tracks"))
            .and_then(|v| v.as_u64()),
    })
}

fn names(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Spotify lists images largest-first. Rows and cards are small, so take the
/// last (smallest) one that is still at least thumbnail-sized rather than
/// pulling a 640px JPEG per row.
fn smallest_image(v: Option<&Value>) -> Option<String> {
    let arr = v?.as_array()?;
    let usable = |img: &&Value| {
        img.get("width")
            .and_then(|w| w.as_u64())
            .is_none_or(|w| w >= 100)
    };
    arr.iter()
        .filter(usable)
        .next_back()
        .or_else(|| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
}

fn urlenc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "tracks": { "items": [
                null,
                { "id": "t1", "name": "Song One", "duration_ms": 213000,
                  "artists": [{ "name": "Artist A" }, { "name": "Artist B" }],
                  "album": { "name": "Album X", "images": [
                      { "url": "https://i/640.jpg", "width": 640 },
                      { "url": "https://i/300.jpg", "width": 300 },
                      { "url": "https://i/64.jpg", "width": 64 }
                  ]}},
                { "name": "no id" }
            ]},
            "playlists": { "items": [
                null,
                { "id": "37i9dQZF1DXcBWIGoYBM5M", "name": "Chill Mix",
                  "owner": { "display_name": "Spotify" },
                  "images": [{ "url": "https://i/p.jpg", "width": 300 }],
                  "tracks": { "total": 50 } }
            ]},
            "albums": { "items": [
                { "id": "a1", "name": "The Album", "total_tracks": 12,
                  "artists": [{ "name": "Artist A" }],
                  "images": [{ "url": "https://i/a.jpg", "width": 300 }] }
            ]}
        })
    }

    #[test]
    fn parses_tracks_playlists_and_albums() {
        let r = parse_results(&sample());

        assert_eq!(r.tracks.len(), 1, "null and id-less entries are dropped");
        let t = &r.tracks[0];
        assert_eq!(t.title, "Song One");
        assert_eq!(t.artists, vec!["Artist A", "Artist B"]);
        assert_eq!(t.album, "Album X");
        assert_eq!(t.duration_secs, 213);

        assert_eq!(r.playlists.len(), 1);
        assert_eq!(r.playlists[0].kind, SpotifyEntityKind::Playlist);
        assert_eq!(r.playlists[0].subtitle, "Spotify");
        assert_eq!(r.playlists[0].track_count, Some(50));

        assert_eq!(r.albums.len(), 1);
        assert_eq!(r.albums[0].kind, SpotifyEntityKind::Album);
        assert_eq!(r.albums[0].subtitle, "Artist A");
        assert_eq!(r.albums[0].track_count, Some(12));
    }

    #[test]
    fn picks_a_thumbnail_sized_cover_not_the_640px_one() {
        let r = parse_results(&sample());
        assert_eq!(
            r.tracks[0].cover_url.as_deref(),
            Some("https://i/300.jpg"),
            "64px is below the usable floor, 640px wastes bandwidth per row",
        );
    }

    #[test]
    fn share_urls_round_trip_through_the_url_parser() {
        let r = parse_results(&sample());
        let (kind, id) = super::super::parse_spotify_url(&r.playlists[0].share_url()).unwrap();
        assert_eq!(kind, SpotifyEntityKind::Playlist);
        assert_eq!(id, "37i9dQZF1DXcBWIGoYBM5M");
    }

    #[test]
    fn empty_response_is_empty_not_an_error() {
        assert!(parse_results(&json!({})).is_empty());
    }
}
