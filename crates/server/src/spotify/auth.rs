//! Spotify OAuth 2.0 Authorization-Code + PKCE flow. No client
//! secret — the user supplies their own (free) app client id and
//! registers `http://127.0.0.1:8898/callback` as redirect URI in the
//! Spotify developer dashboard. We bind a one-shot loopback listener,
//! hand the authorize URL to the caller (which opens the system
//! browser), and trade the returned code for an access + refresh
//! token pair.

use std::io::{Read, Write};
use std::net::TcpListener;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Fixed loopback port — must match the redirect URI the user
/// registers in their Spotify app. Documented in the import dialog.
pub const REDIRECT_PORT: u16 = 8898;
pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/callback";
const SCOPES: &str = "playlist-read-private playlist-read-collaborative user-library-read";

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
