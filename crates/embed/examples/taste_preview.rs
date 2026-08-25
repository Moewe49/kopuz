//! Cluster the analysed library by what it actually sounds like, and print the
//! directions that come out.
//!
//! The mixes shelf currently separates directions by how much two YouTube
//! radios overlap — a good heuristic, but a proxy. This is the thing it was a
//! proxy for. Run both and compare before replacing anything.
//!
//! Run: cargo run -p embed --release --example taste_preview

use std::collections::HashMap;

fn config_dir() -> std::path::PathBuf {
    directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("./config"))
}

fn main() {
    let dir = config_dir();
    let store = match reader::vectors::VectorStore::load(&dir.join("style_vectors.bin"), 400) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vector store: {e}");
            return;
        }
    };
    // Written by build_mixes; missing names simply print as ids.
    let meta: HashMap<String, (String, String, String)> = serde_json::from_str(
        &std::fs::read_to_string(dir.join("style_meta.json")).unwrap_or_default(),
    )
    .unwrap_or_default();

    let (ids, vectors) = store.matrix();
    println!("{} analysed tracks\n", vectors.len());
    if vectors.len() < 4 {
        println!("not enough to cluster yet");
        return;
    }

    // Why this k and not another: the chooser rejects a split when two
    // centroids are too similar, and that threshold was calibrated on
    // track-to-track distances. Centroids are averages, so they sit closer
    // together by construction — print both so the two can be told apart.
    println!("k   silhouette   worst centroid overlap   sizes");
    for probe in 2..=6 {
        let cl = reader::taste::cluster(&vectors, probe, 42);
        let mut worst = 0f32;
        for i in 0..cl.len() {
            for j in i + 1..cl.len() {
                worst = worst.max(embed::similarity(&cl[i].centroid, &cl[j].centroid));
            }
        }
        println!(
            "{probe}   {:>10.3}   {worst:>21.3}   {:?}",
            reader::taste::silhouette_of(&vectors, &cl),
            cl.iter().map(|c| c.members.len()).collect::<Vec<_>>()
        );
    }
    println!();

    let k: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let clusters = reader::taste::cluster(&vectors, k, 42);
    println!("-> {} taste direction(s)\n", clusters.len());

    let label = |i: usize| {
        meta.get(&ids[i])
            .map(|(a, t, _)| format!("{a} — {t}"))
            .unwrap_or_else(|| ids[i].clone())
    };

    for (n, c) in clusters.iter().enumerate() {
        println!(
            "=== direction {} — {} tracks, cohesion {:.3} ===",
            n + 1,
            c.members.len(),
            c.cohesion
        );
        for &i in c.members.iter().take(10) {
            println!(
                "    {:.3}  {}",
                embed::similarity(&vectors[i], &c.centroid),
                label(i)
            );
        }
        println!();
    }

    // How far apart the directions actually are, in the same units the
    // listening test was calibrated in: the listener's own favourites spanned
    // 0.489 to 0.905 between each other.
    println!("centroid similarity between directions:");
    for i in 0..clusters.len() {
        for j in i + 1..clusters.len() {
            println!(
                "  {} vs {}: {:.3}",
                i + 1,
                j + 1,
                embed::similarity(&clusters[i].centroid, &clusters[j].centroid)
            );
        }
    }
}
