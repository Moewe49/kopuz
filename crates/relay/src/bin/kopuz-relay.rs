//! A relay you run yourself, so your devices can hand things to each other.
//!
//! ```text
//! KOPUZ_RELAY_TOKEN=<a long random string> kopuz-relay
//! ```
//!
//! | variable | default | |
//! |---|---|---|
//! | `KOPUZ_RELAY_TOKEN` | — | required; the same secret every device holds |
//! | `KOPUZ_RELAY_BIND` | `0.0.0.0:8484` | |
//! | `KOPUZ_RELAY_DATA` | `./kopuz-relay-state.json` | where values survive a restart |
//!
//! # Put it behind something that speaks TLS
//!
//! The token authenticates, it does not encrypt. On the open internet this
//! belongs behind a reverse proxy holding a real certificate, or inside a
//! private network such as Tailscale or WireGuard. Implementing TLS here would
//! mean owning certificate renewal for one small feature, and doing it worse
//! than the proxy that is already running.
//!
//! # What it deliberately is not
//!
//! No accounts, no users, no history, no merge. One person, one shared secret,
//! last write wins. The data it holds is regenerated from scratch every day by
//! the device that authored it, so durability beyond "survives a restart"
//! would be complexity bought for nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use relay::{Fetched, MAX_VALUE_BYTES, Stored};

#[derive(Clone)]
struct Relay {
    token: Arc<String>,
    data_path: Arc<std::path::PathBuf>,
    values: Arc<Mutex<HashMap<String, Stored>>>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Compare without giving away where the difference is.
///
/// A naive `==` returns as soon as two bytes differ, and the time that takes
/// leaks the length of the matching prefix. That is a real attack on a secret
/// reachable from the internet, and avoiding it costs four lines.
fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorised(relay: &Relay, headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|given| token_matches(&relay.token, given.trim()))
}

#[derive(serde::Deserialize)]
struct Have {
    /// Version the caller already holds. Absent means none.
    #[serde(default)]
    have: u64,
}

async fn read(
    State(relay): State<Relay>,
    Path(key): Path<String>,
    Query(q): Query<Have>,
    headers: HeaderMap,
) -> Response {
    if !authorised(&relay, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let values = relay.values.lock().unwrap_or_else(|e| e.into_inner());
    match values.get(&key) {
        None => (StatusCode::NOT_FOUND, Json(Fetched::Missing)).into_response(),
        // The caller is current: say so in a few hundred bytes rather than
        // sending back what it already has. A phone on mobile data notices.
        Some(stored) if stored.version == q.have => Json(Fetched::Unchanged).into_response(),
        Some(stored) => Json(Fetched::Value(stored.clone())).into_response(),
    }
}

async fn write(
    State(relay): State<Relay>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorised(&relay, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body.len() > MAX_VALUE_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }

    let stored = {
        let mut values = relay.values.lock().unwrap_or_else(|e| e.into_inner());
        let version = values.get(&key).map(|s| s.version).unwrap_or(0) + 1;
        let stored = Stored {
            version,
            written_at: now_secs(),
            bytes: body.to_vec(),
        };
        values.insert(key.clone(), stored.clone());
        persist(&relay.data_path, &values);
        stored
    };
    eprintln!(
        "[relay] {key} <- {} bytes, now version {}",
        stored.bytes.len(),
        stored.version
    );
    Json(stored).into_response()
}

/// Written through a temporary file and renamed, so a crash mid-write leaves
/// the previous state rather than a truncated file that fails to parse on the
/// next start.
fn persist(path: &std::path::Path, values: &HashMap<String, Stored>) {
    let Ok(json) = serde_json::to_vec(values) else {
        return;
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn load(path: &std::path::Path) -> HashMap<String, Stored> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

#[tokio::main]
async fn main() {
    let Ok(token) = std::env::var("KOPUZ_RELAY_TOKEN") else {
        eprintln!(
            "KOPUZ_RELAY_TOKEN is not set.\n\
             \n\
             Pick a long random string, give it to this relay and to every one\n\
             of your devices. There are no accounts here; that string is the\n\
             whole of the security."
        );
        std::process::exit(2);
    };
    // A short secret is worse than an obviously missing one, because it looks
    // like it is working.
    if token.trim().len() < 16 {
        eprintln!("KOPUZ_RELAY_TOKEN is too short — use at least 16 characters.");
        std::process::exit(2);
    }

    let bind = std::env::var("KOPUZ_RELAY_BIND").unwrap_or_else(|_| "0.0.0.0:8484".to_string());
    let data_path = std::path::PathBuf::from(
        std::env::var("KOPUZ_RELAY_DATA").unwrap_or_else(|_| "kopuz-relay-state.json".to_string()),
    );

    let values = load(&data_path);
    eprintln!(
        "[relay] {} value(s) restored from {}",
        values.len(),
        data_path.display()
    );

    let relay = Relay {
        token: Arc::new(token.trim().to_string()),
        data_path: Arc::new(data_path),
        values: Arc::new(Mutex::new(values)),
    };

    let app = Router::new()
        .route("/v1/state/{key}", get(read).put(write))
        // Liveness, unauthenticated on purpose: it says nothing about the data
        // and lets a proxy or a container runtime check the process without
        // holding the secret.
        .route("/healthz", get(|| async { "ok" }))
        .with_state(relay);

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[relay] cannot bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[relay] listening on {bind}");
    eprintln!("[relay] put this behind TLS before exposing it to the internet");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("[relay] stopped: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The comparison must not finish early on the first differing byte —
    /// that timing is what tells an attacker how much of the secret they have
    /// right.
    #[test]
    fn token_comparison_does_not_short_circuit() {
        assert!(token_matches(
            "correct horse battery",
            "correct horse battery"
        ));
        assert!(!token_matches(
            "correct horse battery",
            "correct horse batterX"
        ));
        assert!(!token_matches("correct horse battery", "correct"));
        assert!(!token_matches("", "x"));
        assert!(token_matches("", ""));
    }

    #[test]
    fn a_bearer_header_is_required_and_must_match() {
        let relay = Relay {
            token: Arc::new("a-long-enough-secret".to_string()),
            data_path: Arc::new(std::path::PathBuf::from("/tmp/unused")),
            values: Arc::new(Mutex::new(HashMap::new())),
        };
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(axum::http::header::AUTHORIZATION, v.parse().unwrap());
            h
        };
        assert!(authorised(&relay, &with("Bearer a-long-enough-secret")));
        // Tolerates the whitespace a copy-paste leaves behind.
        assert!(authorised(&relay, &with("Bearer a-long-enough-secret ")));
        assert!(!authorised(&relay, &with("Bearer wrong")));
        assert!(!authorised(&relay, &with("a-long-enough-secret")));
        assert!(!authorised(&relay, &HeaderMap::new()));
    }

    /// A restart must not lose what a device published, and a half-written
    /// file must never be read back as the truth.
    #[test]
    fn values_survive_a_restart() {
        let dir = std::env::temp_dir().join("kopuz-relay-persist-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state.json");
        let _ = std::fs::remove_file(&path);

        let mut values = HashMap::new();
        values.insert(
            "mixes".to_string(),
            Stored {
                version: 3,
                written_at: 1_700_000_000,
                bytes: b"payload".to_vec(),
            },
        );
        persist(&path, &values);
        assert_eq!(load(&path), values);
        // No temporary left behind to be mistaken for the real thing.
        assert!(!path.with_extension("tmp").exists());

        // An unreadable file starts empty rather than refusing to start.
        std::fs::write(&path, b"not json").unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
