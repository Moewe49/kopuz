//! What would the mixes shelf actually contain, on this machine's real
//! listening history?
//!
//! The overlap threshold that separates one taste direction from another was
//! chosen against synthetic radios. This checks it against real ones.
//!
//! Run: cargo run -p server --example mix_preview

fn config_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.config_dir().to_path_buf())
}

#[tokio::main]
async fn main() {
    let Some(dir) = config_dir() else {
        eprintln!("no config dir");
        return;
    };
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
            (parts.next()? == "ytmusic").then(|| parts.next().map(|id| (id.to_string(), n)))?
        })
        .collect();
    plays.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("{} anchors with >=3 plays\n", plays.len());
    let anchors: Vec<String> = plays.into_iter().map(|(id, _)| id).collect();

    // Anonymous: the mixes are meant to reflect this library, not the account's
    // wider YouTube taste.
    let set = server::mixes::generate(&anchors, "", 0).await;
    println!("{} distinct direction(s)\n", set.mixes.len());
    for m in &set.mixes {
        println!("=== {} ({} tracks) [{}]", m.name, m.tracks.len(), m.id);
        for t in m.tracks.iter().take(6) {
            println!("    {} — {}", t.artist, t.title);
        }
        println!();
    }

    // The threshold that decides "same direction" was picked against synthetic
    // radios. This is what it actually admits.
    println!("pairwise overlap of the mixes that were KEPT:");
    let keys: Vec<std::collections::HashSet<String>> = set
        .mixes
        .iter()
        .map(|m| {
            m.tracks
                .iter()
                .map(|t| server::recommend::track_key(&t.path))
                .collect()
        })
        .collect();
    for i in 0..keys.len() {
        for j in i + 1..keys.len() {
            let shared = keys[i].intersection(&keys[j]).count();
            let union = keys[i].union(&keys[j]).count();
            println!(
                "  {:>28}  vs  {:<28}  {shared:>2}/{union:<3} = {:.3}",
                set.mixes[i].name,
                set.mixes[j].name,
                shared as f32 / union as f32
            );
        }
    }
}
