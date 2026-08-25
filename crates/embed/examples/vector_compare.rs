//! Do the ffmpeg path and the Rust path produce the same style vector?
//!
//! The waveforms agreeing is necessary but not sufficient — the vectors are
//! what every ranking is built on, so they are what has to match.
//!
//! Run: cargo run -p embed --example vector_compare -- <model> <audio> <ffmpeg.f32>

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(model), Some(audio), Some(reference)) = (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: vector_compare <model.onnx> <audio file> <ffmpeg.f32>");
        std::process::exit(2);
    };

    let mut embedder = embed::Embedder::open(&model).expect("load model");

    let ours = embed::decode::window_from_file(std::path::Path::new(&audio), 0.0, 30.0)
        .expect("rust decode");
    let raw = std::fs::read(&reference).expect("read reference");
    let theirs: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let a = embedder.vectors(&ours).expect("embed rust");
    let b = embedder.vectors(&theirs).expect("embed ffmpeg");

    println!(
        "style     cosine {:.8}",
        embed::similarity(&a.style, &b.style)
    );
    println!(
        "embedding cosine {:.8}",
        embed::similarity(&a.embedding, &b.embedding)
    );

    // For scale: how close is this to two genuinely different tracks? On the
    // listener's own favourites, style distances ranged 0.489 to 0.905.
    let worst = a
        .style
        .iter()
        .zip(&b.style)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    println!("largest single-dimension difference: {worst:.2e}");
}
