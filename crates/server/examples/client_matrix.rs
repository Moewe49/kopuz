//! Separate "the client is decommissioned" from "this session/IP is flagged".
//!
//! Both hypotheses produce the same symptom — playabilityStatus LOGIN_REQUIRED,
//! "Sign in to confirm you're not a bot" — so no amount of log reading tells
//! them apart. Only varying the client while holding the session fixed does.
//!
//! Read the result as:
//!   ANDROID_VR fails, others succeed  -> the client is dead; migrate.
//!   every arm fails                   -> session/IP reputation; no client
//!                                        change saves us, pacing and cookies
//!                                        might.
//!
//! Deliberately pot-free. A Web BotGuard token is not valid for ANDROID_VR at
//! all (yt-dlp's WEBPO_CLIENTS excludes it), so including it would vary two
//! things at once and answer neither question.
//!
//! Run: cargo run -p server --example client_matrix

use server::ytmusic::clients::YouTubeClient;
use server::ytmusic::innertube::{self, PlayerExtras};
use std::time::Duration;

/// Paced deliberately. A burst is itself a bot signal, and hypothesis (B) is
/// precisely that we burned this session with ~650 rapid requests.
const SPACING: Duration = Duration::from_secs(10);

/// Six distinct videos, because a single fixture lies: `dQw4w9WgXcQ` is
/// reported to pass on configurations that are otherwise broken
/// (lavalink-devs/youtube-source#226, 2026-08-19).
const VIDEOS: &[&str] = &[
    "dQw4w9WgXcQ",
    "9bZkp7q19f0",
    "kJQP7kiw5Fk",
    "JGwWNGJdvx8",
    "OPf0YbXqDm0",
    "60ItHLz5WEA",
];

/// yt-dlp's `visionos`, its current anonymous default. No PO token policy, no
/// JS player, no signature timestamp.
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

/// yt-dlp's `tv_downgraded`. The downgrade is the point: the current TVHTML5
/// version serves SABR-only formats, and dropping back reportedly stops that.
/// Carries no PO token policy at all.
const TV_DOWNGRADED: YouTubeClient = YouTubeClient {
    client_name: "TVHTML5",
    client_version: "5.20260707",
    client_id: "7",
    // The contested one. yt-dlp ships a Cobalt string; a controlled A/B on
    // 2026-08-19 found that string returns "The page needs to be reloaded"
    // while a PS4 UA returns formats. That is our exact error, so this arm
    // tests the PS4 side of the question.
    user_agent: "Mozilla/5.0 (PlayStation; PlayStation 4/12.00) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/16.0 Safari/605.1.15",
    os_name: "",
    os_version: "",
    device_make: "",
    device_model: "",
    android_sdk_version: None,
    login_supported: true,
    use_signature_timestamp: false,
    is_embedded: false,
};

const TV_DOWNGRADED_COBALT: YouTubeClient = YouTubeClient {
    user_agent: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version",
    ..TV_DOWNGRADED
};

fn cookies() -> Option<String> {
    let p = directories::ProjectDirs::from("com", "temidaradev", "kopuz")?
        .config_dir()
        .join("config.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    v.get("server")?
        .get("access_token")?
        .as_str()
        .map(|s| s.to_string())
}

/// What came back, in one line: did YouTube allow it, and is there a stream we
/// could actually fetch?
fn verdict(json: &serde_json::Value) -> String {
    let status = json
        .pointer("/playabilityStatus/status")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let reason = json
        .pointer("/playabilityStatus/reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let fmts = json
        .pointer("/streamingData/adaptiveFormats")
        .and_then(|v| v.as_array());
    let (plain, ciphered) = fmts.map_or((0, 0), |a| {
        a.iter().fold((0, 0), |(p, c), f| {
            if f.get("url").is_some() {
                (p + 1, c)
            } else if f.get("signatureCipher").is_some() {
                (p, c + 1)
            } else {
                (p, c)
            }
        })
    });
    let sabr = json.pointer("/streamingData/serverAbrStreamingUrl").is_some();
    format!(
        "{status:<16} plain={plain:<3} ciphered={ciphered:<3} sabr={:<5} {}",
        sabr,
        if reason.len() > 44 { &reason[..44] } else { reason }
    )
}

#[tokio::main]
async fn main() {
    let jar = cookies();
    println!("cookies available: {}\n", jar.is_some());

    // One fresh visitor_data for the whole run, so the session is the constant
    // and the client is the only thing that varies.
    let visitor = match innertube::visitor_id(None).await {
        Ok(v) => {
            println!("fresh visitor_data: {} chars\n", v.len());
            v
        }
        Err(e) => {
            eprintln!("could not get visitor_data: {e}");
            return;
        }
    };

    let arms: Vec<(&str, YouTubeClient, bool)> = vec![
        ("2 ANDROID_VR bare", server::ytmusic::clients::ANDROID_VR_1_61_48, false),
        ("3 VISIONOS", VISIONOS, false),
        ("4 TV_DOWN ps4-ua +cookies", TV_DOWNGRADED, true),
        ("5 TV_DOWN cobalt-ua +cookies", TV_DOWNGRADED_COBALT, true),
    ];

    let mut tally: Vec<(String, u32, u32)> = arms
        .iter()
        .map(|(l, _, _)| (l.to_string(), 0, 0))
        .collect();

    // Interleaved: video-major, so a slow drift in reputation hits every arm
    // equally instead of poisoning whichever arm ran last.
    for vid in VIDEOS {
        println!("--- {vid} ---");
        for (i, (label, client, want_cookies)) in arms.iter().enumerate() {
            let ck = if *want_cookies { jar.as_deref() } else { None };
            let extras = PlayerExtras {
                content_pot: None,
                visitor_data: Some(&visitor),
                signature_timestamp: None,
            };
            match innertube::player(*client, vid, ck, extras).await {
                Ok(json) => {
                    let v = verdict(&json);
                    let ok = v.starts_with("OK");
                    if ok { tally[i].1 += 1 } else { tally[i].2 += 1 }
                    println!("  {label:<30} {v}");
                }
                Err(e) => {
                    tally[i].2 += 1;
                    println!("  {label:<30} REQUEST ERROR: {e}");
                }
            }
            tokio::time::sleep(SPACING).await;
        }
    }

    println!("\n=== tally ===");
    for (label, ok, bad) in &tally {
        println!("{label:<30} ok={ok}  not-ok={bad}");
    }
    println!(
        "\nAll arms failing points at session/IP reputation, not the client.\n\
         ANDROID_VR alone failing points at the decommission."
    );
}
