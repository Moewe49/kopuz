//! Anonymous fetch of a public playlist/album. Primary path: pull the
//! anonymous Web API bearer token the `open.spotify.com/embed` page carries
//! and page through the FULL track list via the public API (no login, no
//! cap). Fallback: parse the ≈100 tracks the embed inlines in its Next.js
//! `__NEXT_DATA__` blob (`props.pageProps.state.data.entity.trackList`) if
//! the token path fails.

use serde_json::Value;

use super::{SpotifyEntityKind, SpotifyPlaylist, SpotifyTrack};

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) \
                          Gecko/20100101 Firefox/140.0";

pub async fn fetch_public(kind: SpotifyEntityKind, id: &str) -> Result<SpotifyPlaylist, String> {
    let url = format!("https://open.spotify.com/embed/{}/{id}", kind.as_path());
    let html = super::api::http_client()
        .get(&url)
        .header("User-Agent", BROWSER_UA)
        .header("Accept-Language", "en")
        .send()
        .await
        .map_err(|e| format!("Spotify embed HTTP: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Spotify embed HTTP: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Spotify embed body: {e}"))?;

    // The embed page only inlines ~100 tracks, but it also ships an
    // anonymous Web API access token. Use it to fetch the FULL track list
    // (no user login needed) for public playlists/albums. Playlists go
    // through the web player's pathfinder GraphQL endpoint — the only path
    // that reads non-owned playlists in full (the REST API 403s those from a
    // dev-mode app and 429s the anonymous token). Albums are small enough
    // that the REST endpoint, then the inlined ≈100, cover them.
    // Fall back to the inlined ≈100-track list if every token path fails.
    if let Some(token) = extract_access_token(&html) {
        let full = match kind {
            SpotifyEntityKind::Playlist => match super::pathfinder::fetch_playlist(&token, id).await
            {
                Ok(pl) if !pl.tracks.is_empty() => Ok(pl),
                _ => super::api::fetch_playlist(&token, id).await,
            },
            SpotifyEntityKind::Album => super::api::fetch_album(&token, id).await,
        };
        if let Ok(pl) = full
            && !pl.tracks.is_empty()
        {
            return Ok(pl);
        }
    }
    parse_embed_html(&html)
}

/// Pull the anonymous Web API bearer token the embed page carries
/// (`"accessToken":"BQ…"`). Present on the public embed; absent only if
/// Spotify changes the page shape, in which case we fall back to scraping.
fn extract_access_token(html: &str) -> Option<String> {
    let marker = "\"accessToken\":\"";
    let start = html.find(marker)? + marker.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let token = &rest[..end];
    (token.len() > 20).then(|| token.to_string())
}

fn parse_embed_html(html: &str) -> Result<SpotifyPlaylist, String> {
    let marker = "id=\"__NEXT_DATA__\"";
    let at = html
        .find(marker)
        .ok_or("Spotify embed: no __NEXT_DATA__ blob (private or removed?)")?;
    let json_start = html[at..]
        .find('>')
        .map(|i| at + i + 1)
        .ok_or("Spotify embed: malformed script tag")?;
    let json_end = html[json_start..]
        .find("</script>")
        .map(|i| json_start + i)
        .ok_or("Spotify embed: unterminated script tag")?;
    let data: Value = serde_json::from_str(html[json_start..json_end].trim())
        .map_err(|e| format!("Spotify embed JSON: {e}"))?;

    let entity = data
        .pointer("/props/pageProps/state/data/entity")
        .ok_or("Spotify embed: entity missing — playlist may be private")?;
    let name = entity
        .get("name")
        .or_else(|| entity.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("Spotify import")
        .to_string();
    let tracks = entity
        .pointer("/trackList")
        .and_then(|v| v.as_array())
        .map(|rows| rows.iter().filter_map(parse_track_row).collect::<Vec<_>>())
        .unwrap_or_default();
    if tracks.is_empty() {
        return Err("Spotify embed: no tracks found — playlist may be private or empty".into());
    }
    Ok(SpotifyPlaylist { name, tracks })
}

fn parse_track_row(row: &Value) -> Option<SpotifyTrack> {
    let title = row.get("title")?.as_str()?.to_string();
    // `subtitle` is the artist list joined with "," + NBSP.
    let artists: Vec<String> = row
        .get("subtitle")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(|a| {
                    a.trim_matches(|c: char| c.is_whitespace() || c == '\u{a0}')
                        .to_string()
                })
                .filter(|a| !a.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let duration_secs = row
        .get("duration")
        .and_then(|v| v.as_u64())
        .map(|ms| ms / 1000)
        .unwrap_or(0);
    Some(SpotifyTrack {
        title,
        artists,
        duration_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_embed_html() {
        let html = r#"<html><script id="__NEXT_DATA__" type="application/json">
        {"props":{"pageProps":{"state":{"data":{"entity":{
          "name":"My Mix",
          "trackList":[
            {"title":"Song A","subtitle":"Artist One, Artist Two","duration":197949},
            {"title":"Song B","subtitle":"Solo Act","duration":85400}
          ]}}}}}}
        </script></html>"#;
        let pl = parse_embed_html(html).unwrap();
        assert_eq!(pl.name, "My Mix");
        assert_eq!(pl.tracks.len(), 2);
        assert_eq!(pl.tracks[0].artists, vec!["Artist One", "Artist Two"]);
        assert_eq!(pl.tracks[0].duration_secs, 197);
        assert_eq!(pl.tracks[1].artists, vec!["Solo Act"]);
    }

    #[test]
    fn errors_on_missing_blob() {
        assert!(parse_embed_html("<html>nope</html>").is_err());
    }
}
