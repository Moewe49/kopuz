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

use crate::jam::{JamAccess, JamWrite};

/// Open a jam. Authorised by the owner's personal token; returns the session
/// and its join code, which the owner then hands to the other listener.
pub async fn jam_open(config: &RelayConfig) -> Result<JamAccess, RelayError> {
    if !config.is_configured() {
        return Err(RelayError::NotConfigured);
    }
    let response = client()?
        .post(format!("{}/v1/jam", config.url.trim_end_matches('/')))
        .bearer_auth(&config.token)
        .send()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(RelayError::Unauthorised);
    }
    if !response.status().is_success() {
        return Err(RelayError::Protocol(format!("HTTP {}", response.status())));
    }
    let text = response
        .text()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    let session: crate::jam::JamSession =
        serde_json::from_str(&text).map_err(|e| RelayError::Protocol(e.to_string()))?;
    Ok(JamAccess {
        url: config.url.trim_end_matches('/').to_string(),
        id: session.id,
        code: session.code,
    })
}

/// Read a jam's state, saying which version is already held so an unchanged
/// poll comes back as [`Fetched::Unchanged`] in a few bytes. Access is by the
/// join code alone -- no personal token -- so the guest never holds one.
pub async fn jam_read(access: &JamAccess, have: u64) -> Result<Fetched, RelayError> {
    let response = client()?
        .get(access.endpoint())
        .bearer_auth(&access.code)
        .query(&[("have", have.to_string())])
        .send()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    // A gone session and a wrong code both answer 404, on purpose. To a caller
    // that is "the jam has ended", which is what both mean.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(RelayError::JamGone);
    }
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(RelayError::Unauthorised);
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

/// Write a jam's state, guarded by the version it was based on. A
/// [`JamWrite::Conflict`] is not an error: it means someone wrote first, and
/// the caller should re-read, re-apply its change, and try again.
pub async fn jam_write(
    access: &JamAccess,
    bytes: &[u8],
    based_on: u64,
) -> Result<JamWrite, RelayError> {
    if bytes.len() > crate::jam::MAX_JAM_BYTES {
        return Err(RelayError::TooLarge { bytes: bytes.len() });
    }
    let response = client()?
        .put(access.endpoint())
        .bearer_auth(&access.code)
        .header(reqwest::header::IF_MATCH, based_on.to_string())
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(RelayError::JamGone);
    }
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
