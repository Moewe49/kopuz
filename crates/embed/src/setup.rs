//! Fetching the two files audio analysis needs, on the listener's say-so.
//!
//! Neither is bundled, for different reasons. The model's licence forbids it
//! (CC BY-NC-SA), and linking the runtime was measured to add 17.6 MB to the
//! binary — see [`crate::runtime`]. So both arrive here, once, when someone
//! turns the feature on.
//!
//! # Why this is not automatic the way ffmpeg is
//!
//! The app already fetches a ~90 MB ffmpeg on first run without asking, and
//! that is defensible: ffmpeg is LGPL, universally redistributed, and without
//! it downloads do not work at all. This is a different case on all three
//! counts. The model is licensed non-commercially, which is not something to
//! put on someone's disk by surprise; the pair is ~96 MB; and nothing breaks
//! without them — the mixes shelf simply keeps using the radio path.
//!
//! So the caller is expected to have asked first. Nothing here checks that,
//! but nothing here should be called on its own initiative either.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Which file is being fetched, for a caller that wants to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Runtime,
    Model,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Runtime => "runtime",
            Step::Model => "model",
        }
    }
}

/// How far along, for a progress bar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SetupProgress {
    pub step: Option<Step>,
    pub bytes: u64,
    /// `None` when the server did not send a length.
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
}

impl SetupProgress {
    /// 0.0 to 1.0, or `None` when the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.bytes as f32 / t as f32).min(1.0))
    }
}

/// Beside ffmpeg and yt-dlp, which the app already manages.
pub fn install_dir() -> PathBuf {
    let base = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("bin");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn runtime_path() -> PathBuf {
    install_dir().join(crate::runtime::library_name())
}

pub fn model_path() -> PathBuf {
    install_dir().join("discogs-effnet.onnx")
}

/// Everything the analysis job needs, with the vectors kept in the config
/// directory rather than here.
///
/// Config, not cache: vectors cost a paced network round-trip each to rebuild,
/// so losing them to a cache clear would silently undo hours of background
/// work. The two downloads below *are* cache-like — they can be refetched
/// unattended — but they live beside the other managed binaries for
/// consistency.
pub fn paths(config_dir: &Path) -> crate::job::Paths {
    crate::job::Paths {
        runtime: runtime_path(),
        model: model_path(),
        store: config_dir.join("style_vectors.bin"),
        labels: config_dir.join("style_meta.json"),
    }
}

/// Whether both files are already in place.
pub fn is_installed() -> bool {
    runtime_path().is_file() && model_path().is_file()
}

/// Roughly what turning the feature on transfers, for the confirmation the
/// caller shows. Measured on a real first run: a 78 MB runtime archive plus
/// the 17 MB model.
pub const APPROX_DOWNLOAD_MB: u64 = 96;

/// What is left on disk afterwards. Much less than the download, because the
/// runtime archive carries headers, import libraries and provider stubs, and
/// exactly one 15 MB library is kept out of it.
pub const APPROX_ON_DISK_MB: u64 = 32;

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        // Long, because these are large files on a CDN that sometimes crawls.
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| format!("client: {e}"))
}

/// Stream a URL to a file, reporting as it goes.
async fn download(
    url: &str,
    dest: &Path,
    step: Step,
    progress: &Arc<Mutex<SetupProgress>>,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut resp = client()?
        .get(url)
        .header("User-Agent", "kopuz")
        .send()
        .await
        .map_err(|e| format!("{}: {e}", step.as_str()))?
        .error_for_status()
        .map_err(|e| format!("{}: {e}", step.as_str()))?;

    let total = resp.content_length();
    {
        let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
        p.step = Some(step);
        p.bytes = 0;
        p.total = total;
    }

    // Written to a temp name and renamed, so an interrupted download is never
    // mistaken for a finished one on the next launch.
    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| format!("create {}: {e}", tmp.display()))?;
    let mut written: u64 = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("{} body: {e}", step.as_str()))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        written += chunk.len() as u64;
        progress.lock().unwrap_or_else(|e| e.into_inner()).bytes = written;
    }
    file.flush()
        .await
        .map_err(|e| format!("flush {}: {e}", tmp.display()))?;
    drop(file);
    tokio::fs::rename(&tmp, dest)
        .await
        .map_err(|e| format!("rename {}: {e}", dest.display()))
}

/// Pull the one shared library out of the runtime archive.
///
/// The archive is ~78 MB and holds headers, import libraries and provider
/// stubs; exactly one file is wanted. Path-traversal entries are rejected
/// rather than trusted — this writes into the user's data directory.
fn extract_runtime(archive: &Path, dest: &Path) -> Result<(), String> {
    let wanted = crate::runtime::library_name();
    let file = std::fs::File::open(archive).map_err(|e| format!("open archive: {e}"))?;

    let found = if archive.extension().is_some_and(|e| e == "zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;
        let mut out = None;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
            let Some(path) = entry.enclosed_name() else {
                continue;
            };
            if entry.is_file() && path.file_name().is_some_and(|n| n == wanted) {
                let mut bytes = Vec::new();
                std::io::copy(&mut entry, &mut bytes).map_err(|e| format!("zip read: {e}"))?;
                out = Some(bytes);
                break;
            }
        }
        out
    } else {
        let gz = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(gz);
        let mut out = None;
        for entry in tar.entries().map_err(|e| format!("read tar: {e}"))? {
            let mut entry = entry.map_err(|e| format!("tar entry: {e}"))?;
            let path = entry
                .path()
                .map_err(|e| format!("tar path: {e}"))?
                .to_path_buf();
            // The Linux archive ships the real library as a versioned name with
            // an unversioned symlink beside it; match either.
            let is_wanted = path.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n == wanted || n.starts_with(&format!("{wanted}."))
            });
            if is_wanted && entry.header().entry_type().is_file() {
                let mut bytes = Vec::new();
                std::io::copy(&mut entry, &mut bytes).map_err(|e| format!("tar read: {e}"))?;
                out = Some(bytes);
                break;
            }
        }
        out
    };

    let bytes = found.ok_or_else(|| format!("{wanted} not found inside the archive"))?;
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("rename {}: {e}", dest.display()))
}

/// Fetch whatever is missing. Safe to call again: anything already in place is
/// left alone.
///
/// Only call this after the listener has asked for it — see the note at the
/// top of the module.
pub async fn ensure(progress: Arc<Mutex<SetupProgress>>) -> Result<(), String> {
    let finish = |p: &Arc<Mutex<SetupProgress>>, err: Option<String>| {
        let mut g = p.lock().unwrap_or_else(|e| e.into_inner());
        g.done = err.is_none();
        g.error = err;
    };

    // ---- runtime -------------------------------------------------------
    let runtime = runtime_path();
    if !runtime.is_file() {
        let Some(url) = crate::runtime::download_url() else {
            let msg = "no ONNX Runtime build is published for this platform".to_string();
            finish(&progress, Some(msg.clone()));
            return Err(msg);
        };
        let archive = install_dir().join(if url.ends_with(".zip") {
            "onnxruntime-download.zip"
        } else {
            "onnxruntime-download.tgz"
        });
        if let Err(e) = download(&url, &archive, Step::Runtime, &progress).await {
            finish(&progress, Some(e.clone()));
            return Err(e);
        }
        // Archive parsing is blocking CPU work and must not sit on the
        // executor that also drives the UI.
        let archive_for_task = archive.clone();
        let runtime_for_task = runtime.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            extract_runtime(&archive_for_task, &runtime_for_task)
        })
        .await
        .map_err(|e| format!("extract task: {e}"));
        let _ = tokio::fs::remove_file(&archive).await;
        match extracted {
            Ok(Ok(())) => {}
            Ok(Err(e)) | Err(e) => {
                finish(&progress, Some(e.clone()));
                return Err(e);
            }
        }
    }

    // ---- model ---------------------------------------------------------
    let model = model_path();
    if !model.is_file() {
        if let Err(e) = download(crate::model::MODEL_URL, &model, Step::Model, &progress).await {
            finish(&progress, Some(e.clone()));
            return Err(e);
        }
        // Verified before it is ever loaded. A truncated download or a served
        // error page would otherwise become an inference session producing
        // confident nonsense, which is exactly the failure mode this whole
        // pipeline is built to avoid.
        let model_for_task = model.clone();
        let ok = tokio::task::spawn_blocking(move || {
            std::fs::read(&model_for_task)
                .map(|bytes| crate::model::verify(&bytes))
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false);
        if !ok {
            let _ = tokio::fs::remove_file(&model).await;
            let msg = "the downloaded model did not match its checksum".to_string();
            finish(&progress, Some(msg.clone()));
            return Err(msg);
        }
    }

    finish(&progress, None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_files_land_beside_the_other_managed_binaries() {
        assert_eq!(runtime_path().parent(), model_path().parent());
        assert!(runtime_path().ends_with(crate::runtime::library_name()));
        assert!(model_path().extension().is_some_and(|e| e == "onnx"));
    }

    /// Vectors are expensive to rebuild — a paced network round-trip each — so
    /// they must not sit where a cache clear can take them.
    #[test]
    fn vectors_are_kept_apart_from_the_downloads() {
        let config = Path::new("/somewhere/config");
        let p = paths(config);
        assert!(p.store.starts_with(config));
        assert!(p.labels.starts_with(config));
        assert!(!p.model.starts_with(config));
    }

    #[test]
    fn progress_reports_a_fraction_only_when_the_total_is_known() {
        let mut p = SetupProgress {
            bytes: 50,
            total: Some(100),
            ..SetupProgress::default()
        };
        assert_eq!(p.fraction(), Some(0.5));
        p.total = None;
        assert_eq!(p.fraction(), None);
        // A server that lies about the length must not produce 3000%.
        p.total = Some(10);
        assert_eq!(p.fraction(), Some(1.0));
    }
}
