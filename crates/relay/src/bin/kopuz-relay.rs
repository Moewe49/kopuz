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

/// Shortest secret worth calling one. A short token is worse than an obviously
/// missing one, because it looks like it is working.
const MIN_TOKEN_LEN: usize = 16;

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
    let token = token.trim().to_string();
    if token.len() < MIN_TOKEN_LEN {
        eprintln!("KOPUZ_RELAY_TOKEN is too short — use at least {MIN_TOKEN_LEN} characters.");
        std::process::exit(2);
    }

    let bind = std::env::var("KOPUZ_RELAY_BIND").unwrap_or_else(|_| "0.0.0.0:8484".to_string());
    let data_path = std::path::PathBuf::from(
        std::env::var("KOPUZ_RELAY_DATA").unwrap_or_else(|_| "kopuz-relay-state.json".to_string()),
    );

    eprintln!(
        "[relay] {} value(s) restored from {}",
        relay::server::stored_count(&data_path),
        data_path.display()
    );
    let app = relay::server::router(token, &data_path);

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
