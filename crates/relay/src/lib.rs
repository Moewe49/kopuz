//! Getting state from one of your devices to another, through a relay you run
//! yourself.
//!
//! # Why a relay at all
//!
//! The desktop analyses audio and builds the mixes; the phone cannot, and is
//! not meant to — the ONNX runtime has no verified aarch64-android build, and
//! a phone should not spend an evening and a gigabyte of mobile data doing
//! work a desktop already did. So the result has to travel. There is no path
//! between two devices behind different NATs that does not pass through
//! something reachable from both.
//!
//! Hosting it yourself is what keeps that honest: no account, no third party
//! holding your listening history, nothing to pay for, nothing to shut down.
//! It is the same bargain the share codes made, one step further along.
//!
//! # What this is not
//!
//! Not a sync engine. There is no merge, no conflict resolution and no
//! history: the desktop is the author of a mix set and the phone is a reader
//! of it. Pretending otherwise would mean building three-way merges for data
//! that is regenerated from scratch every day anyway.
//!
//! # Shape
//!
//! An authenticated key-value store, deliberately dumb:
//!
//! ```text
//! PUT  /v1/state/{key}   Authorization: Bearer <token>   body = bytes
//! GET  /v1/state/{key}   Authorization: Bearer <token>   -> bytes + version
//! ```
//!
//! A version travels with every value so a reader can ask "has this changed"
//! and get an empty answer when it has not — a phone on mobile data should not
//! re-fetch fifty kilobytes to discover it already had them.
//!
//! # Transport security is not this crate's job
//!
//! The token authenticates; it does not encrypt. Anything reachable from the
//! open internet belongs behind a reverse proxy holding a real certificate, or
//! inside a private network such as Tailscale or WireGuard. Rolling TLS in
//! here would mean owning certificate renewal and a decade of protocol
//! decisions for one small feature, and doing it worse than the proxy the
//! listener already runs.

use serde::{Deserialize, Serialize};

/// Where a mix set lives on the relay.
///
/// Named rather than numbered so a second kind of state — a jam session, a
/// listening position — can be added without either side guessing.
pub const KEY_MIXES: &str = "mixes";

/// Longest value the relay accepts.
///
/// A mix set measures about 50 KB. A megabyte leaves room for that to grow by
/// twenty times and still refuses anything that is obviously not this.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// A stored value and the version it was stored at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    /// Increases on every write. A reader that already has this version has
    /// the current value.
    pub version: u64,
    /// Unix seconds of the write, so a device can say how old what it has is.
    pub written_at: u64,
    #[serde(with = "base64_bytes")]
    pub bytes: Vec<u8>,
}

/// What a reader gets back when it says what it already has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Fetched {
    /// The reader is already current; nothing was sent.
    Unchanged,
    Value(Stored),
    /// Nothing has ever been written under this key.
    Missing,
}

/// Base64 rather than a raw byte array, so the wire format stays JSON and can
/// be read with any tool the listener already has when something goes wrong.
mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// How to reach the relay.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelayConfig {
    /// Base URL, e.g. `https://kopuz.example.net` or
    /// `http://ms01.tailnet:8484`. Empty means the feature is off.
    #[serde(default)]
    pub url: String,
    /// Shared secret. Both the relay and every device of yours hold the same
    /// one — there are no accounts, because there is one person.
    #[serde(default)]
    pub token: String,
}

impl RelayConfig {
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.token.trim().is_empty()
    }

    /// Full URL for one key, with the base's trailing slashes tolerated.
    pub fn endpoint(&self, key: &str) -> String {
        format!("{}/v1/state/{key}", self.url.trim_end_matches('/'))
    }
}

/// Anything that can go wrong, in terms a person can act on.
#[derive(Debug, Clone, PartialEq)]
pub enum RelayError {
    NotConfigured,
    /// The token was rejected. Almost always the two sides disagreeing.
    Unauthorised,
    /// Too large for the relay to accept.
    TooLarge {
        bytes: usize,
    },
    /// The jam session is over -- ended, expired, or the code was wrong, which
    /// the relay does not distinguish on purpose. All three mean the same thing
    /// to a listener: this jam is not there any more.
    JamGone,
    Transport(String),
    Protocol(String),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::NotConfigured => write!(f, "no relay is configured"),
            RelayError::Unauthorised => {
                write!(
                    f,
                    "the relay rejected the token — check it matches on both sides"
                )
            }
            RelayError::TooLarge { bytes } => write!(
                f,
                "{bytes} bytes is more than the relay accepts ({MAX_VALUE_BYTES})"
            ),
            RelayError::JamGone => write!(f, "that jam has ended"),
            RelayError::Transport(m) => write!(f, "could not reach the relay: {m}"),
            RelayError::Protocol(m) => write!(f, "the relay answered unexpectedly: {m}"),
        }
    }
}

impl std::error::Error for RelayError {}

pub mod address;
pub use address::{normalise_url, token_travels_in_the_clear};

pub mod jam;
pub mod pairing;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_tolerates_a_trailing_slash() {
        let a = RelayConfig {
            url: "https://example.net".into(),
            token: "t".into(),
        };
        let b = RelayConfig {
            url: "https://example.net/".into(),
            token: "t".into(),
        };
        assert_eq!(a.endpoint(KEY_MIXES), b.endpoint(KEY_MIXES));
        assert_eq!(a.endpoint(KEY_MIXES), "https://example.net/v1/state/mixes");
    }

    /// Half-filled settings are the normal state while someone is typing them
    /// in, and must not read as ready.
    #[test]
    fn a_relay_is_only_configured_when_both_halves_are_there() {
        let full = RelayConfig {
            url: "https://example.net".into(),
            token: "secret".into(),
        };
        assert!(full.is_configured());
        for partial in [
            RelayConfig {
                url: "https://example.net".into(),
                token: String::new(),
            },
            RelayConfig {
                url: String::new(),
                token: "secret".into(),
            },
            RelayConfig {
                url: "   ".into(),
                token: "secret".into(),
            },
            RelayConfig::default(),
        ] {
            assert!(!partial.is_configured(), "{partial:?}");
        }
    }

    /// The wire format is JSON so it can be inspected with ordinary tools when
    /// something goes wrong at three in the morning.
    #[test]
    fn a_value_survives_the_wire_format() {
        let stored = Stored {
            version: 7,
            written_at: 1_700_000_000,
            bytes: b"{\"mixes\":[]}".to_vec(),
        };
        let json = serde_json::to_string(&stored).unwrap();
        assert!(json.contains("\"version\":7"), "{json}");
        assert_eq!(serde_json::from_str::<Stored>(&json).unwrap(), stored);
    }

    /// Binary payloads must survive too — a future value may not be JSON.
    #[test]
    fn arbitrary_bytes_survive_the_wire_format() {
        let stored = Stored {
            version: 1,
            written_at: 0,
            bytes: (0u8..=255).collect(),
        };
        let json = serde_json::to_string(&stored).unwrap();
        assert_eq!(serde_json::from_str::<Stored>(&json).unwrap(), stored);
    }

    #[test]
    fn the_three_fetch_outcomes_are_distinguishable_on_the_wire() {
        for value in [
            Fetched::Unchanged,
            Fetched::Missing,
            Fetched::Value(Stored {
                version: 1,
                written_at: 2,
                bytes: vec![9],
            }),
        ] {
            let json = serde_json::to_string(&value).unwrap();
            assert_eq!(serde_json::from_str::<Fetched>(&json).unwrap(), value);
        }
    }

    /// The messages are what someone sees when it does not work, so they have
    /// to say what to do rather than what happened.
    #[test]
    fn errors_say_what_to_check() {
        assert!(
            RelayError::Unauthorised.to_string().contains("both sides"),
            "an auth failure must point at the mismatch"
        );
        assert!(
            RelayError::TooLarge { bytes: 9_000_000 }
                .to_string()
                .contains("9000000")
        );
    }
}
