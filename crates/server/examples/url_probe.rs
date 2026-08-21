//! Probe how googlevideo answers the SAME stream URL under different request
//! shapes. Diagnostic only — resolves one video with the user's own session and
//! reports status codes. Never prints the URL or the cookie.
//!
//! Run: cargo run -p server --example url_probe -- <videoId>

use std::time::Duration;

fn cookies() -> Option<String> {
    let p = directories::ProjectDirs::from("com", "temidaradev", "kopuz")?
        .config_dir()
        .join("config.json");
    let text = std::fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("server")?
        .get("access_token")?
        .as_str()
        .map(|s| s.to_string())
}

#[tokio::main]
async fn main() {
    let vid = std::env::args().nth(1).unwrap_or_else(|| "dQw4w9WgXcQ".into());
    let Some(jar) = cookies() else {
        eprintln!("no session in config");
        return;
    };
    let yt = server::ytmusic::YouTubeMusicClient::with_cookies(jar.clone());
    let info = match yt.get_stream_fresh(&vid).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("resolve failed: {e}");
            return;
        }
    };
    println!("resolved itag={:?} deep_range_safe={}", info.itag, info.deep_range_safe);

    let client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .user_agent(info.user_agent.clone())
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    // Each row is one request shape; only the status is reported.
    // Total size first, so the deep probes can target real offsets.
    let total = client
        .get(&info.url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .ok()
        .and_then(|r| {
            r.headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next().map(|s| s.to_string()))
        })
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    println!("content-range total = {total} bytes");

    let tail = format!("bytes={}-{}", total.saturating_sub(160), total.saturating_sub(1));
    let mid = format!("bytes={}-{}", total / 2, total / 2 + 524_287);
    let late = format!("bytes={}-{}", total.saturating_sub(600_000), total.saturating_sub(1));
    let shapes: Vec<(&str, Option<&str>, bool)> = vec![
        ("plain GET, no cookies", None, false),
        ("Range bytes=0-524287 (start)", Some("bytes=0-524287"), false),
        ("Range mid-file 512K", Some(&mid), false),
        ("Range last 600K", Some(&late), false),
        ("Range last 160 bytes (Cues)", Some(&tail), false),
        ("Range last 160 bytes, with cookies", Some(&tail), true),
    ];
    // YouTube's own player asks for byte ranges via a `range=` QUERY PARAM,
    // not the HTTP header. If googlevideo honours that where it refuses the
    // header, the stream is usable after all.
    println!("--- range= query parameter ---");
    for (label, spec) in [
        ("query range 0-524287", format!("{}-{}", 0, 524_287)),
        ("query range mid-file", format!("{}-{}", total / 2, total / 2 + 524_287)),
        ("query range tail 160B", format!("{}-{}", total.saturating_sub(160), total.saturating_sub(1))),
    ] {
        let url = format!("{}&range={}", info.url, spec);
        match client.get(&url).send().await {
            Ok(resp) => println!("{:38} -> {} len={:?}", label, resp.status(), resp.content_length()),
            Err(e) => println!("{label:38} -> transport error: {e}"),
        }
    }
    println!("--- Range header ---");

    for (label, range, with_cookies) in shapes {
        let mut req = client.get(&info.url);
        if let Some(r) = range {
            req = req.header("Range", r);
        }
        if with_cookies {
            req = req.header("Cookie", jar.clone());
        }
        match req.send().await {
            Ok(resp) => println!(
                "{:38} -> {} len={:?}",
                label,
                resp.status(),
                resp.content_length()
            ),
            Err(e) => println!("{label:38} -> transport error: {e}"),
        }
    }
}
