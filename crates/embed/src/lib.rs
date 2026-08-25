//! On-device audio similarity: mel spectrogram, Discogs-EffNet inference, and
//! the vectors a recommendation can be built on.
//!
//! The model weights are CC BY-NC-SA and are never bundled — see [`model`].

pub mod decode;
#[cfg(all(feature = "job", feature = "onnx"))]
pub mod job;
pub mod mel;
pub mod model;
pub mod resample;
pub mod runtime;

#[cfg(feature = "onnx")]
pub mod session;
#[cfg(feature = "onnx")]
pub use session::Embedder;

pub use model::{Vectors, similarity};
