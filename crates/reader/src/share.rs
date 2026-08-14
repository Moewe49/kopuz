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
//! # Why the payload is binary
//!
//! Length is the whole design constraint: a Discord message holds 2000
//! characters, so every byte spent is a track that can't be shared. The first
//! version wrote text lines and base64'd them, which paid the base64 tax on
//! data that was *already* base64 — a YouTube id is eleven base64url
//! characters, and wrapping those in another base64 layer costs a third again
//! for nothing.
//!
//! So an id is packed back down to the eight raw bytes it came from, and the
//! payload is binary throughout: length-prefixed instead of delimited, which
//! also retires the escaping the line format needed. A YouTube track costs 8
//! bytes here against 13 before, and an all-YouTube playlist drops its
//! per-record tag as well — around 38% shorter end to end.
//!
//! Packing an id is only safe when it survives the round trip. Eleven base64
//! characters carry 66 bits and eight bytes hold 64, so the last two bits are
//! dropped on the way in; real YouTube ids are canonical 64-bit values and
//! come back identical, but an id that isn't would silently decode to a
//! *different video* on the recipient's machine. Every pack is therefore
//! verified by re-encoding, and anything that doesn't match falls back to
//! travelling as literal text. Slightly longer beats quietly wrong.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Token prefix. Present so a pasted code is recognisable as ours, and so a
/// future format can be told apart from this one.
pub const PREFIX: &str = "kopuz:pl:";

/// Payload format version. Written as a leading byte; v1 was a text line
/// format whose first byte was the ASCII digit `'1'` (0x31), so the two can
/// never be confused and old codes still decode.
const VERSION: u8 = 2;

/// Layout of the record section, chosen once per playlist.
mod mode {
    /// Every record carries a tag byte saying what it is.
    pub const MIXED: u8 = 0;
    /// Every track is a packed YouTube id — no tags, just 8 bytes each.
    /// The common case for a YouTube Music playlist, and the cheapest.
    pub const ALL_YT_PACKED: u8 = 1;
}

/// Record tags, used in [`mode::MIXED`].
mod rec {
    /// 8 raw bytes of a YouTube video id.
    pub const YT_PACKED: u8 = 1;
    /// Length-prefixed SoundCloud id.
    pub const SOUNDCLOUD: u8 = 2;
    /// Length-prefixed title, artist, then a varint duration.
    pub const LOCAL: u8 = 3;
    /// A YouTube id that would not survive packing — length-prefixed text.
    pub const YT_LITERAL: u8 = 4;
}

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

/// Split a portable path into its source and the id worth sending. YouTube
/// keeps only the video id — the trailing segments are derived data the
/// recipient rebuilds anyway.
fn split_source(path: &str) -> Option<(Source, &str)> {
    let (scheme, rest) = path.split_once(':')?;
    let (source, id) = match scheme {
        "ytmusic" => (Source::YouTube, rest.split(':').next()?),
        "soundcloud" => (Source::SoundCloud, rest),
        _ => return None,
    };
    (!id.is_empty()).then_some((source, id))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    YouTube,
    SoundCloud,
}

/// Squeeze an 11-character YouTube id back into the 8 bytes it encodes.
///
/// Returns `None` unless the bytes re-encode to exactly the original id. That
/// check is the whole point: a non-canonical id would decode to a different
/// video, and a shared playlist quietly containing the wrong song is far worse
/// than a slightly longer code.
fn pack_yt_id(id: &str) -> Option<[u8; 8]> {
    let bytes: [u8; 8] = URL_SAFE_NO_PAD.decode(id).ok()?.try_into().ok()?;
    (URL_SAFE_NO_PAD.encode(bytes) == id).then_some(bytes)
}

fn unpack_yt_id(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn take_varint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let (&byte, rest) = input.split_first()?;
        *input = rest;
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

fn take_bytes<'a>(input: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if input.len() < len {
        return None;
    }
    let (taken, rest) = input.split_at(len);
    *input = rest;
    Some(taken)
}

fn take_str(input: &mut &[u8]) -> Option<String> {
    let len = take_varint(input)? as usize;
    let bytes = take_bytes(input, len)?;
    String::from_utf8(bytes.to_vec()).ok()
}

/// How a track will be written, resolved once so `encode` can decide on a mode
/// before committing anything to bytes.
enum Plan<'a> {
    YtPacked([u8; 8]),
    YtLiteral(&'a str),
    SoundCloud(&'a str),
    Local(&'a SharedTrack),
}

fn plan(track: &SharedTrack) -> Plan<'_> {
    match track.path.as_deref().and_then(split_source) {
        Some((Source::YouTube, id)) => match pack_yt_id(id) {
            Some(bytes) => Plan::YtPacked(bytes),
            None => Plan::YtLiteral(id),
        },
        Some((Source::SoundCloud, id)) => Plan::SoundCloud(id),
        None => Plan::Local(track),
    }
}

/// Encode a playlist into one pasteable token.
pub fn encode(playlist: &SharedPlaylist) -> String {
    let plans: Vec<Plan<'_>> = playlist.tracks.iter().map(plan).collect();
    let all_yt = !plans.is_empty() && plans.iter().all(|p| matches!(p, Plan::YtPacked(_)));

    let mut body = vec![VERSION];
    put_str(&mut body, &playlist.name);

    if all_yt {
        // No tags at all — the mode says every record is 8 bytes, and the
        // count follows from the length of what's left.
        body.push(mode::ALL_YT_PACKED);
        for p in &plans {
            if let Plan::YtPacked(bytes) = p {
                body.extend_from_slice(bytes);
            }
        }
    } else {
        body.push(mode::MIXED);
        for p in &plans {
            match p {
                Plan::YtPacked(bytes) => {
                    body.push(rec::YT_PACKED);
                    body.extend_from_slice(bytes);
                }
                Plan::YtLiteral(id) => {
                    body.push(rec::YT_LITERAL);
                    put_str(&mut body, id);
                }
                Plan::SoundCloud(id) => {
                    body.push(rec::SOUNDCLOUD);
                    put_str(&mut body, id);
                }
                Plan::Local(t) => {
                    body.push(rec::LOCAL);
                    put_str(&mut body, &t.title);
                    put_str(&mut body, &t.artist);
                    put_varint(&mut body, t.duration);
                }
            }
        }
    }

    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&body))
}

fn portable(path: String) -> SharedTrack {
    SharedTrack {
        path: Some(path),
        title: String::new(),
        artist: String::new(),
        duration: 0,
    }
}

/// Decode a token produced by [`encode`].
///
/// Tolerant about what surrounds the token — people paste with stray
/// whitespace, quotes or a chat client's zero-width junk attached — but strict
/// about the payload: an unknown version or unreadable base64 is an error, not
/// a half-recovered playlist.
pub fn decode(input: &str) -> Result<SharedPlaylist, String> {
    let trimmed = input
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>');
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

    let playlist = match bytes.first() {
        Some(&VERSION) => decode_v2(&bytes[1..])?,
        // v1 was UTF-8 text beginning with the ASCII digit '1'. Codes from it
        // may already be sitting in someone's chat history.
        Some(b'1') => decode_v1(&bytes)?,
        Some(other) => {
            return Err(format!(
                "This link was made by a different Kopuz version (format {other}) — update and try again."
            ));
        }
        None => return Err("The playlist link is empty.".into()),
    };
    if playlist.tracks.is_empty() {
        return Err("That playlist link contains no tracks.".into());
    }
    Ok(playlist)
}

fn damaged<T>() -> Result<T, String> {
    Err("The playlist link is damaged — it looks truncated.".to_string())
}

fn decode_v2(mut input: &[u8]) -> Result<SharedPlaylist, String> {
    let Some(name) = take_str(&mut input) else {
        return damaged();
    };
    let Some((&layout, rest)) = input.split_first() else {
        return damaged();
    };
    input = rest;

    let mut tracks = Vec::new();
    match layout {
        mode::ALL_YT_PACKED => {
            if !input.len().is_multiple_of(8) {
                return damaged();
            }
            for chunk in input.chunks_exact(8) {
                tracks.push(portable(format!("ytmusic:{}", unpack_yt_id(chunk))));
            }
        }
        mode::MIXED => {
            while let Some((&tag, rest)) = input.split_first() {
                input = rest;
                match tag {
                    rec::YT_PACKED => {
                        let Some(bytes) = take_bytes(&mut input, 8) else {
                            return damaged();
                        };
                        tracks.push(portable(format!("ytmusic:{}", unpack_yt_id(bytes))));
                    }
                    rec::YT_LITERAL => {
                        let Some(id) = take_str(&mut input) else {
                            return damaged();
                        };
                        tracks.push(portable(format!("ytmusic:{id}")));
                    }
                    rec::SOUNDCLOUD => {
                        let Some(id) = take_str(&mut input) else {
                            return damaged();
                        };
                        tracks.push(portable(format!("soundcloud:{id}")));
                    }
                    rec::LOCAL => {
                        let (Some(title), Some(artist), Some(duration)) = (
                            take_str(&mut input),
                            take_str(&mut input),
                            take_varint(&mut input),
                        ) else {
                            return damaged();
                        };
                        if title.is_empty() {
                            continue;
                        }
                        tracks.push(SharedTrack {
                            path: None,
                            title,
                            artist,
                            duration,
                        });
                    }
                    // An unknown record from a newer minor format. Unlike the
                    // old text layout there is no newline to resynchronise on,
                    // so the rest of the payload can't be trusted — stop and
                    // keep what was read rather than emit garbage tracks.
                    _ => break,
                }
            }
        }
        _ => return damaged(),
    }
    Ok(SharedPlaylist { name, tracks })
}

/// The original line-based format, kept so codes shared before the binary
/// payload landed still work.
fn decode_v1(bytes: &[u8]) -> Result<SharedPlaylist, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "The playlist link is damaged.".to_string())?;
    let mut lines = text.split('\n');
    lines.next();
    let name = unescape_v1(lines.next().unwrap_or_default());

    let mut tracks = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (tag, rest) = line.split_at(1);
        match tag {
            "y" => tracks.push(portable(format!("ytmusic:{}", unescape_v1(rest)))),
            "s" => tracks.push(portable(format!("soundcloud:{}", unescape_v1(rest)))),
            "l" => {
                let mut parts = rest.trim_start_matches('|').split('|');
                let title = unescape_v1(parts.next().unwrap_or_default());
                let artist = unescape_v1(parts.next().unwrap_or_default());
                let duration = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
                if title.is_empty() {
                    continue;
                }
                tracks.push(SharedTrack {
                    path: None,
                    title,
                    artist,
                    duration,
                });
            }
            _ => continue,
        }
    }
    Ok(SharedPlaylist { name, tracks })
}

fn unescape_v1(s: &str) -> String {
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
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
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
        assert_eq!(
            decoded.tracks[0].path.as_deref(),
            Some("ytmusic:dQw4w9WgXcQ")
        );
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
        let code = encode(&SharedPlaylist {
            name: "x".into(),
            tracks: vec![t],
        });
        let raw = URL_SAFE_NO_PAD
            .decode(code.trim_start_matches(PREFIX))
            .unwrap();
        let raw = String::from_utf8_lossy(&raw);
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
        // The binary layout is length-prefixed, so there is nothing to escape
        // — but the old text format needed escaping and this is what caught it.
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
    fn non_ascii_titles_survive() {
        let pl = SharedPlaylist {
            name: "Ünïcödé 🎵".into(),
            tracks: vec![SharedTrack {
                path: None,
                title: "Über den Wolken".into(),
                artist: "Reinhard Mey".into(),
                duration: 267,
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
        let body = [VERSION + 1, 0, 0];
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
    fn a_truncated_code_is_an_error_not_a_partial_playlist() {
        let code = encode(&sample());
        let payload = code.trim_start_matches(PREFIX);
        let cut = &payload[..payload.len() * 2 / 3];
        // Whatever this decodes to, it must not silently pass as the playlist.
        if let Ok(pl) = decode(&format!("{PREFIX}{cut}")) {
            assert_ne!(pl, sample(), "a truncated code must not decode as intact");
        }
    }

    #[test]
    fn an_empty_playlist_is_an_error_not_a_silent_import() {
        let code = encode(&SharedPlaylist {
            name: "Empty".into(),
            tracks: vec![],
        });
        assert!(decode(&code).is_err());
    }

    #[test]
    fn a_youtube_id_that_would_not_survive_packing_travels_as_text() {
        // Eleven base64 chars hold 66 bits and eight bytes hold 64, so an id
        // whose last character carries low bits cannot be packed. Packing it
        // anyway would hand the recipient a DIFFERENT video, which is the one
        // outcome worth spending bytes to avoid.
        let lossy = "dQw4w9WgXcR";
        assert!(pack_yt_id(lossy).is_none(), "must refuse to pack {lossy}");
        assert!(pack_yt_id("dQw4w9WgXcQ").is_some(), "canonical id must pack");

        let pl = SharedPlaylist {
            name: "Edge".into(),
            tracks: vec![
                portable(format!("ytmusic:{lossy}")),
                portable("ytmusic:dQw4w9WgXcQ".into()),
            ],
        };
        let decoded = decode(&encode(&pl)).unwrap();
        assert_eq!(
            decoded.tracks[0].path.as_deref(),
            Some(format!("ytmusic:{lossy}").as_str()),
            "the id must come back exactly as it went in",
        );
        assert_eq!(
            decoded.tracks[1].path.as_deref(),
            Some("ytmusic:dQw4w9WgXcQ")
        );
    }

    #[test]
    fn codes_shared_before_the_binary_format_still_work() {
        // Someone may already have a v1 code sitting in a chat window.
        let v1 = "1\nLate night\nydQw4w9WgXcQ\ns9f2a1b\nl|A Rip From 2009|Unknown|0\n";
        let code = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(v1));
        let decoded = decode(&code).expect("v1 must still decode");
        assert_eq!(decoded.name, "Late night");
        assert_eq!(decoded.tracks.len(), 3);
        assert_eq!(
            decoded.tracks[0].path.as_deref(),
            Some("ytmusic:dQw4w9WgXcQ")
        );
        assert_eq!(decoded.tracks[2].title, "A Rip From 2009");
    }

    #[test]
    fn stays_pasteable_for_a_large_playlist() {
        let tracks: Vec<SharedTrack> = (0..300)
            .map(|i| {
                // Real-shaped ids: 11 canonical base64url characters.
                let bytes = [(i >> 8) as u8, i as u8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
                portable(format!("ytmusic:{}", URL_SAFE_NO_PAD.encode(bytes)))
            })
            .collect();
        let code = encode(&SharedPlaylist {
            name: "Big".into(),
            tracks,
        });
        // Guards against a format change that quietly makes sharing unusable.
        assert!(
            code.len() < 3_600,
            "300 tracks produced {} chars — the binary payload has regressed",
            code.len(),
        );
        assert_eq!(decode(&code).unwrap().tracks.len(), 300);
    }

    #[test]
    fn an_all_youtube_playlist_pays_no_per_track_tag() {
        // The mode exists to drop one byte per track; if a future change breaks
        // the all-YouTube detection this is what notices.
        let yt = |i: u8| portable(format!("ytmusic:{}", URL_SAFE_NO_PAD.encode([i; 8])));
        let pure = SharedPlaylist {
            name: "P".into(),
            tracks: (0..50).map(yt).collect(),
        };
        let mut mixed = pure.clone();
        mixed.tracks.push(portable("soundcloud:abc".into()));

        let pure_len = encode(&pure).len();
        let mixed_len = encode(&mixed).len();
        // One extra track plus a tag byte on all 51 records.
        assert!(
            mixed_len > pure_len + 50,
            "mixed mode should cost a tag per record: pure={pure_len} mixed={mixed_len}",
        );
        assert_eq!(decode(&encode(&pure)).unwrap().tracks.len(), 50);
        assert_eq!(decode(&encode(&mixed)).unwrap().tracks.len(), 51);
    }
}
