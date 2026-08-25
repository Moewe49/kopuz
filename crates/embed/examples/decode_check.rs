//! Decode a file with the Rust path and compare it against an ffmpeg-produced
//! reference of the same window.
//!
//! The embeddings only mean anything if the two agree, and the ways they can
//! disagree are all silent — a wrong downmix, an aliased decimation, a codec
//! that quietly refuses. So this measures rather than inspects.
//!
//! Run: cargo run -p embed --example decode_check -- <audio file> <ref.f32>

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(file), Some(reference)) = (args.next(), args.next()) else {
        eprintln!("usage: decode_check <audio file> <reference.f32>");
        std::process::exit(2);
    };

    let ours = match embed::decode::window_from_file(std::path::Path::new(&file), 0.0, 30.0) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rust decode failed: {e}");
            std::process::exit(1);
        }
    };
    let raw = std::fs::read(&reference).expect("read reference");
    let theirs: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    println!(
        "rust   {} samples ({:.2}s)",
        ours.len(),
        ours.len() as f32 / 16000.0
    );
    println!(
        "ffmpeg {} samples ({:.2}s)",
        theirs.len(),
        theirs.len() as f32 / 16000.0
    );

    // Compared over the overlap: a decoder may start a few frames earlier or
    // later than another, and that is not the failure being looked for.
    let n = ours.len().min(theirs.len());
    if n == 0 {
        eprintln!("nothing to compare");
        std::process::exit(1);
    }
    let dot: f64 = (0..n).map(|i| ours[i] as f64 * theirs[i] as f64).sum();
    let na: f64 = (0..n).map(|i| (ours[i] as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = (0..n)
        .map(|i| (theirs[i] as f64).powi(2))
        .sum::<f64>()
        .sqrt();
    println!(
        "waveform cosine over {n} samples: {:.6}",
        dot / (na * nb).max(1e-12)
    );
    println!(
        "rms  rust {:.5}  ffmpeg {:.5}",
        na / (n as f64).sqrt(),
        nb / (n as f64).sqrt()
    );
}
