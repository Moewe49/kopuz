//! Run the background analysis job against this machine's real history.
//!
//! Run: cargo run -p embed --release --example analyse -- <budget>
//! Needs KOPUZ_ORT and KOPUZ_MODEL.

fn config_dir() -> std::path::PathBuf {
    directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("./config"))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let budget: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let dir = config_dir();
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap_or_default())
            .unwrap_or_default();
    let counts: std::collections::HashMap<String, u64> = cfg
        .get("listen_counts")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut plays: Vec<(String, u64)> = counts
        .iter()
        .filter(|&(_, &n)| n >= 3)
        .filter_map(|(k, &n)| {
            let mut parts = k.split(':');
            if parts.next()? != "ytmusic" {
                return None;
            }
            Some((parts.next()?.to_string(), n))
        })
        .collect();
    plays.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let ids: Vec<String> = plays.into_iter().map(|(id, _)| id).collect();
    println!("{} tracks in the history", ids.len());

    let paths = embed::job::Paths {
        runtime: std::env::var("KOPUZ_ORT").unwrap_or_default().into(),
        model: std::env::var("KOPUZ_MODEL").unwrap_or_default().into(),
        store: dir.join("style_vectors.bin"),
        labels: dir.join("style_meta.json"),
    };
    if !embed::job::is_ready(&paths) {
        eprintln!("set KOPUZ_ORT and KOPUZ_MODEL to existing files");
        std::process::exit(1);
    }

    let started = std::time::Instant::now();
    match embed::job::analyse(&ids, &paths, budget).await {
        Ok(p) => println!(
            "embedded {}, relabelled {}, failed {}, remaining {}, total {} — {:.1}s",
            p.embedded,
            p.relabelled,
            p.failed,
            p.remaining,
            p.total,
            started.elapsed().as_secs_f32(),
        ),
        Err(e) => eprintln!("job failed: {e}"),
    }
}
