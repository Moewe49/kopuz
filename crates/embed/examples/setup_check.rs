//! Fetch the runtime and the model the way the app will, and prove the result
//! actually runs an inference.
//!
//! Run: cargo run -p embed --release --example setup_check

use std::sync::{Arc, Mutex};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("install dir: {}", embed::setup::install_dir().display());
    println!("already installed: {}", embed::setup::is_installed());

    let progress = Arc::new(Mutex::new(embed::setup::SetupProgress::default()));
    let watch = progress.clone();
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let p = watch.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if p.done || p.error.is_some() {
                return;
            }
            if let Some(step) = p.step {
                match p.fraction() {
                    Some(f) => println!("  {} {:.0}%", step.as_str(), f * 100.0),
                    None => println!("  {} {} bytes", step.as_str(), p.bytes),
                }
            }
        }
    });

    let started = std::time::Instant::now();
    match embed::setup::ensure(progress.clone()).await {
        Ok(()) => println!("ready in {:.1}s", started.elapsed().as_secs_f32()),
        Err(e) => {
            eprintln!("setup failed: {e}");
            std::process::exit(1);
        }
    }
    ticker.abort();

    // The point is not that two files exist, it is that they work together.
    embed::session::use_runtime(embed::setup::runtime_path()).expect("load runtime");
    let mut embedder = embed::Embedder::open(embed::setup::model_path()).expect("open model");
    let silence: Vec<f32> = (0..16_000 * 3)
        .map(|i| (i as f32 * 0.01).sin() * 0.2)
        .collect();
    let v = embedder.vectors(&silence).expect("embed");
    println!(
        "inference ok — style {} dims, embedding {} dims",
        v.style.len(),
        v.embedding.len()
    );
}
