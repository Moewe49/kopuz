//! What would the rediscovery mix actually contain, on this machine's real
//! history? Thresholds tuned against synthetic data are guesses; this checks
//! them against the library and play counts that exist.
//!
//! Run: cargo run -p reader --example rediscover_preview

use std::collections::HashMap;

/// The reader crate has no `directories` dependency, and adding one for a
/// diagnostic example would be the wrong trade. The path is stable per OS.
fn config_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|p| std::path::PathBuf::from(p).join("temidaradev/kopuz/config"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME").map(|p| {
            std::path::PathBuf::from(p).join(".config/temidaradev/kopuz")
        })
    }
}

fn main() {
    let Some(dir) = config_dir() else {
        eprintln!("no config dir");
        return;
    };
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap_or_default())
            .unwrap_or_default();

    // The history is what the mix is built from now. Until the running build
    // starts writing it, fall back to synthesising it from listen_counts so the
    // preview still says something — clearly labelled, because the synthesised
    // form has no timestamps and no titles.
    let history: HashMap<String, config::PlayRecord> = cfg
        .get("play_history")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let synthesised = history.is_empty();
    let history = if synthesised {
        let lib: reader::Library = serde_json::from_str(
            &std::fs::read_to_string(dir.join("library.json")).unwrap_or_default(),
        )
        .unwrap_or_default();
        let meta: HashMap<String, (String, String)> = lib
            .tracks
            .iter()
            .chain(lib.jellyfin_tracks.iter())
            .map(|t| {
                (
                    t.path.to_string_lossy().into_owned(),
                    (t.title.clone(), t.artist.clone()),
                )
            })
            .collect();
        cfg.get("listen_counts")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        let plays = v.as_u64()?;
                        let (title, artist) = meta
                            .get(k)
                            .cloned()
                            .unwrap_or_else(|| (k.chars().take(28).collect(), "?".into()));
                        Some((
                            k.clone(),
                            config::PlayRecord {
                                title,
                                artist,
                                plays,
                                last_played: 0,
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        history
    };

    let mut recent: Vec<String> = Vec::new();
    for key in ["recently_played", "recently_played_server"] {
        if let Some(a) = cfg.get(key).and_then(|v| v.as_array()) {
            recent.extend(a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())));
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    println!(
        "history: {} entries{}   recent: {}",
        history.len(),
        if synthesised {
            "  (synthesised from listen_counts — no timestamps yet)"
        } else {
            ""
        },
        recent.len()
    );

    for week in [0u64, 1, 2] {
        let mix = reader::rediscover::build(&history, &recent, now, week + 1);
        println!(
            "
--- seed {week}: {} tracks (eligible {}, held back {}) ---",
            mix.tracks.len(),
            mix.eligible,
            mix.excluded_recent
        );
        for t in mix.tracks.iter().take(8) {
            let title: String = t.title.chars().take(44).collect();
            println!("   {:>3}x  {title}  —  {}", t.plays, t.artist);
        }
        if mix.tracks.len() > 8 {
            println!("   … {} more", mix.tracks.len() - 8);
        }
    }
}
