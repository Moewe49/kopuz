//! Shareable playlist codes.
//!
//! A playlist is turned into one pasteable token — `kopuz:pl:<base64url>` —
//! that carries everything the receiving app needs. No server, no account, no
//! link shortener: the code *is* the playlist, so it works over any channel
//! that can carry text and keeps working when nobody is hosting anything.
//!
//! What travels depends on where a track lives:
//!
//! - `ytmusic:` and `soundcloud:` tracks carry their id, so the recipient
//!   resolves the exact same track.
//! - A file on the sender's disk cannot travel — the path is meaningless on
//!   another machine. Its title/artist/duration go along instead, and the
//!   recipient matches those against YouTube Music the way the Spotify import
//!   already does.
//!
//! Length is the whole design constraint: a Discord message holds 2000
//! characters, so every byte spent is a track that can't be shared. Hence a
//! line format rather than JSON, and — the big one — a portable track sends
//! only its id. Its title and artist are already known to whoever resolves it,
//! so shipping them would more than triple the code for nothing. Only a local
//! track, which has no id worth sending, spends bytes on metadata.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Token prefix. Present so a pasted code is recognisable as ours, and so a
/// future format can be told apart from this one.
pub const PREFIX: &str = "kopuz:pl:";

/// Payload format version. Bumped if the line layout ever changes; a decoder
/// refuses anything it doesn't know rather than guessing.
const VERSION: u8 = 1;

/// One track inside a shared playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedTrack {
    /// Portable source path (`ytmusic:<id>` / `soundcloud:<id>`), or `None` for
    /// a track that only existed as a local file on the sender's machine.
    pub path: Option<String>,
    pub title: String,
    pub artist: String,
    pub duration: u64,
}

impl SharedTrack {
    /// Whether the recipient can play this directly, or has to look it up.
    pub fn is_portable(&self) -> bool {
        self.path.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedPlaylist {
    pub name: String,
    pub tracks: Vec<SharedTrack>,
}

/// A path is worth sending only if it means the same thing on someone else's
/// machine. A filesystem path does not.
fn portable_path(path: &str) -> Option<String> {
    let scheme = path.split(':').next()?;
    matches!(scheme, "ytmusic" | "soundcloud").then(|| path.to_string())
}

/// Build a shareable track from whatever the app has locally.
pub fn shared_track(path: &str, title: &str, artist: &str, duration: u64) -> SharedTrack {
    SharedTrack {
        path: portable_path(path),
        title: title.to_string(),
        artist: artist.to_string(),
        duration,
    }
}

/// `|` separates fields and `\n` separates records, so both have to survive
/// inside a value — a track called "A|B" must not silently become two fields.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\p"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('p') => out.push('|'),
            Some('n') => out.push('\n'),
            // Unknown escape: keep it literal rather than dropping data.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Split a portable path into its one-character tag and the id worth sending.
/// YouTube keeps only the video id — the trailing segments are derived data
/// the recipient rebuilds anyway.
fn split_source(path: &str) -> Option<(char, &str)> {
    let (scheme, rest) = path.split_once(':')?;
    let (tag, id) = match scheme {
        "ytmusic" => ('y', rest.split(':').next()?),
        "soundcloud" => ('s', rest),
        _ => return None,
    };
    (!id.is_empty()).then_some((tag, id))
}

/// Encode a playlist into one pasteable token.
pub fn encode(playlist: &SharedPlaylist) -> String {
    let mut body = format!("{VERSION}\n{}\n", escape(&playlist.name));
    for t in &playlist.tracks {
        match t.path.as_deref().and_then(split_source) {
            // Id only. Everything else about the track follows from it.
            Some((tag, id)) => body.push_str(&format!("{tag}{}\n", escape(id))),
            // No id to send — metadata is all the recipient can match on.
            None => body.push_str(&format!(
                "l|{}|{}|{}\n",
                escape(&t.title),
                escape(&t.artist),
                t.duration,
            )),
        }
    }
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(body.as_bytes()))
}

/// Decode a token produced by [`encode`].
///
/// Tolerant about what surrounds the token — people paste with stray
/// whitespace, quotes or a chat client's zero-width junk attached — but strict
/// about the payload: an unknown version or unreadable base64 is an error, not
/// a half-recovered playlist.
pub fn decode(input: &str) -> Result<SharedPlaylist, String> {
    let trimmed = input.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>');
    let payload = trimmed
        .find(PREFIX)
        .map(|i| &trimmed[i + PREFIX.len()..])
        .ok_or("That doesn't look like a Kopuz playlist link.")?;
    let payload: String = payload
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if payload.is_empty() {
        return Err("The playlist link is empty.".into());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .map_err(|_| "The playlist link is damaged — it looks truncated.".to_string())?;
    let text = String::from_utf8(bytes)
        .map_err(|_| "The playlist link is damaged.".to_string())?;

    let mut lines = text.split('\n');
    let version = lines.next().unwrap_or_default().trim();
    if version != VERSION.to_string() {
        return Err(format!(
            "This link was made by a different Kopuz version (format {version}) — update and try again."
        ));
    }
    let name = unescape(lines.next().unwrap_or_default());

    let mut tracks = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (tag, rest) = line.split_at(1);
        let track = match tag {
            "y" => SharedTrack {
                path: Some(format!("ytmusic:{}", unescape(rest))),
                title: String::new(),
                artist: String::new(),
                duration: 0,
            },
            "s" => SharedTrack {
                path: Some(format!("soundcloud:{}", unescape(rest))),
                title: String::new(),
                artist: String::new(),
                duration: 0,
            },
            "l" => {
                let mut parts = rest.trim_start_matches('|').split('|');
                let title = unescape(parts.next().unwrap_or_default());
                let artist = unescape(parts.next().unwrap_or_default());
                let duration = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
                if title.is_empty() {
                    continue;
                }
                SharedTrack { path: None, title, artist, duration }
            }
            // Unknown record kind from a newer minor format — skip it rather
            // than refuse the whole playlist.
            _ => continue,
        };
        tracks.push(track);
    }
    if tracks.is_empty() {
        return Err("That playlist link contains no tracks.".into());
    }
    Ok(SharedPlaylist { name, tracks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SharedPlaylist {
        SharedPlaylist {
            name: "Late night".to_string(),
            tracks: vec![
                SharedTrack {
                    path: Some("ytmusic:dQw4w9WgXcQ:x".into()),
                    title: "Never Gonna Give You Up".into(),
                    artist: "Rick Astley".into(),
                    duration: 213,
                },
                SharedTrack {
                    path: Some("soundcloud:9f2a1b".into()),
                    title: "Some Remix".into(),
                    artist: "Someone".into(),
                    duration: 401,
                },
                SharedTrack {
                    path: None,
                    title: "A Rip From 2009".into(),
                    artist: "Unknown".into(),
                    duration: 0,
                },
            ],
        }
    }

    #[test]
    fn portable_tracks_travel_as_a_bare_id() {
        // Deliberate: title/artist are derivable from the id, so sending them
        // would only make the code longer. The trailing YouTube path segment
        // is derived data too and is dropped.
        let decoded = decode(&encode(&sample())).expect("should decode");
        assert_eq!(decoded.name, "Late night");
        assert_eq!(decoded.tracks[0].path.as_deref(), Some("ytmusic:dQw4w9WgXcQ"));
        assert_eq!(decoded.tracks[1].path.as_deref(), Some("soundcloud:9f2a1b"));
        assert_eq!(decoded.tracks[0].title, "");
    }

    #[test]
    fn a_local_track_keeps_the_metadata_it_needs_to_be_found_again() {
        // It has no id, so title/artist/duration are the only way the
        // recipient can match it — they must survive intact.
        let decoded = decode(&encode(&sample())).expect("should decode");
        let local = &decoded.tracks[2];
        assert_eq!(local.path, None);
        assert_eq!(local.title, "A Rip From 2009");
        assert_eq!(local.artist, "Unknown");
        assert!(!local.is_portable());
    }

    #[test]
    fn a_local_file_path_never_leaves_the_machine() {
        // A path from the sender's disk is meaningless to the recipient and
        // leaks their folder layout — only the metadata may travel.
        let t = shared_track(r"C:\Users\someone\Music\track.mp3", "Title", "Artist", 100);
        assert_eq!(t.path, None);
        let code = encode(&SharedPlaylist { name: "x".into(), tracks: vec![t] });
        let raw = String::from_utf8(
            URL_SAFE_NO_PAD.decode(code.trim_start_matches(PREFIX)).unwrap(),
        )
        .unwrap();
        assert!(!raw.contains("someone"), "sender's path leaked: {raw}");
        assert!(!raw.contains(".mp3"), "sender's path leaked: {raw}");
    }

    #[test]
    fn portable_sources_keep_their_id() {
        assert_eq!(
            shared_track("ytmusic:abc:def", "t", "a", 1).path.as_deref(),
            Some("ytmusic:abc:def"),
        );
        assert_eq!(
            shared_track("soundcloud:9f2a", "t", "a", 1).path.as_deref(),
            Some("soundcloud:9f2a"),
        );
    }

    #[test]
    fn separators_inside_a_title_survive() {
        let pl = SharedPlaylist {
            name: "Mix | 2026\nedition".into(),
            tracks: vec![SharedTrack {
                path: None,
                title: r"A|B\C".into(),
                artist: "x\ny".into(),
                duration: 7,
            }],
        };
        assert_eq!(decode(&encode(&pl)).unwrap(), pl);
    }

    #[test]
    fn tolerates_how_people_actually_paste() {
        let code = encode(&sample());
        for wrapped in [
            format!("  {code}  "),
            format!("\"{code}\""),
            format!("<{code}>"),
            format!("schau mal: {code}"),
            format!("{code}\n"),
        ] {
            assert!(decode(&wrapped).is_ok(), "failed on: {wrapped}");
        }
    }

    #[test]
    fn rejects_a_future_format_instead_of_guessing() {
        let body = format!("{}\nName\n|t|a|1\n", VERSION + 1);
        let code = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(body));
        let err = decode(&code).expect_err("must refuse");
        assert!(err.contains("different Kopuz version"), "got: {err}");
    }

    #[test]
    fn rejects_things_that_are_not_playlist_links() {
        assert!(decode("").is_err());
        assert!(decode("https://open.spotify.com/playlist/abc").is_err());
        assert!(decode(PREFIX).is_err());
        assert!(decode(&format!("{PREFIX}!!!not-base64!!!")).is_err());
    }

    #[test]
    fn an_empty_playlist_is_an_error_not_a_silent_import() {
        let code = encode(&SharedPlaylist { name: "Empty".into(), tracks: vec![] });
        assert!(decode(&code).is_err());
    }

    #[test]
    fn stays_pasteable_for_a_large_playlist() {
        let tracks: Vec<SharedTrack> = (0..300)
            .map(|i| SharedTrack {
                path: Some(format!("ytmusic:vid{i:08}")),
                title: format!("Track number {i}"),
                artist: "An Artist Name".into(),
                duration: 200 + i,
            })
            .collect();
        let code = encode(&SharedPlaylist { name: "Big".into(), tracks });
        // Guards against a format change that quietly makes sharing unusable.
        assert!(
            code.len() < 8_000,
            "300 tracks produced {} chars — too long to paste",
            code.len(),
        );
        assert_eq!(decode(&code).unwrap().tracks.len(), 300);
    }
}
