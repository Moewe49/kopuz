//! Write the test signal and its mel spectrogram as raw f32 for the Python
//! reference to check against. The port is only trustworthy if the numbers
//! match; "the output looks like a spectrogram" is not a test.
//!
//! Run: cargo run -p embed --example mel_dump -- <out_dir>

use embed::mel::{Mel, N_MELS, SAMPLE_RATE};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let n = SAMPLE_RATE * 3;
    let signal: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            0.5 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                + 0.3 * (2.0 * std::f32::consts::PI * 1750.0 * t).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 6300.0 * t).sin()
        })
        .collect();

    let spec = Mel::new().spectrogram(&signal);
    let flat: Vec<f32> = spec.iter().flat_map(|r| r.iter().copied()).collect();

    let bytes = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
    std::fs::write(format!("{dir}/signal.f32"), bytes(&signal)).unwrap();
    std::fs::write(format!("{dir}/mel_rust.f32"), bytes(&flat)).unwrap();
    println!("{} frames x {N_MELS} bands", spec.len());
}
