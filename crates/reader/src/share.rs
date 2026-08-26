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
///
/// The one-shot share encodes these with its own tight codec below; the live
/// jam ([`crate::jamlive`]) sends whole documents as JSON, which is why serde
/// rides along too. Two formats for two jobs — the share code pays a byte tax
/// it cannot afford, the jam document values legibility over size.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(encode_body(playlist)))
}

/// The payload without its prefix or base64 wrapper, so a jam code can carry
/// the same bytes behind its own header instead of duplicating the format.
fn encode_body(playlist: &SharedPlaylist) -> Vec<u8> {
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

    body
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
    let bytes = unwrap_token(
        input,
        PREFIX,
        "That doesn't look like a Kopuz playlist link.",
    )?;
    decode_body(&bytes)
}

/// Pull the base64 payload out of a pasted token.
///
/// Tolerant about what surrounds it — people paste with stray whitespace,
/// quotes, or a chat client's zero-width junk attached — and strict about
/// what is inside.
fn unwrap_token(input: &str, prefix: &str, not_ours: &str) -> Result<Vec<u8>, String> {
    let trimmed = input
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>');
    let payload = trimmed
        .find(prefix)
        .map(|i| &trimmed[i + prefix.len()..])
        .ok_or_else(|| not_ours.to_string())?;
    let payload: String = payload
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if payload.is_empty() {
        return Err("The link is empty.".into());
    }
    URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .map_err(|_| "The link is damaged — it looks truncated.".to_string())
}

fn decode_body(bytes: &[u8]) -> Result<SharedPlaylist, String> {
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

// ── Jam ───────────────────────────────────────────────────────────────────
//
// A jam is a share code that also says *where the sender is*. The receiver
// imports the queue and jumps to the same moment, so two people end up
// listening to the same thing at the same time.
//
// It is deliberately one-shot rather than a live session. A live jam needs a
// rendezvous server somebody pays for, a clock-offset protocol between the two
// machines, and drift correction on three different playback engines — and the
// only correction primitive here is `seek`, which is audible. This carries a
// real share of the same feeling for none of that, and it keeps the property
// the playlist code was built around: no server, no account, nothing to expire.
//
// What it cannot do: the sender skipping a track will not move the receiver.
// That is the honest limit of a code you paste.

/// Token prefix for a jam. Distinct from [`PREFIX`], so pasting one where the
/// other is expected fails with a clear message instead of a corrupt playlist.
pub const JAM_PREFIX: &str = "kopuz:jam:";

/// Jam payload version, independent of the playlist body version that follows
/// it — the two can move separately.
const JAM_VERSION: u8 = 1;

/// A moment in someone else's listening, packaged to be pasted.
#[derive(Debug, Clone, PartialEq)]
pub struct Jam {
    pub playlist: SharedPlaylist,
    /// Index into `playlist.tracks` that was playing when the code was made.
    pub index: usize,
    /// Position within that track, in milliseconds.
    pub position_ms: u64,
    /// Unix seconds at which the code was made. Without this the receiver
    /// lands wherever the sender *was*, which is behind by however long the
    /// code spent in a chat window.
    pub sent_at: u64,
}

pub fn encode_jam(jam: &Jam) -> String {
    let mut body = vec![JAM_VERSION];
    put_varint(&mut body, jam.index as u64);
    put_varint(&mut body, jam.position_ms);
    put_varint(&mut body, jam.sent_at);

    // Durations, which the playlist body deliberately does not carry: a
    // YouTube track travels as an id alone, because the recipient re-resolves
    // its title and length anyway. A jam cannot wait for that. Without the
    // lengths [`catch_up`] has no way to know when a track ended, so it stops
    // at the first one and the receiver lands wherever the sender was rather
    // than where they are — which is the entire point of a jam.
    //
    // A varint each, so a thirty-track jam pays about sixty bytes for it.
    put_varint(&mut body, jam.playlist.tracks.len() as u64);
    for track in &jam.playlist.tracks {
        put_varint(&mut body, track.duration);
    }

    body.extend_from_slice(&encode_body(&jam.playlist));
    format!("{JAM_PREFIX}{}", URL_SAFE_NO_PAD.encode(body))
}

pub fn decode_jam(input: &str) -> Result<Jam, String> {
    let bytes = unwrap_token(
        input,
        JAM_PREFIX,
        "That doesn't look like a Kopuz jam link.",
    )?;
    let mut rest: &[u8] = match bytes.split_first() {
        Some((&JAM_VERSION, rest)) => rest,
        Some((other, _)) => {
            return Err(format!(
                "This jam link was made by a different Kopuz version (format {other}) - update and try again."
            ));
        }
        None => return Err("The jam link is empty.".into()),
    };
    let (Some(index), Some(position_ms), Some(sent_at)) = (
        take_varint(&mut rest),
        take_varint(&mut rest),
        take_varint(&mut rest),
    ) else {
        return damaged();
    };
    let Some(count) = take_varint(&mut rest) else {
        return damaged();
    };
    let mut durations = Vec::with_capacity(count.min(4096) as usize);
    for _ in 0..count {
        let Some(d) = take_varint(&mut rest) else {
            return damaged();
        };
        durations.push(d);
    }

    let mut playlist = decode_body(rest)?;
    // Put the lengths back on the tracks that travelled as bare ids.
    for (track, duration) in playlist.tracks.iter_mut().zip(durations) {
        if track.duration == 0 {
            track.duration = duration;
        }
    }
    // An index past the end would seek into nothing. Clamp rather than reject:
    // the queue is still worth having.
    let index = (index as usize).min(playlist.tracks.len().saturating_sub(1));
    Ok(Jam {
        playlist,
        index,
        position_ms,
        sent_at,
    })
}

/// Where the sender is *now*, given when the code was made.
///
/// A code sits in a chat window for a while before anyone pastes it. Landing
/// at the position it recorded would put the receiver behind by exactly that
/// delay, which for a three-minute-old code means starting a track the sender
/// already finished. So the elapsed time is played forward through the queue.
///
/// A track whose duration is unknown (0) stops the walk: without a length
/// there is no way to know when it ended, and guessing would skip music the
/// receiver should hear.
pub fn catch_up(jam: &Jam, now_secs: u64) -> (usize, u64) {
    let tracks = &jam.playlist.tracks;
    if tracks.is_empty() {
        return (0, 0);
    }
    let mut index = jam.index.min(tracks.len() - 1);
    let mut position = jam
        .position_ms
        .saturating_add(now_secs.saturating_sub(jam.sent_at).saturating_mul(1000));

    while index < tracks.len() {
        let duration_ms = tracks[index].duration.saturating_mul(1000);
        if duration_ms == 0 || position < duration_ms {
            break;
        }
        if index + 1 == tracks.len() {
            // They have run out of queue; sit at the end rather than seeking
            // past it.
            position = duration_ms;
            break;
        }
        position -= duration_ms;
        index += 1;
    }
    (index, position)
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
        assert!(
            pack_yt_id("dQw4w9WgXcQ").is_some(),
            "canonical id must pack"
        );

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

    // ── Jam ───────────────────────────────────────────────────────────────

    fn jam_track(id: &str, secs: u64) -> SharedTrack {
        SharedTrack {
            path: Some(format!("ytmusic:{id}")),
            title: format!("track {id}"),
            artist: "Someone".into(),
            duration: secs,
        }
    }

    fn a_jam() -> Jam {
        Jam {
            playlist: SharedPlaylist {
                name: "Jam".into(),
                tracks: vec![
                    jam_track("aaaaaaaaaaa", 200),
                    jam_track("bbbbbbbbbbb", 180),
                    jam_track("ccccccccccc", 240),
                ],
            },
            index: 1,
            position_ms: 45_000,
            sent_at: 1_700_000_000,
        }
    }

    #[test]
    fn a_jam_survives_the_round_trip() {
        let jam = a_jam();
        let decoded = decode_jam(&encode_jam(&jam)).unwrap();
        assert_eq!(decoded.index, jam.index);
        assert_eq!(decoded.position_ms, jam.position_ms);
        assert_eq!(decoded.sent_at, jam.sent_at);
        assert_eq!(decoded.playlist.tracks.len(), 3);
        assert_eq!(decoded.playlist.name, "Jam");
    }

    /// The two token kinds must not be interchangeable: a playlist pasted into
    /// the jam box would otherwise decode into a jam starting at zero, and a
    /// jam pasted into the playlist box would fail on the version byte with a
    /// misleading message.
    #[test]
    fn a_playlist_code_is_not_a_jam_code() {
        let jam = a_jam();
        assert!(decode(&encode_jam(&jam)).is_err());
        assert!(decode_jam(&encode(&jam.playlist)).is_err());
    }

    /// The whole point of the timestamp: pasting late must not start late.
    #[test]
    fn catching_up_advances_within_the_current_track() {
        let jam = a_jam();
        // 30 seconds later, still inside the 180-second track.
        let (index, position) = catch_up(&jam, jam.sent_at + 30);
        assert_eq!(index, 1);
        assert_eq!(position, 75_000);
    }

    #[test]
    fn catching_up_rolls_into_the_next_track() {
        let jam = a_jam();
        // Track 1 has 135 s left; 150 s later they are 15 s into track 2.
        let (index, position) = catch_up(&jam, jam.sent_at + 150);
        assert_eq!(index, 2);
        assert_eq!(position, 15_000);
    }

    #[test]
    fn catching_up_can_cross_several_tracks() {
        let mut jam = a_jam();
        jam.index = 0;
        jam.position_ms = 0;
        // 200 + 180 = 380 s consumed, so 400 s later is 20 s into track 2.
        let (index, position) = catch_up(&jam, jam.sent_at + 400);
        assert_eq!(index, 2);
        assert_eq!(position, 20_000);
    }

    /// A code found the next morning must not seek past the end of the queue.
    #[test]
    fn catching_up_stops_at_the_end_of_the_queue() {
        let jam = a_jam();
        let (index, position) = catch_up(&jam, jam.sent_at + 86_400);
        assert_eq!(index, 2);
        assert_eq!(position, 240_000, "must sit at the end, not beyond it");
    }

    /// An unknown duration cannot be walked past — and must not spin.
    #[test]
    fn an_unknown_duration_stops_the_walk_rather_than_looping() {
        let jam = Jam {
            playlist: SharedPlaylist {
                name: "x".into(),
                tracks: vec![jam_track("aaaaaaaaaaa", 0), jam_track("bbbbbbbbbbb", 100)],
            },
            index: 0,
            position_ms: 0,
            sent_at: 1_000,
        };
        let (index, position) = catch_up(&jam, 1_000 + 9_999);
        assert_eq!(index, 0);
        assert_eq!(position, 9_999_000);
    }

    /// A clock that disagrees, or a code from the future, must not underflow.
    #[test]
    fn a_code_from_the_future_does_not_underflow() {
        let jam = a_jam();
        let (index, position) = catch_up(&jam, jam.sent_at - 500);
        assert_eq!(index, 1);
        assert_eq!(position, 45_000, "no time has passed as far as we can tell");
    }

    /// An index past the end is clamped rather than rejected — the queue is
    /// still worth having.
    #[test]
    fn an_out_of_range_index_is_clamped_not_rejected() {
        let mut jam = a_jam();
        jam.index = 99;
        let decoded = decode_jam(&encode_jam(&jam)).unwrap();
        assert_eq!(decoded.index, 2);
    }

    /// The whole path, as the app walks it: a queue is turned into a code, the
    /// code sits somewhere for a while, and the receiver lands on the right
    /// track at the right second. Each piece has its own test; this is the one
    /// that would catch them being wired together wrongly.
    #[test]
    fn a_moment_survives_the_whole_journey() {
        // Sender: four songs, two minutes into the second.
        let queue = SharedPlaylist {
            name: "Wisp - Sword".into(),
            tracks: vec![
                jam_track("aaaaaaaaaaa", 180),
                jam_track("bbbbbbbbbbb", 200),
                jam_track("ccccccccccc", 240),
                jam_track("ddddddddddd", 150),
            ],
        };
        let sent_at = 1_700_000_000;
        let code = encode_jam(&Jam {
            playlist: queue,
            index: 1,
            position_ms: 120_000,
            sent_at,
        });

        // It travels as text and picks up the usual chat-window debris.
        let pasted = format!("  \"{code}\"  ");
        let received = decode_jam(&pasted).expect("a quoted code must still decode");

        // Receiver opens it five minutes later. Track 1 had 80s left, track 2
        // takes 240s, so they are 300 - 80 = 220s into track 2.
        let (index, position_ms) = catch_up(&received, sent_at + 300);
        assert_eq!(index, 2, "landed on the wrong track");
        assert_eq!(position_ms, 220_000);

        // And the queue itself arrived intact.
        assert_eq!(received.playlist.tracks.len(), 4);
        assert!(received.playlist.tracks.iter().all(|t| t.is_portable()));
        assert_eq!(received.playlist.name, "Wisp - Sword");
    }

    /// Codes are pasted into chat, so length is the design constraint the
    /// whole format was built around. The durations must not undo that.
    #[test]
    fn a_thirty_track_jam_still_fits_in_a_chat_message() {
        let tracks: Vec<SharedTrack> = (0..30)
            .map(|i| jam_track(&format!("aaaaaaaaa{i:02}"), 180 + i as u64))
            .collect();
        let code = encode_jam(&Jam {
            playlist: SharedPlaylist {
                name: "A Mix".into(),
                tracks,
            },
            index: 3,
            position_ms: 45_000,
            sent_at: 1_700_000_000,
        });
        assert!(code.len() < 2000, "{} characters", code.len());
        // And it round-trips at that size.
        assert_eq!(decode_jam(&code).unwrap().playlist.tracks.len(), 30);
    }

    #[test]
    fn a_damaged_jam_link_is_reported() {
        assert!(decode_jam("kopuz:jam:").is_err());
        assert!(decode_jam("hello").is_err());
        assert!(decode_jam("kopuz:jam:!!!!").is_err());
    }
}
