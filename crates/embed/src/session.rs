//! The ONNX session itself, kept behind the `onnx` feature so the rest of the
//! crate stays free of the native runtime.

use crate::mel::{Mel, N_MELS, PATCH};
use crate::model::{Error, N_EMBED, N_STYLES, Vectors, patch_count, pool};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

/// A loaded model plus its preprocessing. Holding one of these is what makes
/// embedding a whole library affordable: the 18 MB of weights and the mel
/// filterbank are built once, not per track.
pub struct Embedder {
    session: Session,
    mel: Mel,
}

/// Point ort at an ONNX Runtime shared library on disk.
///
/// Must succeed before any [`Embedder`] is opened. The runtime is not linked
/// into the binary — see the note on the `ort` dependency for the measurement
/// behind that — so this is how it gets found.
///
/// Calling it twice is harmless: ort keeps the first environment, and a second
/// path is ignored rather than being an error.
pub fn use_runtime(path: impl AsRef<std::path::Path>) -> Result<(), Error> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(Error::Model(format!(
            "ONNX Runtime not found at {}",
            path.display()
        )));
    }
    // `commit` returns whether this call was the one that created the
    // environment, not whether it worked — a second call with a different path
    // simply returns false and keeps the first.
    let _first = ort::init_from(path.to_string_lossy().as_ref())
        .map_err(|e| Error::Model(format!("ONNX Runtime at {}: {e}", path.display())))?
        .commit();
    Ok(())
}

impl Embedder {
    /// Load weights from disk. The file is never bundled — see the licence
    /// note in [`crate::model`] — so this is always a path the caller
    /// downloaded and verified with [`crate::model::verify`] first.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(Error::Model(format!("{} does not exist", path.display())));
        }
        // Each step carries its own error type in ort, so they are converted
        // one at a time rather than chained.
        let builder = Session::builder().map_err(|e| Error::Model(e.to_string()))?;
        let mut builder = builder
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| Error::Model(e.to_string()))?;
        let session = builder
            .commit_from_file(path)
            .map_err(|e| Error::Model(e.to_string()))?;
        Ok(Self {
            session,
            mel: Mel::new(),
        })
    }

    /// Both vectors for one window of audio: 16 kHz mono, f32.
    ///
    /// Needs at least one full patch — about 2.05 seconds. Thirty seconds
    /// taken from the middle of a track is what the listening test used; less
    /// than that starts to pick up intros rather than the track.
    pub fn vectors(&mut self, samples: &[f32]) -> Result<Vectors, Error> {
        let spec = self.mel.spectrogram(samples);
        let flat = crate::mel::patches(&spec);
        let n = patch_count(&flat);
        if n == 0 {
            return Err(Error::TooShort);
        }

        let input = Tensor::from_array(([n, PATCH, N_MELS], flat))
            .map_err(|e| Error::Inference(e.to_string()))?;
        let outputs = self
            .session
            .run(ort::inputs!["melspectrogram" => input])
            .map_err(|e| Error::Inference(e.to_string()))?;

        let extract = |name: &str, width: usize| -> Result<Vec<f32>, Error> {
            let (_, data) = outputs[name]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Inference(format!("{name}: {e}")))?;
            Ok(pool(data, width))
        };

        Ok(Vectors {
            style: extract("activations", N_STYLES)?,
            embedding: extract("embeddings", N_EMBED)?,
        })
    }
}
