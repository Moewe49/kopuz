//! VISIONOS passed 6/6 in a paced experiment and then failed 45/45 inside the
//! app. Two things differ, so vary exactly those two and nothing else:
//!
//!   visitor_data present / absent   ×   paced 10s / burst
//!
//! The app runs the absent+burst corner. Whichever factor flips the result is
//! the one to fix — guessing between them is how the last three days went.
//!
//! Run: cargo run -p server --example visionos_factors

use server::ytmusic::clients::VISIONOS;
use server::ytmusic::innertube::{self, PlayerExtras};
use std::time::Duration;

const VIDEOS: &[&str] = &["9bZkp7q19f0", "kJQP7kiw5Fk", "JGwWNGJdvx8", "OPf0YbXqDm0"];

async fn run(label: &str, visitor: Option<&str>, spacing: Duration) -> (u32, u32) {
    let (mut ok, mut bad) = (0, 0);
    for vid in VIDEOS {
        let res = innertube::player(
            VISIONOS,
            vid,
            None,
            PlayerExtras { content_pot: None, visitor_data: visitor, signature_timestamp: None },
        )
        .await;
        let status = match &res {
            Ok(j) => j
                .pointer("/playabilityStatus/status")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            Err(e) => format!("ERR {e}"),
        };
        if status == "OK" { ok += 1 } else { bad += 1 }
        println!("  {label:<28} {vid}  {status}");
        tokio::time::sleep(spacing).await;
    }
    (ok, bad)
}

#[tokio::main]
async fn main() {
    let visitor = innertube::visitor_id(None).await.unwrap_or_default();
    println!("fresh visitor_data: {} chars\n", visitor.len());

    // Burst arms first: if a burst poisons the session, the paced arms
    // afterwards will show it, which is itself the answer.
    let d = Duration::from_millis(0);
    let p = Duration::from_secs(10);

    println!("--- burst, no visitor_data  (what the app does) ---");
    let a = run("burst / no vd", None, d).await;
    println!("--- burst, with visitor_data ---");
    let b = run("burst / vd", Some(&visitor), d).await;
    println!("--- paced 10s, no visitor_data ---");
    let c = run("paced / no vd", None, p).await;
    println!("--- paced 10s, with visitor_data  (the experiment that passed) ---");
    let e = run("paced / vd", Some(&visitor), p).await;

    println!("\n=== tally (ok/total) ===");
    for (label, (ok, bad)) in [
        ("burst / no vd  <- app", a),
        ("burst / vd", b),
        ("paced / no vd", c),
        ("paced / vd", e),
    ] {
        println!("{label:<24} {ok}/{}", ok + bad);
    }
    println!(
        "\nOnly the vd arms passing -> send visitor_data.\n\
         Only the paced arms passing -> pace the look-ahead.\n\
         All failing -> the session is already burned; retest on a fresh one."
    );
}
