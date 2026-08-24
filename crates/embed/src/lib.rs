//! On-device audio similarity: mel spectrogram, Discogs-EffNet inference, and
//! the vectors a recommendation can be built on.
//!
//! The model weights are CC BY-NC-SA and are never bundled — see [`model`].

pub mod mel;
pub mod model;

#[cfg(feature = "onnx")]
pub mod session;
#[cfg(feature = "onnx")]
pub use session::Embedder;

pub use model::{Vectors, similarity};
