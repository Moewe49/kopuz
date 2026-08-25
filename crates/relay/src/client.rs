//! Talking to the relay from a device.
//!
//! Both directions are here because both are small, and because a device is
//! rarely only one of the two: the desktop publishes mixes and will later read
//! a listening position back.

use crate::{Fetched, MAX_VALUE_BYTES, RelayConfig, RelayError, Stored};

/// Short, because a relay that is not answering should not hold up a launch.
/// A device that misses one round simply reads what it already had.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn client() -> Result<reqwest::Client, RelayError> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| RelayError::Transport(e.to_string()))
}

/// Publish a value under `key`.
pub async fn put(config: &RelayConfig, key: &str, bytes: &[u8]) -> Result<Stored, RelayError> {
    if !config.is_configured() {
        return Err(RelayError::NotConfigured);
    }
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(RelayError::TooLarge { bytes: bytes.len() });
    }
    let response = client()?
        .put(config.endpoint(key))
        .bearer_auth(&config.token)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(RelayError::Unauthorised);
    }
    if response.status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return Err(RelayError::TooLarge { bytes: bytes.len() });
    }
    if !response.status().is_success() {
        return Err(RelayError::Protocol(format!("HTTP {}", response.status())));
    }
    let text = response
        .text()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| RelayError::Protocol(e.to_string()))
}

/// Read a value, saying which version is already held.
///
/// `have` is the version the caller already has, or 0 for none. The relay
/// answers [`Fetched::Unchanged`] when they match, so a phone on mobile data
/// pays a few hundred bytes to learn there is nothing new rather than fifty
/// kilobytes.
pub async fn get(config: &RelayConfig, key: &str, have: u64) -> Result<Fetched, RelayError> {
    if !config.is_configured() {
        return Err(RelayError::NotConfigured);
    }
    let response = client()?
        .get(config.endpoint(key))
        .bearer_auth(&config.token)
        .query(&[("have", have.to_string())])
        .send()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(RelayError::Unauthorised);
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Fetched::Missing);
    }
    if !response.status().is_success() {
        return Err(RelayError::Protocol(format!("HTTP {}", response.status())));
    }
    let text = response
        .text()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| RelayError::Protocol(e.to_string()))
}

/// Whether the relay is reachable and the token is accepted.
///
/// For the settings screen: someone who has just typed a URL and a token wants
/// to know now, not the next time a mix set happens to be published.
pub async fn check(config: &RelayConfig) -> Result<(), RelayError> {
    // A read of a key that may not exist is the cheapest thing that still
    // exercises the URL and the token. "Nothing stored yet" is a pass.
    match get(config, crate::KEY_MIXES, u64::MAX).await {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}
