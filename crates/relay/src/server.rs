//! Being a relay.
//!
//! Separated from the binary so an integration test can start one in-process
//! and drive it with the real client. The two halves agreeing with themselves
//! proves nothing; the thing worth testing is that they agree with each other.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::{Fetched, MAX_VALUE_BYTES, Stored};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

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
        let saved = persist(&relay.data_path, &values);
        (stored, saved)
    };
    let (stored, saved) = stored;
    eprintln!(
        "[relay] {key} <- {} bytes, now version {}{}",
        stored.bytes.len(),
        stored.version,
        if saved {
            ""
        } else {
            " (NOT PERSISTED — see warning above)"
        }
    );
    Json(stored).into_response()
}

/// Written through a temporary file and renamed, so a crash mid-write leaves
/// the previous state rather than a truncated file that fails to parse on the
/// next start.
///
/// Returns whether it stuck. A silent failure here is the nastiest kind: the
/// value is already in memory and served happily, the write answers 200, and
/// only a restart reveals that nothing was ever saved -- by which point the
/// data is gone and there is no line anywhere saying why. So the caller logs
/// what this returns.
#[must_use]
fn persist(path: &std::path::Path, values: &HashMap<String, Stored>) -> bool {
    let Ok(json) = serde_json::to_vec(values) else {
        return false;
    };
    let tmp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &json) {
        eprintln!(
            "[relay] WARNING: could not write state to {}: {e}",
            tmp.display()
        );
        return false;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        eprintln!("[relay] WARNING: could not replace {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

fn load(path: &std::path::Path) -> HashMap<String, Stored> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The relay, ready to be served.
///
/// `token` is the shared secret and `data_path` is where values are kept
/// across restarts. Both are validated by the caller -- a library should not
/// exit the process over a short string.
pub fn router(token: impl Into<String>, data_path: impl Into<std::path::PathBuf>) -> Router {
    let data_path = data_path.into();
    let values = load(&data_path);
    let relay = Relay {
        token: Arc::new(token.into()),
        data_path: Arc::new(data_path),
        values: Arc::new(Mutex::new(values)),
    };
    Router::new()
        .route("/v1/state/{key}", get(read).put(write))
        // Liveness, unauthenticated on purpose: it says nothing about the data
        // and lets a proxy or a container runtime check the process without
        // holding the secret.
        .route("/healthz", get(|| async { "ok" }))
        .with_state(relay)
}

/// How many values are already stored, for a line on startup.
pub fn stored_count(data_path: &std::path::Path) -> usize {
    load(data_path).len()
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
