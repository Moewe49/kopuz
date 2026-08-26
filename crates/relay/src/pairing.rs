//! One code that carries both halves, so nobody types a 43-character secret
//! into a phone.
//!
//! The address and the token together are a `kopuz:relay:<base64url>` string —
//! the same shape as the playlist share codes, for the same reason: it is one
//! thing you can send yourself by any means to hand and paste into a single
//! field. The desktop makes it; either device reads it.
//!
//! This is not encryption. The code carries the token in the clear, exactly as
//! typing it would, so it is as sensitive as the token itself: sent over a
//! channel only you can read, never posted anywhere. It saves the typing and
//! the transcription errors, nothing more.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::RelayConfig;

/// What a pairing code starts with, so a field can tell one from a plain URL
/// the moment it is pasted.
pub const PREFIX: &str = "kopuz:relay:";

/// Pack an address and token into a single pasteable code.
///
/// The URL is normalised first, so the code a desktop hands out already
/// carries the scheme and cannot arrive as the bare `ms-01:8484` that fails to
/// parse later.
pub fn encode(config: &RelayConfig) -> String {
    // A tiny hand-rolled JSON object. Pulling serde_json in for two string
    // fields would be the tail wagging the dog, and escaping is trivial: a URL
    // has no quotes or backslashes, and a token is base64 or hex.
    let url = crate::normalise_url(&config.url);
    let json = format!(
        "{{\"u\":\"{}\",\"t\":\"{}\"}}",
        escape(&url),
        escape(config.token.trim())
    );
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(json.as_bytes()))
}

/// Read a pairing code back into an address and token.
///
/// Returns `None` for anything that is not a well-formed code — which is the
/// common case, because a field that accepts these also accepts a plain URL,
/// and it tells the two apart by trying this and falling back.
pub fn decode(code: &str) -> Option<RelayConfig> {
    let body = code.trim().strip_prefix(PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(body.as_bytes()).ok()?;
    let json = std::str::from_utf8(&bytes).ok()?;
    let url = field(json, "u")?;
    let token = field(json, "t")?;
    if url.is_empty() || token.is_empty() {
        return None;
    }
    Some(RelayConfig { url, token })
}

/// True when a pasted string is a pairing code rather than a plain address, so
/// the caller knows to decode it instead of treating it as a URL.
pub fn looks_like_code(text: &str) -> bool {
    text.trim_start().starts_with(PREFIX)
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Pull one string field out of the tiny JSON object. Deliberately small: it
/// reads exactly what [`encode`] writes and nothing more elaborate.
fn field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            other => out.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_round_trips_a_config() {
        let config = RelayConfig {
            url: "http://192.168.0.220:8484".to_string(),
            token: "aVeryLong-token_43charsWithUrlSafeChars012345".to_string(),
        };
        let code = encode(&config);
        assert!(code.starts_with(PREFIX));
        assert!(looks_like_code(&code));
        let back = decode(&code).expect("decodes");
        assert_eq!(back.url, config.url);
        assert_eq!(back.token, config.token);
    }

    /// A code made from the address the placeholder shows must come back with
    /// the scheme filled in, not as the bare host that fails to parse.
    #[test]
    fn the_code_normalises_a_scheme_less_address() {
        let code = encode(&RelayConfig {
            url: "ms-01:8484".to_string(),
            token: "a-token-of-respectable-length".to_string(),
        });
        assert_eq!(decode(&code).unwrap().url, "http://ms-01:8484");
    }

    #[test]
    fn a_plain_url_is_not_mistaken_for_a_code() {
        assert!(!looks_like_code("http://ms-01:8484"));
        assert!(!looks_like_code("192.168.0.220:8484"));
        assert!(decode("http://ms-01:8484").is_none());
        assert!(decode("").is_none());
        assert!(decode("kopuz:relay:not-valid-base64!!").is_none());
        // Right prefix, but the body decodes to nothing useful.
        assert!(decode("kopuz:relay:").is_none());
    }

    /// Leading and trailing whitespace is what a copy-paste leaves behind, and
    /// must not stop a code from being recognised or read.
    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let code = encode(&RelayConfig {
            url: "http://ms-01:8484".into(),
            token: "a-token-of-respectable-length".into(),
        });
        let padded = format!("  \n {code}\t ");
        assert!(looks_like_code(&padded));
        assert_eq!(decode(&padded).unwrap().url, "http://ms-01:8484");
    }

    /// A token with the characters that would break the hand-rolled JSON must
    /// still survive, even though real tokens do not contain them -- a check
    /// that costs nothing and means the format is not a trap for a later change.
    #[test]
    fn awkward_characters_survive() {
        let config = RelayConfig {
            url: "http://ms-01:8484".into(),
            token: r#"has"a quote and \ a backslash"#.into(),
        };
        let back = decode(&encode(&config)).unwrap();
        assert_eq!(back.token, config.token);
    }
}
