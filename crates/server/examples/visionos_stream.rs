//! Does a VISIONOS stream actually play, or does it only say OK?
//!
//! The /player endpoint answering OK has misled this investigation twice
//! already: a URL can be issued and then refuse every byte past the opening
//! chunk. So this fetches real byte ranges — start, middle, and the tail where
//! the Matroska Cues live — because that tail is the first thing the decoder
//! asks for.
//!
//! Run: cargo run -p server --example visionos_stream

use server::ytmusic::clients::YouTubeClient;
use server::ytmusic::innertube::{self, PlayerExtras};
use std::time::Duration;

const VISIONOS: YouTubeClient = YouTubeClient {
    client_name: "VISIONOS",
    client_version: "1.02",
    client_id: "101",
    user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
    os_name: "visionOS",
    os_version: "26.5.23O471",
    device_make: "Apple",
    device_model: "RealityDevice17,1",
    android_sdk_version: None,
    login_supported: false,
    use_signature_timestamp: false,
    is_embedded: false,
};

#[tokio::main]
async fn main() {
    let vid = std::env::args().nth(1).unwrap_or_else(|| "9bZkp7q19f0".into());
    let visitor = innertube::visitor_id(None).await.unwrap_or_default();
    let json = match innertube::player(
        VISIONOS,
        &vid,
        None,
        PlayerExtras { content_pot: None, visitor_data: Some(&visitor), signature_timestamp: None },
    )
    .await
    {
        Ok(j) => j,
        Err(e) => {
            eprintln!("player failed: {e}");
            return;
        }
    };

    let Some(fmts) = json.pointer("/streamingData/adaptiveFormats").and_then(|v| v.as_array())
    else {
        eprintln!("no adaptiveFormats");
        return;
    };

    // VISIONOS returns one entry per dubbed language. Picking blind plays a
    // dub, so prefer the track YouTube marks original/default.
    let audio: Vec<&serde_json::Value> = fmts
        .iter()
        .filter(|f| {
            f.get("mimeType").and_then(|m| m.as_str()).is_some_and(|m| m.starts_with("audio/"))
        })
        .collect();
    println!("audio formats: {}", audio.len());

    let is_original = |f: &serde_json::Value| -> bool {
        f.pointer("/audioTrack/audioIsDefault").and_then(|v| v.as_bool()).unwrap_or(false)
            || f.pointer("/audioTrack/id").and_then(|v| v.as_str()).is_some_and(|s| s.contains("original"))
    };
    let chosen = audio
        .iter()
        .find(|f| is_original(f))
        .or_else(|| audio.first())
        .copied();
    let Some(fmt) = chosen else {
        eprintln!("no audio format");
        return;
    };
    println!(
        "chosen itag={:?} mime={:?} track={:?} default={:?}",
        fmt.get("itag"),
        fmt.get("mimeType").and_then(|v| v.as_str()),
        fmt.pointer("/audioTrack/displayName").and_then(|v| v.as_str()),
        fmt.pointer("/audioTrack/audioIsDefault"),
    );
    let Some(url) = fmt.get("url").and_then(|v| v.as_str()) else {
        eprintln!("format carries no plain url (ciphered or SABR-only)");
        return;
    };

    let client = reqwest::Client::builder()
        .user_agent(VISIONOS.user_agent)
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    let total = client
        .get(url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .ok()
        .and_then(|r| {
            r.headers().get("content-range")?.to_str().ok()?.rsplit('/').next()?.parse::<u64>().ok()
        })
        .unwrap_or(0);
    println!("total = {total} bytes\n");

    let probes = [
        ("start   0-512K", format!("bytes=0-{}", 524_287u64.min(total.saturating_sub(1)))),
        ("mid-file 512K", format!("bytes={}-{}", total / 2, total / 2 + 524_287)),
        ("last 600K", format!("bytes={}-{}", total.saturating_sub(600_000), total.saturating_sub(1))),
        ("tail 160B (Cues)", format!("bytes={}-{}", total.saturating_sub(160), total.saturating_sub(1))),
    ];
    let mut all_ok = true;
    for (label, range) in probes {
        match client.get(url).header("Range", &range).send().await {
            Ok(r) => {
                if !r.status().is_success() { all_ok = false }
                println!("{label:<18} -> {} len={:?}", r.status(), r.content_length());
            }
            Err(e) => { all_ok = false; println!("{label:<18} -> error {e}") }
        }
    }
    println!(
        "\n{}",
        if all_ok {
            "Every range served — this stream can carry a whole track AND seek."
        } else {
            "Some range refused — same trap as the pot-less fallback."
        }
    );
}
