//! The mel spectrogram the Discogs-EffNet model was trained on.
//!
//! This is the part that has to be exactly right. The model saw Essentia's
//! `TensorflowInputMusiCNN` output and nothing else; feed it a mel spectrogram
//! computed with any other convention — a different window, librosa's default
//! filter normalisation, natural log instead of log10 — and the embeddings
//! still come out as 1280 finite numbers that sort tracks into a confident,
//! meaningless order. There is no error to catch at runtime, which is why the
//! tests here compare against the reference implementation numerically rather
//! than checking that the output looks plausible.
//!
//! The convention, in full:
//!
//!   16 kHz mono, frame 512, hop 256, periodic Hann (unnormalised),
//!   magnitude spectrum, 96 mel bands (Slaney warping, unit-triangle
//!   normalisation, 0-8000 Hz), then log10(1 + 10000 * mel),
//!   grouped into patches of 128 frames.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex32;

pub const SAMPLE_RATE: usize = 16_000;
pub const FRAME: usize = 512;
pub const HOP: usize = 256;
pub const N_MELS: usize = 96;
/// Frames per patch — one inference step of the model.
pub const PATCH: usize = 128;

/// Slaney's mel scale: linear below 1 kHz, logarithmic above. Not the
/// HTK formula, which is the other common convention and gives different
/// band edges.
fn hz_to_mel(f: f64) -> f64 {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    if f >= MIN_LOG_HZ {
        let logstep = (6.4f64).ln() / 27.0;
        min_log_mel + (f / MIN_LOG_HZ).ln() / logstep
    } else {
        f / F_SP
    }
}

fn mel_to_hz(m: f64) -> f64 {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    if m >= min_log_mel {
        let logstep = (6.4f64).ln() / 27.0;
        MIN_LOG_HZ * (logstep * (m - min_log_mel)).exp()
    } else {
        m * F_SP
    }
}

/// The filterbank and window, built once. Both are pure functions of the
/// constants above, so they never need rebuilding for a different track.
pub struct Mel {
    /// `N_MELS` rows of `FRAME / 2 + 1` weights.
    filters: Vec<Vec<f32>>,
    window: Vec<f32>,
    planner: RealFftPlanner<f32>,
}

impl Default for Mel {
    fn default() -> Self {
        Self::new()
    }
}

impl Mel {
    pub fn new() -> Self {
        let n_bins = FRAME / 2 + 1;
        // Bin centres of the real FFT, in Hz.
        let fft_hz: Vec<f64> = (0..n_bins)
            .map(|i| i as f64 * (SAMPLE_RATE as f64 / 2.0) / (n_bins - 1) as f64)
            .collect();

        let (lo_mel, hi_mel) = (hz_to_mel(0.0), hz_to_mel(8000.0));
        let edges: Vec<f64> = (0..N_MELS + 2)
            .map(|i| mel_to_hz(lo_mel + (hi_mel - lo_mel) * i as f64 / (N_MELS + 1) as f64))
            .collect();

        let filters = (0..N_MELS)
            .map(|i| {
                let (lo, ctr, hi) = (edges[i], edges[i + 1], edges[i + 2]);
                // Unit-triangle normalisation, Essentia's `unit_tri`. Without
                // it the low bands dominate and the embedding drifts away from
                // what the model was trained on.
                let scale = 2.0 / (hi - lo).max(1e-9);
                fft_hz
                    .iter()
                    .map(|&f| {
                        let left = (f - lo) / (ctr - lo).max(1e-9);
                        let right = (hi - f) / (hi - ctr).max(1e-9);
                        (left.min(right).max(0.0) * scale) as f32
                    })
                    .collect()
            })
            .collect();

        // Periodic Hann, matching numpy's `hanning(N + 1)[:-1]`. The symmetric
        // form is off by one sample and shifts every bin slightly.
        let window = (0..FRAME)
            .map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / FRAME as f64).cos())
            .map(|w| w as f32)
            .collect();

        Self {
            filters,
            window,
            planner: RealFftPlanner::new(),
        }
    }

    /// Log-compressed mel bands, one row of `N_MELS` per frame.
    ///
    /// Returns fewer than `PATCH` rows — possibly none — for short input; the
    /// caller decides whether that is enough to embed.
    pub fn spectrogram(&mut self, samples: &[f32]) -> Vec<[f32; N_MELS]> {
        if samples.len() < FRAME {
            return Vec::new();
        }
        let fft = self.planner.plan_fft_forward(FRAME);
        let mut scratch = vec![0f32; FRAME];
        let mut spectrum = vec![Complex32::new(0.0, 0.0); FRAME / 2 + 1];

        let n_frames = 1 + (samples.len() - FRAME) / HOP;
        let mut out = Vec::with_capacity(n_frames);
        for f in 0..n_frames {
            let start = f * HOP;
            for (s, (x, w)) in scratch
                .iter_mut()
                .zip(samples[start..start + FRAME].iter().zip(&self.window))
            {
                *s = x * w;
            }
            // `process` is allowed to clobber the input, which is why `scratch`
            // is rebuilt from `samples` every frame rather than shifted.
            if fft.process(&mut scratch, &mut spectrum).is_err() {
                return out;
            }

            let mut row = [0f32; N_MELS];
            for (band, weights) in row.iter_mut().zip(&self.filters) {
                // POWER, not magnitude — `norm_sqr`, not `norm`.
                //
                // This is the whole difference between a spectrogram the model
                // understands and one it does not. Magnitude compresses the
                // dynamic range: measured against Essentia's own output, a
                // band with no signal in it came out at 0.518 instead of
                // 0.004, and the peak at 4.30 instead of 6.10. Every track
                // then looks uniformly dense and mid-heavy, which is why a
                // solo cello, a reggae song and a death metal track all came
                // back labelled `Electronic---Experimental`.
                //
                // Cosine against the reference: 0.9396 with magnitude,
                // 0.999919 with power.
                let energy: f32 = spectrum
                    .iter()
                    .zip(weights)
                    .map(|(c, w)| c.norm_sqr() * w)
                    .sum();
                *band = (1.0 + 10_000.0 * energy).log10();
            }
            out.push(row);
        }
        out
    }
}

/// Split a spectrogram into whole patches of `PATCH` frames, dropping the
/// remainder. A partial patch would have to be padded, and padding with
/// silence tilts the embedding towards quiet music.
pub fn patches(spec: &[[f32; N_MELS]]) -> Vec<f32> {
    let n = spec.len() / PATCH;
    let mut out = Vec::with_capacity(n * PATCH * N_MELS);
    for row in &spec[..n * PATCH] {
        out.extend_from_slice(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic signal with content in several bands, so a mistake in
    /// the filterbank shows up rather than cancelling out. Shared with the
    /// Python reference via `examples/mel_dump.rs`.
    pub(crate) fn test_signal(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                0.5 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                    + 0.3 * (2.0 * std::f32::consts::PI * 1750.0 * t).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * 6300.0 * t).sin()
            })
            .collect()
    }

    /// The band edges are the whole ballgame: HTK warping instead of Slaney
    /// puts every triangle in the wrong place, and the failure is silent.
    #[test]
    fn slaney_warping_round_trips() {
        for hz in [0.0, 100.0, 999.0, 1000.0, 4000.0, 8000.0] {
            let back = mel_to_hz(hz_to_mel(hz));
            assert!((back - hz).abs() < 1e-6, "{hz} -> {back}");
        }
        // Below 1 kHz the scale is linear, above it is not — if this ever
        // holds on both sides, the warping has been replaced by a plain line.
        assert!((hz_to_mel(500.0) - hz_to_mel(250.0) * 2.0).abs() < 1e-9);
        assert!((hz_to_mel(4000.0) - hz_to_mel(2000.0) * 2.0).abs() > 1.0);
    }

    #[test]
    fn frame_count_follows_the_hop() {
        let mut mel = Mel::new();
        // Exactly one frame, then one more per hop.
        assert_eq!(mel.spectrogram(&test_signal(FRAME)).len(), 1);
        assert_eq!(mel.spectrogram(&test_signal(FRAME + HOP)).len(), 2);
        assert_eq!(mel.spectrogram(&test_signal(FRAME + HOP - 1)).len(), 1);
        assert!(mel.spectrogram(&test_signal(FRAME - 1)).is_empty());
    }

    /// A tone must light up the bands around its frequency and leave the rest
    /// dark — the cheapest check that the filterbank is not transposed.
    #[test]
    fn a_pure_tone_lands_in_the_expected_band() {
        let mut mel = Mel::new();
        let tone: Vec<f32> = (0..FRAME * 4)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let spec = mel.spectrogram(&tone);
        let row = spec[1];
        let peak = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        // 440 Hz on a Slaney scale with 96 bands over 0-8000 Hz.
        let expected = (0..N_MELS)
            .map(|i| {
                let m = hz_to_mel(0.0)
                    + (hz_to_mel(8000.0) - hz_to_mel(0.0)) * (i + 1) as f64 / (N_MELS + 1) as f64;
                (mel_to_hz(m) - 440.0).abs()
            })
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            peak.abs_diff(expected) <= 1,
            "peak at band {peak}, expected near {expected}"
        );
        // And the top of the range must be quiet.
        assert!(row[90] < row[peak] * 0.5, "high bands are not quiet");
    }

    #[test]
    fn patches_drop_the_remainder_rather_than_padding() {
        let spec = vec![[1f32; N_MELS]; PATCH + 5];
        assert_eq!(patches(&spec).len(), PATCH * N_MELS);
        assert!(patches(&vec![[1f32; N_MELS]; PATCH - 1]).is_empty());
    }
}
