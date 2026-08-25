//! What the model actually calls each analysed track.
//!
//! The 400 activations are named Discogs styles. Comparing whole vectors by
//! cosine turned out not to separate a loud pop song from loud guitar music;
//! reading the top labels asks the model directly instead.
//!
//! Run: cargo run -p embed --release --example style_labels -- <labels.json> [filter]

use std::collections::HashMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(classes_path) = args.next() else {
        eprintln!("usage: style_labels <classes.json> [name filter]");
        return;
    };
    let filter = args.next().unwrap_or_default().to_lowercase();

    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&classes_path).expect("read classes"))
            .expect("parse classes");
    let classes: Vec<String> = meta["classes"]
        .as_array()
        .expect("classes array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    let dir = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("./config"));
    let store =
        reader::vectors::VectorStore::load(&dir.join("style_vectors.bin"), 400).expect("store");
    let names: HashMap<String, (String, String, String)> = serde_json::from_str(
        &std::fs::read_to_string(dir.join("style_meta.json")).unwrap_or_default(),
    )
    .unwrap_or_default();

    let (ids, vectors) = store.matrix();
    for (id, v) in ids.iter().zip(&vectors) {
        let label = names
            .get(id)
            .map(|(a, t, _)| format!("{a} — {t}"))
            .unwrap_or_else(|| id.clone());
        if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
            continue;
        }
        let mut top: Vec<(usize, f32)> = v.iter().copied().enumerate().collect();
        top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let named: Vec<String> = top
            .iter()
            .take(3)
            .map(|(i, s)| format!("{} {:.2}", classes[*i], s))
            .collect();
        println!("{label}\n    {}", named.join("  |  "));
    }
}
