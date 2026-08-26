//! Live jam sessions: a shared space two people edit at once.
//!
//! The rest of the relay holds one person's state under one long secret. A jam
//! is the opposite shape — two people, neither of whom should hold the other's
//! token — so it gets its own kind of thing: an ephemeral session, addressed by
//! a short **join code** that grants access to that one session and nothing
//! else. Your colleague enters the code, can move the queue around and hit
//! pause, and cannot see or touch your mixes.
//!
//! The relay stays a dumb broker even here. It holds the session's bytes
//! opaquely and knows nothing of what a jam *is* — the queue, the playhead, who
//! did what all live in the app. What the relay adds is exactly two things a
//! shared document needs and a single-writer key does not:
//!
//! - **A short code instead of the long token**, so it can be said over a call.
//! - **Compare-and-swap**, so two people editing the queue in the same breath do
//!   not silently overwrite each other: a write carries the version it was based
//!   on, and the relay refuses it if the version has moved on. The loser re-reads
//!   and re-applies. That is the whole of the concurrency story, and it is
//!   enough because a queue is small and edits are rare relative to a round trip.

use serde::{Deserialize, Serialize};

/// A freshly minted session: where it lives and the code that opens it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JamSession {
    /// Opaque, unguessable, and never shown to a person. The URL path.
    pub id: String,
    /// Short and sayable — this is what one person gives the other. A
    /// capability for this session alone.
    pub code: String,
}

/// The answer to a compare-and-swap write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JamWrite {
    /// The write landed; here is the version it now sits at.
    Stored { version: u64 },
    /// Someone else wrote first. Here is the version that is now current, so
    /// the caller can re-read, re-apply its change on top, and try again.
    Conflict { current: u64 },
}

/// Longest a jam document may be.
///
/// A jam is a queue of portable tracks plus a little playhead state. A few
/// hundred tracks at ~40 bytes each is tens of kilobytes; a quarter-megabyte
/// leaves generous room and still refuses anything absurd.
pub const MAX_JAM_BYTES: usize = 256 * 1024;

/// How long a session lives with nothing touching it before the relay reaps it.
///
/// Long enough to survive a pause for dinner, short enough that abandoned jams
/// do not accumulate. Every read or write pushes it back out.
pub const SESSION_IDLE_SECS: u64 = 6 * 60 * 60;

/// Characters a join code is drawn from: unambiguous when read aloud or typed.
/// No `0`/`O`, no `1`/`I`/`l` — the confusions that turn "did it work" into a
/// five-minute argument.
pub const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

/// How many characters a join code has. Twelve from a 31-symbol alphabet is
/// about 60 bits — far past guessing within a session's life. It is delivered
/// inside a paste-one-string join code rather than typed, so length is free and
/// spent on margin.
pub const CODE_LEN: usize = 12;

/// Everything needed to reach one jam: which relay, which session, and the code
/// that opens it. The host builds this from [`JamSession`] plus its own relay
/// URL; the guest gets the whole thing at once from a join code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JamAccess {
    pub url: String,
    pub id: String,
    pub code: String,
}

impl JamAccess {
    /// The URL of this jam's session on the relay.
    pub fn endpoint(&self) -> String {
        format!("{}/v1/jam/{}", self.url.trim_end_matches('/'), self.id)
    }
}

/// What a jam join code starts with. Distinct from the one-shot share's
/// `kopuz:jam:` — a live session and a frozen moment are different things and a
/// field must not mistake one for the other.
pub const JOIN_PREFIX: &str = "kopuz:live:";

/// Pack a whole jam access into one pasteable string, so joining is one paste
/// and not three fields typed by hand. Same shape and same non-secret as the
/// data it carries: whoever holds this can drive the jam, so send it the way
/// you would send the jam itself.
pub fn encode_join(access: &JamAccess) -> String {
    use base64::Engine;
    let json = format!(
        "{{\"u\":\"{}\",\"i\":\"{}\",\"c\":\"{}\"}}",
        esc(&crate::normalise_url(&access.url)),
        esc(&access.id),
        esc(&access.code)
    );
    format!(
        "{JOIN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes())
    )
}

/// Read a join code back. `None` for anything that is not one, so a field can
/// try this and fall back to treating the text as something else.
pub fn decode_join(code: &str) -> Option<JamAccess> {
    use base64::Engine;
    let body = code.trim().strip_prefix(JOIN_PREFIX)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.as_bytes())
        .ok()?;
    let json = std::str::from_utf8(&bytes).ok()?;
    let access = JamAccess {
        url: json_field(json, "u")?,
        id: json_field(json, "i")?,
        code: json_field(json, "c")?,
    };
    (!access.url.is_empty() && !access.id.is_empty() && !access.code.is_empty()).then_some(access)
}

/// True when a pasted string is a jam join code.
pub fn looks_like_join(text: &str) -> bool {
    text.trim_start().starts_with(JOIN_PREFIX)
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn json_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let mut out = String::new();
    let mut chars = json[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            other => out.push(other),
        }
    }
    None
}

/// The two ways credentials get generated, so the server can use real
/// randomness and a test can pin exact strings.
///
/// The id must be unguessable (it is not secret, but it should not collide);
/// the code must be unguessable (it *is* the secret). Both come from the same
/// random bytes so there is one source to reason about.
pub fn credentials_from_bytes(id_bytes: &[u8; 16], code_bytes: &[u8; CODE_LEN]) -> JamSession {
    let id = id_bytes.iter().map(|b| format!("{b:02x}")).collect();
    let code = code_bytes
        .iter()
        .map(|b| CODE_ALPHABET[(*b as usize) % CODE_ALPHABET.len()] as char)
        .collect();
    JamSession { id, code }
}

/// Compare two codes without letting the time it takes reveal how much of the
/// code was right — the same reasoning as the main token, because a join code
/// is a secret reachable from the network.
pub fn code_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_short_sayable_and_free_of_lookalikes() {
        let s = credentials_from_bytes(&[0xAB; 16], &[7, 13, 0, 30, 1, 2, 3, 4, 5, 6, 8, 9]);
        assert_eq!(s.code.len(), CODE_LEN);
        assert_eq!(s.id.len(), 32); // 16 bytes as hex
        // None of the characters people misread.
        for bad in ['0', 'O', '1', 'I', 'l'] {
            assert!(!s.code.contains(bad), "code contains a lookalike: {bad}");
        }
    }

    #[test]
    fn the_same_bytes_make_the_same_credentials() {
        let a = credentials_from_bytes(&[1; 16], &[5; CODE_LEN]);
        let b = credentials_from_bytes(&[1; 16], &[5; CODE_LEN]);
        assert_eq!(a, b);
        let c = credentials_from_bytes(&[2; 16], &[5; CODE_LEN]);
        assert_ne!(a.id, c.id);
    }

    #[test]
    fn code_comparison_does_not_short_circuit() {
        assert!(code_matches("ABCDEFGH", "ABCDEFGH"));
        assert!(!code_matches("ABCDEFGH", "ABCDEFGX"));
        assert!(!code_matches("ABCDEFGH", "ABC"));
        assert!(!code_matches("", "X"));
    }

    #[test]
    fn a_join_code_round_trips_the_whole_access() {
        let access = JamAccess {
            url: "https://kopuz.example.net".to_string(),
            id: "0123456789abcdef0123456789abcdef".to_string(),
            code: "ABCDEFGHJKMN".to_string(),
        };
        let code = encode_join(&access);
        assert!(code.starts_with(JOIN_PREFIX));
        assert!(looks_like_join(&code));
        assert_eq!(decode_join(&code), Some(access));
    }

    #[test]
    fn a_join_code_fills_in_a_scheme_less_relay_url() {
        let code = encode_join(&JamAccess {
            url: "ms-01:8484".to_string(),
            id: "deadbeef".to_string(),
            code: "ABCDEFGHJKMN".to_string(),
        });
        assert_eq!(decode_join(&code).unwrap().url, "http://ms-01:8484");
    }

    #[test]
    fn a_live_join_is_not_confused_with_a_one_shot_share_or_junk() {
        // The one-shot share code prefix must not read as a live join.
        assert!(!looks_like_join("kopuz:jam:whatever"));
        assert!(decode_join("kopuz:jam:whatever").is_none());
        assert!(decode_join("").is_none());
        assert!(decode_join("kopuz:live:not-base64!!").is_none());
        assert!(decode_join("kopuz:live:").is_none());
    }

    #[test]
    fn an_endpoint_tolerates_a_trailing_slash_on_the_relay_url() {
        let access = JamAccess {
            url: "https://kopuz.example.net/".to_string(),
            id: "abc123".to_string(),
            code: "ABCDEFGHJKMN".to_string(),
        };
        assert_eq!(access.endpoint(), "https://kopuz.example.net/v1/jam/abc123");
    }
}
