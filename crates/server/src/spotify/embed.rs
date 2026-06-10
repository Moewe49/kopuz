//! Anonymous fetch of a public playlist/album via the
//! `open.spotify.com/embed` page. The page inlines a Next.js
//! `__NEXT_DATA__` JSON blob whose
//! `props.pageProps.state.data.entity` carries the name and a
//! `trackList` of `{title, subtitle, duration}` rows — verified live
//! against both playlist and album embeds. The embed serves roughly
//! the first 100 tracks; callers surface that cap to the user.

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
    parse_embed_html(&html)
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
