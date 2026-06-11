//! Manual probe for the CDP cookie-refresh plumbing. Launches headless Chrome
//! on a throwaway profile against anonymous YouTube. Expected result against a
//! signed-OUT profile: Err("timed out waiting for a signed-in YouTube session")
//! — which means the port discovery, websocket, and Network.getAllCookies all
//! worked. Any other error points at a plumbing break.
//!
//! Run: cargo run -p server --example cdp_probe -- chrome
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "chrome".to_string());
    let browser = config::Browser::from_id(&id).expect("unknown browser id");
    let profile = std::env::temp_dir().join("kopuz_cdp_probe");
    println!("Probing {id} (profile: {})", profile.display());
    let r = server::ytmusic::cdp::fetch_cookies(browser, &profile, true, Duration::from_secs(10)).await;
    match r {
        Ok(h) => println!("OK signed-in header ({} bytes)", h.len()),
        Err(e) => println!("Err: {e}"),
    }
}
