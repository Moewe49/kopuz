//! What the mixes shelf shows once the library has been analysed.
//!
//! The radio-overlap path can be seen with `mix_preview`; this is the same
//! shelf built from audio instead. Run both to compare.
//!
//! Run: cargo run -p server --release --example mix_from_audio

fn main() {
    let dir = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("./config"));

    let store = match reader::vectors::VectorStore::load(&dir.join("style_vectors.bin"), 400) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vector store: {e}");
            return;
        }
    };
    let labels = server::mixes::load_labels(&dir.join("style_meta.json"));

    println!(
        "{} analysed tracks, {} labelled\n",
        store.len(),
        labels.len()
    );
    let set = server::mixes::from_vectors(&store, &labels, 0, 42);
    for m in &set.mixes {
        println!("=== {} ({} tracks) ===", m.name, m.tracks.len());
        for t in m.tracks.iter() {
            let cover = if t.path.to_string_lossy().contains("urlhex_") {
                "[cover]"
            } else {
                "[  --  ]"
            };
            println!("    {cover} {} — {}", t.artist, t.title);
        }
        println!();
    }
}
