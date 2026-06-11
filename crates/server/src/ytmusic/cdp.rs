//! Headless cookie refresh via the Chrome DevTools Protocol (CDP).
//!
//! This powers the "auto-login" path: a kopuz-managed, **persistent** browser
//! profile that the user signs into YouTube once. Thereafter — on every app
//! start and periodically — kopuz launches the same browser **headless**,
//! lets it load music.youtube.com (which refreshes the rotating session
//! cookies), and pulls the cookies straight out of the live browser via CDP
//! `Network.getAllCookies`.
//!
//! Why CDP instead of reading the cookie SQLite (the `rookie` path):
//! - On Windows, Chrome 127+ App-Bound Encryption makes the on-disk cookie DB
//!   undecryptable by anything but Chrome itself. CDP returns the cookies
//!   already decrypted, sidestepping ABE entirely.
//! - CDP reads the browser's *live* cookie jar, so we get the freshly rotated
//!   `__Secure-*PSIDTS`/`SIDCC` values rather than a stale on-disk snapshot.
//!
//! The same routine drives the one-time visible login (poll until the auth
//! cookies appear) and the silent refresh (grab once the page has settled).

#![cfg(not(any(target_os = "android", target_os = "ios")))]

use std::path::Path;
use std::time::{Duration, Instant};

use config::Browser;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::isolated_profile::{browser_command, find_browser_bin, find_host_browser_bin, in_flatpak};

const YT_URL: &str = "https://music.youtube.com/";
/// Where the one-time *visible* login points the browser, so the user lands on
/// the Google sign-in form and is redirected to YT Music once signed in.
const SIGNIN_URL: &str =
    "https://accounts.google.com/ServiceLogin?continue=https%3A%2F%2Fmusic.youtube.com%2F";

async fn resolve_bin(browser: Browser) -> Result<String, String> {
    if in_flatpak() {
        find_host_browser_bin(browser).await
    } else {
        find_browser_bin(browser)
    }
    .ok_or_else(|| format!("{browser} not found — install it to use auto-login"))
}

/// Open a **normal, visible** browser window on the persistent `profile` for the
/// one-time Google login, and return its process id so the caller can close it
/// once the user has signed in.
///
/// Crucially this launch carries **no** `--remote-debugging-port` / automation
/// flags: Google blocks sign-in ("this browser or app may not be secure") when
/// it detects the DevTools debugging pipe. Cookie extraction happens afterwards
/// in a separate headless CDP launch ([`fetch_cookies`]) on the same profile —
/// by then there's no login page involved, so the block never triggers.
pub async fn spawn_login_window(browser: Browser, profile: &Path) -> Result<u32, String> {
    let bin = resolve_bin(browser).await?;
    tokio::fs::create_dir_all(profile)
        .await
        .map_err(|e| format!("mkdir profile: {e}"))?;
    for name in ["SingletonLock", "SingletonCookie", "SingletonSocket"] {
        let _ = tokio::fs::remove_file(profile.join(name)).await;
    }
    let profile_arg = format!("--user-data-dir={}", profile.display());
    let build = |breakaway: bool| {
        let mut cmd = browser_command(&bin);
        cmd.arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(&profile_arg)
            .arg(SIGNIN_URL)
            .kill_on_drop(false);
        #[cfg(target_os = "windows")]
        if breakaway {
            cmd.creation_flags(0x0100_0000);
        }
        let _ = breakaway;
        cmd
    };
    let child = match build(true).spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            build(false).spawn().map_err(|e| format!("spawn {bin}: {e}"))?
        }
        Err(e) => return Err(format!("spawn {bin}: {e}")),
    };
    child.id().ok_or_else(|| "browser exited immediately".to_string())
}

/// Best-effort terminate a login window (and its child processes) by pid.
pub async fn kill_pid(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = tokio::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await;
    }
}

/// Launch `browser` on the persistent `profile`, drive it over CDP, and return
/// a `Cookie:` header of the signed-in youtube.com cookies.
///
/// - `headless`: invisible refresh (`true`) vs. a visible one-time login
///   window (`false`).
/// - `overall_timeout`: how long to keep polling for the signed-in cookies.
///   For login this is generous (the user has to type); for a refresh it's
///   short (the session is already there, we just wait for the page to settle).
///
/// The browser is always terminated before returning, success or error.
pub async fn fetch_cookies(
    browser: Browser,
    profile: &Path,
    headless: bool,
    overall_timeout: Duration,
) -> Result<String, String> {
    let bin = if in_flatpak() {
        find_host_browser_bin(browser).await
    } else {
        find_browser_bin(browser)
    }
    .ok_or_else(|| format!("{browser} not found — install it to use auto-login"))?;

    tokio::fs::create_dir_all(profile)
        .await
        .map_err(|e| format!("mkdir profile: {e}"))?;
    for name in ["SingletonLock", "SingletonCookie", "SingletonSocket"] {
        let _ = tokio::fs::remove_file(profile.join(name)).await;
    }
    // Drop a stale port file so we read the port from *this* launch.
    let dtap = profile.join("DevToolsActivePort");
    let _ = tokio::fs::remove_file(&dtap).await;

    let profile_arg = format!("--user-data-dir={}", profile.display());
    let build = |breakaway: bool| {
        let mut cmd = browser_command(&bin);
        cmd.arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--remote-debugging-port=0")
            .arg("--remote-allow-origins=*")
            .arg(&profile_arg);
        if headless {
            cmd.arg("--headless=new")
                .arg("--mute-audio")
                .arg("--disable-gpu")
                .arg("--window-size=1024,768");
        }
        // Visible login lands on the Google sign-in form; the silent refresh
        // just reloads YT Music to roll the rotating cookies forward.
        cmd.arg(if headless { YT_URL } else { SIGNIN_URL })
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Detach from kopuz's Windows job object, or Chromium's sandbox can't
        // spawn the nested jobs its child processes need (see isolated_profile).
        // Some contexts (no job, or a job that forbids breakaway) reject the
        // flag with "access denied" — the caller retries without it.
        #[cfg(target_os = "windows")]
        if breakaway {
            cmd.creation_flags(0x0100_0000);
        }
        let _ = breakaway;
        cmd
    };

    let mut child = match build(true).spawn() {
        Ok(c) => c,
        // CREATE_BREAKAWAY_FROM_JOB → PermissionDenied where breakaway isn't
        // allowed; fall back to a plain spawn.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => build(false)
            .spawn()
            .map_err(|e| format!("spawn {bin}: {e}"))?,
        Err(e) => return Err(format!("spawn {bin}: {e}")),
    };
    let result = drive(&dtap, overall_timeout).await;
    let _ = child.start_kill();
    let _ = child.wait().await;
    result
}

/// Read the ephemeral debugging port, open a CDP page session, and poll
/// `Network.getAllCookies` until the signed-in cookies appear (or we time out).
async fn drive(dtap: &Path, overall_timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + overall_timeout;
    let port = read_port(dtap, Duration::from_secs(20)).await?;
    let ws_url = page_ws_url(port, Duration::from_secs(15)).await?;
    let (mut sock, _) = connect_async(ws_url.as_str())
        .await
        .map_err(|e| format!("CDP ws connect: {e}"))?;

    let mut id = 0i64;
    loop {
        id += 1;
        let cookies = get_all_cookies(&mut sock, id).await?;
        let header = build_cookie_header(&cookies);
        if is_signed_in(&header) {
            return Ok(header);
        }
        if Instant::now() >= deadline {
            return Err(
                "timed out waiting for a signed-in YouTube session in the managed browser"
                    .to_string(),
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Chrome writes `DevToolsActivePort` (first line = port) once the debugging
/// endpoint is up. Poll for it.
async fn read_port(dtap: &Path, timeout: Duration) -> Result<u16, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(content) = tokio::fs::read_to_string(dtap).await
            && let Some(line) = content.lines().next()
            && let Ok(port) = line.trim().parse::<u16>()
            && port != 0
        {
            return Ok(port);
        }
        if Instant::now() >= deadline {
            return Err("browser did not expose a debugging port (DevToolsActivePort)".to_string());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Ask the debugging HTTP endpoint for a page target and return its
/// `webSocketDebuggerUrl`.
async fn page_ws_url(port: u16, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let url = format!("http://127.0.0.1:{port}/json");
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(targets) = resp.json::<Value>().await
            && let Some(arr) = targets.as_array()
        {
            // Prefer a real page target; fall back to any with a ws url.
            let pick = arr
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                .or_else(|| arr.iter().find(|t| t.get("webSocketDebuggerUrl").is_some()));
            if let Some(t) = pick
                && let Some(ws) = t.get("webSocketDebuggerUrl").and_then(|v| v.as_str())
            {
                return Ok(ws.to_string());
            }
        }
        if Instant::now() >= deadline {
            return Err("no CDP page target became available".to_string());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Send `Network.getAllCookies` and return the `cookies` array from the reply
/// matching our request id (skipping unrelated CDP events).
async fn get_all_cookies(
    sock: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: i64,
) -> Result<Vec<Value>, String> {
    let req = json!({ "id": id, "method": "Network.getAllCookies" });
    sock.send(Message::Text(req.to_string().into()))
        .await
        .map_err(|e| format!("CDP send: {e}"))?;

    let read = async {
        while let Some(msg) = sock.next().await {
            let msg = msg.map_err(|e| format!("CDP recv: {e}"))?;
            if let Message::Text(txt) = msg
                && let Ok(v) = serde_json::from_str::<Value>(txt.as_str())
                && v.get("id").and_then(|x| x.as_i64()) == Some(id)
            {
                let cookies = v
                    .pointer("/result/cookies")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                return Ok(cookies);
            }
        }
        Err("CDP socket closed before reply".to_string())
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .map_err(|_| "CDP getAllCookies timed out".to_string())?
}

/// Build a `name=value; …` header from the youtube.com cookies CDP returned.
fn build_cookie_header(cookies: &[Value]) -> String {
    cookies
        .iter()
        .filter(|c| {
            c.get("domain")
                .and_then(|d| d.as_str())
                .map(|d| d.contains("youtube.com"))
                .unwrap_or(false)
        })
        .filter_map(|c| {
            let name = c.get("name").and_then(|v| v.as_str())?;
            let value = c.get("value").and_then(|v| v.as_str())?;
            if name.is_empty() || value.is_empty() {
                return None;
            }
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn is_signed_in(header: &str) -> bool {
    let has = |n: &str| {
        header
            .split(';')
            .any(|p| p.trim().starts_with(&format!("{n}=")))
    };
    (has("SAPISID") || has("__Secure-3PAPISID")) && (has("SID") || has("__Secure-3PSID"))
}
