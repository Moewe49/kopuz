//! Embed raw 16 kHz mono f32 samples and print the two vectors, so the Rust
//! port can be checked against the Python reference on identical input.
//!
//! Run: cargo run -p embed --example vectors -- <model.onnx> <samples.f32>

use embed::Embedder;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(model), Some(samples)) = (args.next(), args.next()) else {
        eprintln!("usage: vectors <model.onnx> <samples.f32>");
        std::process::exit(2);
    };

    let raw = std::fs::read(&samples).expect("read samples");
    let pcm: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    eprintln!(
        "{} samples ({:.1}s)",
        pcm.len(),
        pcm.len() as f32 / 16_000.0
    );

    let mut embedder = Embedder::open(&model).expect("load model");
    let v = embedder.vectors(&pcm).expect("embed");
    // Machine-readable, so the comparison script does not have to parse prose.
    println!(
        "style {}",
        v.style
            .iter()
            .map(|x| format!("{x:.8}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "embedding {}",
        v.embedding
            .iter()
            .map(|x| format!("{x:.8}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
}
