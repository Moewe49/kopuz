//! The client and the server, over a real socket.
//!
//! Both halves are already covered by unit tests, and that proves less than it
//! sounds: each one agreeing with itself says nothing about the two agreeing
//! with each other. The last time this project trusted per-side unit tests
//! over an end-to-end one, ten of them passed while track durations were being
//! dropped in transit — because every one of those tests built the struct by
//! hand instead of sending it anywhere.
//!
//! Needs both features:
//! `cargo test -p relay --features "client server" --test roundtrip`

#![cfg(all(feature = "client", feature = "server"))]

use relay::{Fetched, RelayConfig, RelayError};

const TOKEN: &str = "a-token-of-respectable-length";

/// Start a relay on a port the operating system picks, and return its URL.
///
/// Port 0 rather than a hard-coded one: two tests running at once must not
/// fight over a number, and a developer's own relay may well already hold
/// 8484.
async fn start(dir: &std::path::Path) -> String {
    let app = relay::server::router(TOKEN, dir.join("state.json"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kopuz-relay-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn a_value_survives_the_trip_and_the_version_answers_the_next_one() {
    let dir = scratch("roundtrip");
    let config = RelayConfig {
        url: start(&dir).await,
        token: TOKEN.to_string(),
    };

    // Nothing published yet.
    assert_eq!(
        relay::client::get(&config, relay::KEY_MIXES, 0)
            .await
            .unwrap(),
        Fetched::Missing
    );

    // Something roughly the shape and size of a real mix set, including bytes
    // outside ASCII — artist names are full of them.
    let payload = format!(
        r#"{{"mixes":[],"note":"Sigur Rós — Ágætis byrjun","pad":"{}"}}"#,
        "x".repeat(50_000)
    );
    let stored = relay::client::put(&config, relay::KEY_MIXES, payload.as_bytes())
        .await
        .unwrap();
    assert_eq!(stored.version, 1);
    assert_eq!(stored.bytes, payload.as_bytes());

    // A reader with nothing gets the whole thing back, byte for byte.
    match relay::client::get(&config, relay::KEY_MIXES, 0)
        .await
        .unwrap()
    {
        Fetched::Value(got) => {
            assert_eq!(got.version, 1);
            assert_eq!(got.bytes, payload.as_bytes());
        }
        other => panic!("expected the value, got {other:?}"),
    }

    // A reader that is current is told so, and is not sent 50 KB to find out.
    assert_eq!(
        relay::client::get(&config, relay::KEY_MIXES, 1)
            .await
            .unwrap(),
        Fetched::Unchanged
    );

    // A second write moves the version on, and the reader is no longer current.
    let second = relay::client::put(&config, relay::KEY_MIXES, b"newer")
        .await
        .unwrap();
    assert_eq!(second.version, 2);
    assert!(matches!(
        relay::client::get(&config, relay::KEY_MIXES, 1).await.unwrap(),
        Fetched::Value(v) if v.bytes == b"newer"
    ));
}

/// The wrong token must fail as *the wrong token*, not as some generic
/// transport noise — that message is the only thing standing between the
/// listener and an afternoon of blaming their firewall.
#[tokio::test]
async fn a_mismatched_token_says_so() {
    let dir = scratch("auth");
    let url = start(&dir).await;
    let wrong = RelayConfig {
        url: url.clone(),
        token: "not-the-right-secret".to_string(),
    };
    assert_eq!(
        relay::client::get(&wrong, relay::KEY_MIXES, 0).await,
        Err(RelayError::Unauthorised)
    );
    assert_eq!(
        relay::client::put(&wrong, relay::KEY_MIXES, b"x").await,
        Err(RelayError::Unauthorised)
    );
    assert_eq!(
        relay::client::check(&wrong).await,
        Err(RelayError::Unauthorised)
    );

    // And the right one passes the same check, on a relay holding nothing —
    // which is exactly the state someone is in when they first press Test.
    let right = RelayConfig {
        url,
        token: TOKEN.to_string(),
    };
    assert_eq!(relay::client::check(&right).await, Ok(()));
}

/// The cap is enforced by the relay, not only by the caller — a device running
/// an older build must not be able to fill the disk.
#[tokio::test]
async fn the_relay_refuses_what_is_too_large() {
    let dir = scratch("toobig");
    let config = RelayConfig {
        url: start(&dir).await,
        token: TOKEN.to_string(),
    };
    let huge = vec![b'x'; relay::MAX_VALUE_BYTES + 1];
    assert!(matches!(
        relay::client::put(&config, relay::KEY_MIXES, &huge).await,
        Err(RelayError::TooLarge { .. })
    ));
    // The oversized write left nothing behind.
    assert_eq!(
        relay::client::get(&config, relay::KEY_MIXES, 0)
            .await
            .unwrap(),
        Fetched::Missing
    );
}

/// A relay that is not there is the ordinary case on a phone, not an
/// exceptional one: out of the flat, off the tailnet, no relay. It must come
/// back as an error the caller can shrug at, and reasonably quickly.
#[tokio::test]
async fn an_unreachable_relay_fails_rather_than_hangs() {
    let config = RelayConfig {
        // Port 1 on loopback: refused immediately, no DNS, no waiting.
        url: "http://127.0.0.1:1".to_string(),
        token: TOKEN.to_string(),
    };
    assert!(matches!(
        relay::client::get(&config, relay::KEY_MIXES, 0).await,
        Err(RelayError::Transport(_))
    ));
}

/// Half-filled settings are the normal state while someone is typing, and must
/// not produce a request at all.
#[tokio::test]
async fn an_unconfigured_relay_is_not_contacted() {
    let half = RelayConfig {
        url: "http://127.0.0.1:1".to_string(),
        token: String::new(),
    };
    assert_eq!(
        relay::client::get(&half, relay::KEY_MIXES, 0).await,
        Err(RelayError::NotConfigured)
    );
    assert_eq!(
        relay::client::put(&half, relay::KEY_MIXES, b"x").await,
        Err(RelayError::NotConfigured)
    );
}
