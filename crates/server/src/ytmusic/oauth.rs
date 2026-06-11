//! YouTube OAuth 2.0 device-code flow — the "limited-input device" (TV)
//! client, the same scheme `ytmusicapi` and `yt-dlp` use.
//!
//! Why this exists: the cookie session (pasted or browser-read) rotates and
//! ultimately depends on a browser staying signed in. OAuth instead hands us a
//! long-lived **refresh token**: kopuz mints a short-lived access token from it
//! on startup and periodically, with no browser process kept alive and no
//! re-pasting. That's the right fit for a lightweight player meant to sit
//! quietly next to a game.
//!
//! Flow:
//! 1. [`request_device_code`] → show the user a short `user_code` + a URL.
//! 2. The user opens the URL in *any* browser, signs in once, types the code.
//! 3. We [`poll_once`] the token endpoint until it flips from
//!    `authorization_pending` to a token pair.
//! 4. We persist the `refresh_token`; [`refresh`] turns it into a Bearer
//!    access token for InnerTube on every launch thereafter.
//!
//! The access token is carried through the existing auth plumbing as the
//! sentinel string `oauth:<access_token>` (see `innertube::apply_auth`), so
//! none of the per-endpoint call sites need to know which scheme is active.

use serde::Deserialize;

use super::innertube::http_client;

/// The OAuth client baked into the YouTube on TV / limited-input-device app.
/// This is **not** a secret — it ships publicly in the TV client and is used
/// verbatim by ytmusicapi and yt-dlp. The device-code grant still requires
/// explicit user consent in a browser, so possessing these identifiers grants
/// nothing on its own.
pub const CLIENT_ID: &str =
    "861556708454-d6dlm3lh05idd8npek18k6be8ba3oc68.apps.googleusercontent.com";
pub const CLIENT_SECRET: &str = "SboVhoG9s0rNafixCSGGKXAT";

const SCOPE: &str = "https://www.googleapis.com/auth/youtube";
const DEVICE_CODE_URL: &str = "https://www.youtube.com/o/oauth2/device/code";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Device-code polling grant. Google still accepts the legacy OAuth 1.0-era
/// identifier for this client; the modern `urn:` form is rejected for it.
const GRANT_DEVICE: &str = "http://oauth.net/grant_type/device/1.0";

/// What the device-code endpoint hands back: the code to show the user plus
/// polling parameters.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    /// Where the user types the code. Google returns it under
    /// `verification_url` for this client (not the spec's `verification_uri`).
    #[serde(alias = "verification_uri")]
    pub verification_url: String,
    /// Seconds to wait between polls.
    #[serde(default = "default_interval")]
    pub interval: u64,
    /// Seconds until `device_code` expires.
    #[serde(default)]
    pub expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

/// A freshly minted token pair.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// One poll attempt's outcome. The UI loops on [`PollResult::Pending`] /
/// [`PollResult::SlowDown`], waiting `interval` seconds between calls.
#[derive(Debug, Clone)]
pub enum PollResult {
    /// User hasn't authorized yet — keep polling at the current interval.
    Pending,
    /// Server asked us to back off — add 5s to the interval and keep polling.
    SlowDown,
    /// Done — persist `refresh_token` and use `access_token` now.
    Authorized(Tokens),
    /// Terminal failure (expired code, access denied, transport error).
    Failed(String),
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
}

/// Step 1: ask Google for a device code to show the user.
pub async fn request_device_code() -> Result<DeviceCode, String> {
    let resp = http_client()
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await
        .map_err(|e| format!("device code HTTP: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        return Err(format!("device code HTTP {status}: {snippet}"));
    }
    resp.json::<DeviceCode>()
        .await
        .map_err(|e| format!("device code parse: {e}"))
}

/// Step 3: poll once. Call repeatedly (every `interval` s) until it returns
/// [`PollResult::Authorized`] or [`PollResult::Failed`].
pub async fn poll_once(device_code: &str) -> PollResult {
    let resp = match http_client()
        .post(TOKEN_URL)
        .form(&[
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("code", device_code),
            ("grant_type", GRANT_DEVICE),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return PollResult::Failed(format!("token poll HTTP: {e}")),
    };

    let parsed: TokenResponse = match resp.json().await {
        Ok(p) => p,
        Err(e) => return PollResult::Failed(format!("token poll parse: {e}")),
    };

    if let (Some(access), Some(refresh)) = (parsed.access_token, parsed.refresh_token) {
        return PollResult::Authorized(Tokens {
            access_token: access,
            refresh_token: refresh,
            expires_in: parsed.expires_in.unwrap_or(3600),
        });
    }
    match parsed.error.as_deref() {
        Some("authorization_pending") => PollResult::Pending,
        Some("slow_down") => PollResult::SlowDown,
        Some("access_denied") => PollResult::Failed("access denied — sign-in cancelled".into()),
        Some("expired_token") => {
            PollResult::Failed("the code expired — start the sign-in again".into())
        }
        Some(other) => PollResult::Failed(format!("OAuth error: {other}")),
        None => PollResult::Failed("OAuth: unexpected empty token response".into()),
    }
}

/// Exchange a stored refresh token for a fresh access token. Returns the
/// access token and its lifetime in seconds.
pub async fn refresh(refresh_token: &str) -> Result<(String, u64), String> {
    let resp = http_client()
        .post(TOKEN_URL)
        .form(&[
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(|e| format!("token refresh HTTP: {e}"))?;
    let parsed: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("token refresh parse: {e}"))?;
    if let Some(access) = parsed.access_token {
        Ok((access, parsed.expires_in.unwrap_or(3600)))
    } else {
        Err(parsed
            .error
            .unwrap_or_else(|| "token refresh: no access_token".into()))
    }
}

/// The sentinel prefix that marks an access token (vs. a cookie header) as it
/// flows through the auth plumbing.
pub const OAUTH_PREFIX: &str = "oauth:";

/// Wrap a bare access token into the `oauth:<token>` sentinel stored in
/// `server.access_token`.
pub fn to_sentinel(access_token: &str) -> String {
    format!("{OAUTH_PREFIX}{access_token}")
}
