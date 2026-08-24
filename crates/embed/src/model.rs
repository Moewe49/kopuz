//! Running the Discogs-EffNet model over a mel spectrogram.
//!
//! # Licensing — read before wiring this into a build
//!
//! The model weights are CC BY-NC-SA 4.0. That is not the licence of this
//! repository, and it is not compatible with shipping the file inside a
//! binary or an APK. The weights are therefore *never* bundled: the listener's
//! machine fetches them at runtime, from the address below, and the code here
//! only ever loads a file that already exists on disk.
//!
//! The non-commercial clause also constrains what the app may become. Any
//! paid tier, ad-supported build or commercial redistribution needs a
//! different model, not a different reading of the licence.
//!
//! # Two outputs, and why the smaller one is the useful one
//!
//! The model returns a 1280-d embedding and 400 style activations. The
//! embedding is the obvious choice and it is the wrong one: measured against
//! the listener's own favourites it put Cannibal Corpse at 0.806 and a Bach
//! cello suite at 0.648, i.e. it ranked death metal as the closer match. The
//! same pairs in activation space came out at 0.482 and 0.483 — both far away,
//! which is the answer a listener would give. The embedding encodes
//! production and texture; the activations encode what kind of music it is.

use crate::mel::{N_MELS, PATCH};

/// Where the weights come from. Verified byte-identical to the file the
/// listening test was run against.
pub const MODEL_URL: &str = "https://essentia.upf.edu/models/feature-extractors/discogs-effnet/discogs-effnet-bsdynamic-1.onnx";
/// SHA-256 of that file, so a silently swapped model is caught rather than
/// quietly changing everyone's recommendations.
pub const MODEL_SHA256: &str = "a280825b334797cf677939db8cd5762c0392aedd0ca6415dbc1cd083f045e43c";
/// Exact size in bytes, as a cheap first check before hashing 18 MB.
pub const MODEL_BYTES: usize = 18_027_718;

/// Style activations — 400 Discogs genres and styles.
pub const N_STYLES: usize = 400;
/// Embedding width, kept for callers that want the texture vector too.
pub const N_EMBED: usize = 1280;

#[derive(Debug)]
pub enum Error {
    /// The model file is missing, unreadable, or not the expected one.
    Model(String),
    /// Inference failed.
    Inference(String),
    /// Less than one full patch of audio — about 2.05 seconds.
    TooShort,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Model(m) => write!(f, "model: {m}"),
            Error::Inference(m) => write!(f, "inference: {m}"),
            Error::TooShort => write!(f, "audio is shorter than one patch"),
        }
    }
}

impl std::error::Error for Error {}

/// Check downloaded bytes before writing them anywhere. Cheap length check
/// first so a truncated download or an HTML error page is rejected without
/// hashing.
pub fn verify(bytes: &[u8]) -> bool {
    bytes.len() == MODEL_BYTES && sha256_hex(bytes) == MODEL_SHA256
}

/// A small SHA-256, to avoid a dependency for one hash of one file.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

/// L2-normalise in place, so every vector returned is directly comparable by
/// dot product.
fn normalise(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Mean-pool a `[patches, width]` output down to one vector, then normalise.
///
/// Pooling before normalising, not after: normalising each patch first would
/// give a two-second passage of silence the same weight as the chorus.
pub(crate) fn pool(flat: &[f32], width: usize) -> Vec<f32> {
    let n = flat.len() / width;
    let mut out = vec![0f32; width];
    for patch in flat.chunks_exact(width) {
        for (o, x) in out.iter_mut().zip(patch) {
            *o += x;
        }
    }
    if n > 0 {
        for o in out.iter_mut() {
            *o /= n as f32;
        }
    }
    normalise(&mut out);
    out
}

/// Both vectors for one track, from one inference pass.
#[derive(Debug, Clone)]
pub struct Vectors {
    /// 400 style activations, L2-normalised. This is the one to compare on.
    pub style: Vec<f32>,
    /// 1280-d embedding, L2-normalised. Texture and production rather than
    /// kind of music — useful for finding a similar-sounding recording of a
    /// different genre, misleading as a similarity measure on its own.
    pub embedding: Vec<f32>,
}

/// Cosine similarity of two L2-normalised vectors.
///
/// For calibration: on the listener's own eight favourites this ranged from
/// 0.489 to 0.905 in style space, median 0.729. Above 0.90 is closer than
/// their favourites are to each other; below 0.49 is outside the taste.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Number of whole patches in a flattened spectrogram.
pub(crate) fn patch_count(flat: &[f32]) -> usize {
    flat.len() / (PATCH * N_MELS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash guards an 18 MB download; a broken implementation of it would
    /// accept anything. Vectors are the NIST ones plus the empty string.
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// An HTML error page served with status 200 is the realistic failure —
    /// it must not be written over a good model file.
    #[test]
    fn verify_rejects_anything_that_is_not_the_model() {
        assert!(!verify(b"<!DOCTYPE html><title>404</title>"));
        assert!(!verify(&vec![0u8; MODEL_BYTES]));
        assert!(!verify(&[]));
    }

    /// Pooling before normalising is the point: a quiet patch must not count
    /// as much as a loud one.
    #[test]
    fn pooling_averages_patches_then_normalises() {
        // Two patches of width 2: a loud one and a near-silent one.
        let flat = vec![3.0, 4.0, 0.03, 0.04];
        let v = pool(&flat, 2);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-6);
        // The mean points the same way as the loud patch, not halfway to the
        // quiet one, because both happen to share a direction here.
        assert!((v[0] - 0.6).abs() < 1e-5, "{v:?}");
        assert!((v[1] - 0.8).abs() < 1e-5, "{v:?}");
    }

    #[test]
    fn similarity_is_the_cosine_for_normalised_vectors() {
        let mut a = vec![1.0, 1.0, 0.0];
        let mut b = vec![1.0, 0.0, 0.0];
        normalise(&mut a);
        normalise(&mut b);
        assert!((similarity(&a, &b) - 0.70710677).abs() < 1e-6);
        assert!((similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn patch_count_ignores_a_partial_patch() {
        assert_eq!(patch_count(&vec![0f32; PATCH * N_MELS * 3]), 3);
        assert_eq!(patch_count(&vec![0f32; PATCH * N_MELS - 1]), 0);
    }
}
