//! Embedding a library, a few tracks at a time.
//!
//! Analysing a track costs a stream resolve, a partial download and an
//! inference — a second or two each, and the network part is the expensive
//! half. A listener with a few hundred tracks therefore cannot be made to wait
//! for a progress bar; this runs in the background, in small batches, and
//! remembers what it has already done.
//!
//! # Pacing is not politeness here
//!
//! A burst of InnerTube calls is what trips YouTube's bot gate. The Android
//! engine measured it the hard way: roughly a hundred resolves in five seconds
//! got *every one of them* answered with "Sign in to confirm you're not a
//! bot", which breaks playback, not just analysis. So this is deliberately
//! slower than it could be, and it is why a batch has a budget rather than
//! running until it is done.
//!
//! # Only what is needed gets downloaded
//!
//! [`utils::range_source::RangeStreamSource`] serves a seekable HTTP file in
//! windows, so seeking to the analysis offset and reading thirty seconds
//! fetches roughly that much rather than the whole file. Across a few hundred
//! tracks that is the difference between tens of megabytes and a gigabyte.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use reader::vectors::VectorStore;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;

use crate::model::N_STYLES;

/// Where the analysis window starts, and how long it is.
///
/// Inside the track on purpose: an intro is not what a track sounds like.
/// Thirty seconds is what the listening test that validated this approach
/// used.
const WINDOW_START: f64 = 45.0;
const WINDOW_SECS: f64 = 30.0;

/// Gap between tracks. See the note above — this is the bot gate, not
/// courtesy.
const SPACING: std::time::Duration = std::time::Duration::from_millis(800);

/// Files the job needs on disk. All three are downloaded or built by the app;
/// none is bundled.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The ONNX Runtime shared library — see [`crate::runtime`].
    pub runtime: PathBuf,
    /// The model weights — see [`crate::model`].
    pub model: PathBuf,
    /// Where vectors accumulate between runs.
    pub store: PathBuf,
    /// Artist, title and cover URL per id, beside the vectors.
    ///
    /// A vector store keyed by video id can rank tracks but cannot name them,
    /// and everything built on it — a mix tile, a suggestion list — has to
    /// show something a person recognises — and a tile with no artwork reads as
    /// broken. The player response that yields the stream URL carries all
    /// three, so this costs no extra request.
    pub labels: PathBuf,
}

/// What one batch achieved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    pub embedded: usize,
    /// Tracks that could not be analysed. A track that fails once is not
    /// retried inside the same batch, but nothing marks it permanently bad:
    /// most failures here are an expired URL or a transient refusal.
    pub failed: usize,
    /// Still without a vector after this batch.
    pub remaining: usize,
    /// Total vectors held, including earlier runs.
    pub total: usize,
    /// Tracks that already had a vector but were missing their artwork.
    pub relabelled: usize,
}

/// `Read + Seek` as something symphonia will accept.
///
/// The player has the same wrapper; duplicating twenty lines is better here
/// than depending on the whole playback crate, which drags in the audio device
/// layer for a background job that never plays anything.
struct SeekableSource<T: Read + Seek + Send + Sync> {
    inner: T,
    len: Option<u64>,
}

impl<T: Read + Seek + Send + Sync> Read for SeekableSource<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<T: Read + Seek + Send + Sync> Seek for SeekableSource<T> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<T: Read + Seek + Send + Sync> MediaSource for SeekableSource<T> {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

/// Analyse one stream URL. Blocking: it downloads and decodes.
fn analyse_url(embedder: &mut crate::Embedder, url: &str) -> Result<Vec<f32>, String> {
    let source = utils::range_source::RangeStreamSource::new(url.to_string(), None, None)
        .map_err(|e| format!("open stream: {e}"))?;
    let len = source.total_size();
    let media = SeekableSource {
        inner: source,
        len: Some(len),
    };
    // No extension to hint with — a YouTube stream URL carries none, and
    // symphonia's probe reads the container's own magic anyway.
    let samples = crate::decode::window(Box::new(media), &Hint::new(), WINDOW_START, WINDOW_SECS)?;
    Ok(embedder.vectors(&samples).map_err(|e| e.to_string())?.style)
}

/// Embed up to `budget` of the given tracks that have no vector yet.
///
/// `ids` are YouTube video ids in priority order — most-played first is the
/// useful order, since those are what the mixes are built from. Ids that
/// already have a vector are skipped without a request, so calling this
/// repeatedly walks through a library rather than redoing it.
pub async fn analyse(ids: &[String], paths: &Paths, budget: usize) -> Result<Progress, String> {
    // The store is read before anything else, so a library that is already
    // analysed — the normal case after the first day — costs a file read and
    // nothing more. Loading the runtime first would spin up an inference
    // engine on every launch to discover there was no work.
    let mut store =
        VectorStore::load(&paths.store, N_STYLES).map_err(|e| format!("vector store: {e}"))?;
    let mut labels = server::mixes::load_labels(&paths.labels);
    let todo: Vec<&String> = ids.iter().filter(|id| !store.contains(id)).collect();
    let mut progress = Progress {
        remaining: todo.len(),
        total: store.len(),
        ..Progress::default()
    };
    // Tracks analysed before covers were recorded have a vector but no
    // artwork, and nothing would ever revisit them — they already have what
    // the job checks for. A metadata-only resolve fixes them: no download, no
    // inference, just the player response.
    let coverless: Vec<String> = store
        .ids()
        .filter(|id| {
            labels
                .get(*id)
                .map(|(_, _, cover)| cover.is_empty())
                .unwrap_or(true)
        })
        .map(str::to_string)
        .take(budget)
        .collect();
    for id in &coverless {
        if let Some((artist, title, cover, _)) = resolve(id).await
            && !cover.is_empty()
        {
            labels.insert(id.clone(), (artist, title, cover));
            progress.relabelled += 1;
        }
        tokio::time::sleep(SPACING).await;
    }

    if todo.is_empty() {
        // Nothing left to analyse, but the backfill above may still have
        // learned something worth keeping.
        write_labels(&paths.labels, &labels);
        return Ok(progress);
    }

    crate::session::use_runtime(&paths.runtime).map_err(|e| e.to_string())?;
    let mut embedder = crate::Embedder::open(&paths.model).map_err(|e| e.to_string())?;

    for id in todo.into_iter().take(budget) {
        let Some((artist, title, cover, url)) = resolve(id).await else {
            progress.failed += 1;
            continue;
        };
        // Downloading and decoding are both blocking and neither belongs on
        // the async executor, which on desktop also drives the UI.
        let id_owned = id.clone();
        let result = tokio::task::block_in_place(|| analyse_url(&mut embedder, &url));
        match result {
            Ok(style) => {
                if store.insert(id_owned.clone(), style).is_ok() {
                    labels.insert(id_owned, (artist, title, cover));
                    progress.embedded += 1;
                    progress.remaining = progress.remaining.saturating_sub(1);
                }
            }
            Err(e) => {
                progress.failed += 1;
                tracing::debug!("embed {id}: {e}");
            }
        }
        tokio::time::sleep(SPACING).await;
    }

    // Saved after the batch rather than after each track: a batch is seconds,
    // and rewriting the whole store per track would be the most expensive part
    // of it once a library is large.
    store
        .save(&paths.store)
        .map_err(|e| format!("save vectors: {e}"))?;
    write_labels(&paths.labels, &labels);
    progress.total = store.len();
    Ok(progress)
}

/// Artist, title, cover URL and the highest-bitrate non-dub audio URL.
async fn resolve(video_id: &str) -> Option<(String, String, String, String)> {
    use server::ytmusic::clients::VISIONOS;
    use server::ytmusic::innertube::{self, PlayerExtras, visitor_id};

    let visitor = visitor_id(None).await.ok()?;
    let json = innertube::player(
        VISIONOS,
        video_id,
        None,
        PlayerExtras {
            content_pot: None,
            visitor_data: Some(&visitor),
            signature_timestamp: None,
        },
    )
    .await
    .ok()?;
    let artist = json
        .pointer("/videoDetails/author")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let title = json
        .pointer("/videoDetails/title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    // Largest thumbnail, matching what the search path stores. Without it a
    // generated mix renders as grey placeholders.
    let cover = json
        .pointer("/videoDetails/thumbnail/thumbnails")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .max_by_key(|t| t.get("width").and_then(|w| w.as_u64()).unwrap_or(0))
        })
        .and_then(|t| t.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let url = json
        .pointer("/streamingData/adaptiveFormats")
        .and_then(|v| v.as_array())?
        .iter()
        .filter(|f| {
            f.get("mimeType")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m.starts_with("audio/"))
                && f.get("url").is_some()
                // A dubbed track is a different performance in a different
                // language; embedding it would describe the dub, not the song.
                && !f.get("audioTrack").is_some_and(|t| {
                    !t.get("audioIsDefault")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
        })
        .max_by_key(|f| f.get("bitrate").and_then(|v| v.as_u64()).unwrap_or(0))
        .and_then(|f| f.get("url"))
        .and_then(|v| v.as_str())?
        .to_string();
    Some((artist, title, cover, url))
}

fn write_labels(path: &std::path::Path, labels: &server::mixes::Labels) {
    if let Ok(json) = serde_json::to_string(labels) {
        let _ = std::fs::write(path, json);
    }
}

/// Whether everything the job needs is present.
///
/// Both files are downloaded rather than bundled — the model because its
/// licence forbids bundling, the runtime because linking it costs 17.6 MB in
/// the binary — so "not analysed yet" and "not set up yet" are different
/// states the caller has to be able to tell apart.
pub fn is_ready(paths: &Paths) -> bool {
    paths.runtime.exists() && paths.model.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_needs_both_files() {
        let missing = Paths {
            runtime: PathBuf::from("/no/such/runtime"),
            model: PathBuf::from("/no/such/model"),
            store: PathBuf::from("/tmp/store.bin"),
            labels: PathBuf::from("/tmp/labels.json"),
        };
        assert!(!is_ready(&missing));
    }

    /// A library that is fully analysed must cost nothing: no runtime load, no
    /// request, no inference. This is the common case after the first day.
    #[tokio::test]
    async fn an_already_analysed_library_makes_no_requests() {
        let dir = std::env::temp_dir().join("kopuz-embedjob-test");
        let _ = std::fs::create_dir_all(&dir);
        let store_path = dir.join("vectors.bin");
        let _ = std::fs::remove_file(&store_path);

        let mut store = VectorStore::new(N_STYLES);
        store.insert("known", vec![0.1; N_STYLES]).unwrap();
        store.save(&store_path).unwrap();

        let paths = Paths {
            // Deliberately absent: reaching them would mean the early return
            // did not happen.
            runtime: PathBuf::from("/no/such/runtime"),
            model: PathBuf::from("/no/such/model"),
            store: store_path.clone(),
            labels: dir.join("labels.json"),
        };
        let progress = analyse(&["known".to_string()], &paths, 5)
            .await
            .expect("a fully-analysed library must not need the runtime at all");
        assert_eq!(progress.embedded, 0);
        assert_eq!(progress.remaining, 0);
        assert_eq!(progress.total, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
