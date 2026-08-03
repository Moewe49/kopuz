//! Spotify OAuth 2.0 Authorization-Code + PKCE flow. No client
//! secret — the user supplies their own (free) app client id and
//! registers `http://127.0.0.1:8898/callback` as redirect URI in the
//! Spotify developer dashboard. We bind a one-shot loopback listener,
//! hand the authorize URL to the caller (which opens the system
//! browser), and trade the returned code for an access + refresh
//! token pair.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Fixed loopback port — must match the redirect URI the user
/// registers in their Spotify app. Documented in the import dialog.
pub const REDIRECT_PORT: u16 = 8898;
pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/callback";
// modify-* scopes let kopuz auto-follow a playlist before importing it — a
// dev-mode app can only read the FULL track list of playlists the user owns or
// follows, so following first lifts the 403 on other people's public playlists.
const SCOPES: &str = "playlist-read-private playlist-read-collaborative user-library-read playlist-modify-private playlist-modify-public";

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: u64,
}

pub struct PkcePending {
    pub authorize_url: String,
    verifier: String,
    client_id: String,
    listener: TcpListener,
}

/// Step 1: build the authorize URL and bind the loopback listener.
/// Binding *before* the browser opens closes the race where Spotify
/// redirects into a port nobody owns yet.
pub fn begin_pkce(client_id: &str) -> Result<PkcePending, String> {
    let client_id = client_id.trim().to_string();
    if client_id.is_empty() {
        return Err("Spotify client id is empty".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .map_err(|e| format!("Cannot bind 127.0.0.1:{REDIRECT_PORT}: {e}"))?;

    let verifier: String = {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
        let mut rng = rand::rng();
        (0..64)
            .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
            .collect()
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state: String = {
        let mut rng = rand::rng();
        (0..16)
            .map(|_| char::from_digit(rng.random_range(0..36), 36).unwrap_or('x'))
            .collect()
    };

    let authorize_url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge_method=S256&code_challenge={}&state={}&scope={}",
        urlenc(&client_id),
        urlenc(REDIRECT_URI),
        urlenc(&challenge),
        urlenc(&state),
        urlenc(SCOPES),
    );

    Ok(PkcePending {
        authorize_url,
        verifier,
        client_id,
        listener,
    })
}

/// Step 2 (blocking — run on a blocking thread): wait for the browser
/// redirect, answer it with a tiny "you can close this tab" page, and
/// exchange the code for tokens.
pub async fn finish_pkce(pending: PkcePending) -> Result<TokenResponse, String> {
    let PkcePending {
        verifier,
        client_id,
        listener,
        ..
    } = pending;

    let code = tokio::task::spawn_blocking(move || wait_for_code(&listener))
        .await
        .map_err(|e| format!("listener task: {e}"))??;

    let resp = super::api::http_client()
        .post("https://accounts.spotify.com/api/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("token exchange HTTP: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token exchange failed ({status}): {body}"));
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(|e| format!("token exchange JSON: {e}"))
}

/// Refresh an expired access token. Spotify rotates refresh tokens on
/// PKCE apps — persist the returned one when present.
pub async fn refresh_token(client_id: &str, refresh: &str) -> Result<TokenResponse, String> {
    let resp = super::api::http_client()
        .post("https://accounts.spotify.com/api/token")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client_id),
        ])
        .send()
        .await
        .map_err(|e| format!("token refresh HTTP: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!(
            "token refresh failed ({status}) — reconnect Spotify"
        ));
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(|e| format!("token refresh JSON: {e}"))
}

/// An access token ready to use, plus the rotated refresh token when Spotify
/// handed out a new one.
#[derive(Debug, Clone)]
pub struct FreshToken {
    pub access_token: String,
    /// `Some` only when Spotify rotated the refresh token. The caller **must**
    /// persist it — the previous one is dead from that moment on.
    pub rotated_refresh: Option<String>,
}

/// Process-wide access token, so every feature that talks to Spotify (import,
/// search, …) shares one.
static ACCESS: std::sync::Mutex<Option<(String, Instant)>> = std::sync::Mutex::new(None);
/// Serializes refreshes. Spotify rotates PKCE refresh tokens, so two refreshes
/// racing means one of them burns the other's token and the account silently
/// disconnects.
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Refresh this long before the token actually expires, so a request can't
/// start valid and land expired.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

fn cached_access() -> Option<String> {
    let g = ACCESS.lock().ok()?;
    let (tok, at) = g.as_ref()?;
    (*at > Instant::now()).then(|| tok.clone())
}

/// A usable access token for `client_id`, refreshing through `refresh` when the
/// cached one is gone or about to expire.
pub async fn access_token(client_id: &str, refresh: &str) -> Result<FreshToken, String> {
    if let Some(tok) = cached_access() {
        return Ok(FreshToken {
            access_token: tok,
            rotated_refresh: None,
        });
    }
    if client_id.trim().is_empty() || refresh.trim().is_empty() {
        return Err("Spotify not connected".into());
    }
    let _guard = REFRESH_LOCK.lock().await;
    // Someone else may have refreshed while we waited for the lock.
    if let Some(tok) = cached_access() {
        return Ok(FreshToken {
            access_token: tok,
            rotated_refresh: None,
        });
    }
    let tokens = refresh_token(client_id, refresh).await?;
    if let Ok(mut g) = ACCESS.lock() {
        let ttl = Duration::from_secs(tokens.expires_in).saturating_sub(EXPIRY_MARGIN);
        *g = Some((tokens.access_token.clone(), Instant::now() + ttl));
    }
    Ok(FreshToken {
        access_token: tokens.access_token,
        rotated_refresh: tokens.refresh_token.filter(|r| !r.is_empty()),
    })
}

/// Seed the shared cache with a token obtained some other way (the initial
/// PKCE exchange), so the first API call after connecting doesn't refresh.
pub fn remember_access(access_token: &str, expires_in: u64) {
    if let Ok(mut g) = ACCESS.lock() {
        let ttl = Duration::from_secs(expires_in).saturating_sub(EXPIRY_MARGIN);
        *g = Some((access_token.to_string(), Instant::now() + ttl));
    }
}

/// Drop the cached token (disconnect, or a 401 that means it went stale early).
pub fn forget_access() {
    if let Ok(mut g) = ACCESS.lock() {
        *g = None;
    }
}

fn wait_for_code(listener: &TcpListener) -> Result<String, String> {
    // Browsers may open a speculative second connection; loop until a
    // request actually carrying /callback?code= arrives.
    for incoming in listener.incoming() {
        let mut stream = incoming.map_err(|e| format!("accept: {e}"))?;
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let Some(line) = req.lines().next() else {
            continue;
        };
        // "GET /callback?code=…&state=… HTTP/1.1"
        let Some(path) = line.split_whitespace().nth(1) else {
            continue;
        };
        if !path.starts_with("/callback") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        }
        let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
        let mut code = None;
        let mut error = None;
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("code", v)) => code = Some(v.to_string()),
                Some(("error", v)) => error = Some(v.to_string()),
                _ => {}
            }
        }
        let body = if code.is_some() {
            "<html><body style='font-family:sans-serif;background:#121212;color:#eee;display:flex;align-items:center;justify-content:center;height:100vh'><h2>Spotify connected &#10003; &mdash; you can close this tab and return to Kopuz.</h2></body></html>"
        } else {
            "<html><body style='font-family:sans-serif'><h2>Spotify sign-in failed or was denied.</h2></body></html>"
        };
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        );
        if let Some(code) = code {
            return Ok(code);
        }
        if let Some(err) = error {
            return Err(format!("Spotify authorization denied: {err}"));
        }
    }
    Err("listener closed before a callback arrived".into())
}

fn urlenc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}
