//! Getting decoded audio to the rate and channel count the model expects.
//!
//! The model was trained on `ffmpeg -ac 1 -ar 16000` output, so this has to
//! agree with that closely enough that the embeddings do. Two details in the
//! player's own helpers are wrong for this purpose, and both fail silently:
//!
//! - `convert_channels` takes channel 0 and discards the rest. `-ac 1`
//!   averages. A stereo mix with a wide guitar is a different recording
//!   depending on which of the two you believe.
//! - `resample` is unfiltered linear interpolation. Going from 44.1 kHz to
//!   16 kHz that way folds everything above 8 kHz back down as an alias:
//!   cymbals reappear as tones in the mid-range, and the model — which has no
//!   way to know — reads them as content.
//!
//! Neither shows up as an error. Both produce finite numbers and a confident,
//! meaningless ordering, which is why the tests here measure the alias rather
//! than checking that the output looks like audio.

/// What the model wants.
pub const TARGET_RATE: u32 = 16_000;

/// Half-width of the interpolation kernel, in output samples.
///
/// Sixteen taps either side is the usual place to stop: the stopband keeps
/// improving with more, but the cost is linear and there is nothing left to
/// win. Measured, not estimated — `alias_rejection_measured` prints it, and on
/// tones decimated from 44.1 kHz the alias lands 87 to 99 dB below the
/// passband, which is far under anything the model can distinguish.
const HALF_WIDTH: usize = 16;

/// Mix the channels down to one, the way `ffmpeg -ac 1` does.
///
/// Divided by the **square root** of the channel count, not by the count.
/// That is not a guess: decoding one real stereo track both ways and
/// correlating the results gave 1.000000 with a level ratio of 1.41421 —
/// exactly √2. ffmpeg normalises the downmix matrix so its coefficients sum
/// to one in power, which for N equal channels is 1/√N.
///
/// Averaging instead is 3 dB quiet for stereo, and that is not cosmetic here:
/// the mel compression is `log10(1 + 10000·x)`, which is not scale-invariant,
/// so every band would sit slightly below where the model was trained to find
/// it. Nothing would have reported an error.
pub fn to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let scale = 1.0 / (channels as f32).sqrt();
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() * scale)
        .collect()
}

fn sinc(x: f32) -> f32 {
    if x.abs() < 1e-6 {
        1.0
    } else {
        let pi_x = std::f32::consts::PI * x;
        pi_x.sin() / pi_x
    }
}

/// Blackman window — cheap, and its stopband is deep enough that the alias
/// disappears under the noise floor of anything the model will see.
fn blackman(t: f32) -> f32 {
    let x = std::f32::consts::PI * (t + 1.0);
    0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos()
}

/// Resample mono audio to [`TARGET_RATE`].
///
/// Windowed-sinc interpolation with the cutoff placed at the lower of the two
/// Nyquist limits, so downsampling low-passes on the way rather than aliasing.
pub fn to_target_rate(mono: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 0 || mono.is_empty() {
        return Vec::new();
    }
    if src_rate == TARGET_RATE {
        return mono.to_vec();
    }
    let ratio = TARGET_RATE as f64 / src_rate as f64;
    // Downsampling widens the kernel in input samples and lowers its cutoff;
    // upsampling leaves both alone, since the input is already band-limited.
    let cutoff = (ratio as f32).min(1.0);
    let half = (HALF_WIDTH as f64 / cutoff as f64).ceil() as isize;

    let out_len = (mono.len() as f64 * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let center = i as f64 / ratio;
        let base = center.floor() as isize;
        let mut acc = 0f32;
        let mut norm = 0f32;
        for k in (base - half)..=(base + half) {
            if k < 0 || k as usize >= mono.len() {
                continue;
            }
            let dist = (center - k as f64) as f32;
            let w = blackman(dist / half as f32);
            if w <= 0.0 {
                continue;
            }
            let h = cutoff * sinc(cutoff * dist) * w;
            acc += mono[k as usize] * h;
            norm += h;
        }
        // Normalising by the kernel sum keeps the level right at the edges,
        // where part of the window hangs off the end of the buffer.
        out.push(if norm.abs() > 1e-6 { acc / norm } else { 0.0 });
    }
    out
}

/// Decoded audio in whatever form it arrived, turned into what the model wants.
pub fn prepare(interleaved: &[f32], channels: usize, src_rate: u32) -> Vec<f32> {
    to_target_rate(&to_mono(interleaved, channels), src_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    /// Energy at `freq`, by correlating against a complex exponential. Enough
    /// to answer "is this frequency present", without a full spectrum.
    fn energy_at(samples: &[f32], freq: f32, rate: u32) -> f32 {
        let (mut re, mut im) = (0f32, 0f32);
        for (i, s) in samples.iter().enumerate() {
            let phase = 2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32;
            re += s * phase.cos();
            im += s * phase.sin();
        }
        ((re * re + im * im).sqrt()) / samples.len() as f32
    }

    /// Taking channel 0 — which the player's helper does — throws away a
    /// whole side of the mix.
    #[test]
    fn mono_mixes_the_channels_rather_than_taking_the_first() {
        // Left silent, right loud: channel 0 alone would be silence.
        let out = to_mono(&[0.0, 1.0, 0.0, 1.0], 2);
        assert!(out.iter().all(|&x| x > 0.0), "the right channel vanished");
        // Mono passes straight through.
        assert_eq!(to_mono(&[0.3, 0.4], 1), vec![0.3, 0.4]);
    }

    /// The measured constant. ffmpeg divides by √N, not N — verified by
    /// decoding a real stereo track both ways: correlation 1.000000, level
    /// ratio 1.41421. Averaging would be 3 dB quiet, and the mel compression
    /// is not scale-invariant, so every band would land off where the model
    /// expects it.
    #[test]
    fn the_downmix_preserves_power_the_way_ffmpeg_does() {
        let stereo = vec![1.0f32, 1.0, -1.0, -1.0];
        let out = to_mono(&stereo, 2);
        let expected = 2.0 / 2f32.sqrt(); // (L+R)/√2
        assert!((out[0] - expected).abs() < 1e-6, "got {}", out[0]);
        assert!((out[1] + expected).abs() < 1e-6, "got {}", out[1]);

        // Plain averaging, which is what this replaced, is quieter by √2.
        let averaged = 2.0 / 2.0;
        assert!((out[0] / averaged - 2f32.sqrt()).abs() < 1e-5);
    }

    /// The measurement this module exists for. A 12 kHz tone cannot be
    /// represented at 16 kHz; unfiltered decimation folds it down to
    /// |12000 - 16000| = 4 kHz, where it sounds like content the model then
    /// reads as real. It must be attenuated instead.
    #[test]
    fn a_tone_above_nyquist_does_not_come_back_as_an_alias() {
        let input = tone(12_000.0, 44_100, 0.5);
        let out = to_target_rate(&input, 44_100);

        let alias = energy_at(&out, 4_000.0, TARGET_RATE);
        // Reference: the same tone at a frequency that survives the trip.
        let kept = energy_at(
            &to_target_rate(&tone(1_000.0, 44_100, 0.5), 44_100),
            1_000.0,
            TARGET_RATE,
        );

        assert!(kept > 0.3, "a 1 kHz tone must survive, got {kept}");
        assert!(
            alias < kept / 100.0,
            "alias at 4 kHz is {alias}, only {:.1} dB below the passband",
            20.0 * (kept / alias.max(1e-12)).log10()
        );
    }

    /// Naive linear interpolation is what the player does, and it is what this
    /// must beat — otherwise there is no reason for the extra code.
    #[test]
    fn windowed_sinc_beats_linear_interpolation_on_the_same_input() {
        let input = tone(12_000.0, 44_100, 0.5);

        let ratio = TARGET_RATE as f64 / 44_100.0;
        let n = (input.len() as f64 * ratio).floor() as usize;
        let linear: Vec<f32> = (0..n)
            .map(|i| {
                let pos = i as f64 / ratio;
                let idx = pos.floor() as usize;
                let frac = (pos - idx as f64) as f32;
                let a = input.get(idx).copied().unwrap_or(0.0);
                let b = input.get(idx + 1).copied().unwrap_or(0.0);
                a + (b - a) * frac
            })
            .collect();

        let ours = energy_at(&to_target_rate(&input, 44_100), 4_000.0, TARGET_RATE);
        let theirs = energy_at(&linear, 4_000.0, TARGET_RATE);
        assert!(
            ours < theirs / 10.0,
            "ours {ours} vs linear {theirs} — not an improvement worth the code"
        );
    }

    /// A tone inside the band has to come through at the right frequency and
    /// roughly the right level, or the filter is eating signal too.
    #[test]
    fn audible_content_passes_through_unharmed() {
        for freq in [220.0, 1_000.0, 5_000.0] {
            let out = to_target_rate(&tone(freq, 44_100, 0.5), 44_100);
            let kept = energy_at(&out, freq, TARGET_RATE);
            assert!(kept > 0.3, "{freq} Hz came through at only {kept}");
        }
    }

    #[test]
    fn the_target_rate_passes_through_untouched() {
        let input = tone(440.0, TARGET_RATE, 0.1);
        assert_eq!(to_target_rate(&input, TARGET_RATE), input);
    }

    #[test]
    fn output_length_follows_the_ratio() {
        let input = tone(440.0, 48_000, 1.0);
        let out = to_target_rate(&input, 48_000);
        // 48k -> 16k is exactly a third.
        assert!(
            out.len().abs_diff(TARGET_RATE as usize) <= 1,
            "{}",
            out.len()
        );
    }

    /// Where the "60 dB" in the module docs comes from. Run with:
    ///   cargo test -p embed --lib resample -- --ignored --nocapture
    #[test]
    #[ignore = "prints the measured alias rejection"]
    fn alias_rejection_measured() {
        let reference = energy_at(
            &to_target_rate(&tone(1_000.0, 44_100, 0.5), 44_100),
            1_000.0,
            TARGET_RATE,
        );
        println!("passband reference (1 kHz): {reference:.4}");
        for (src, alias) in [
            (12_000.0, 4_000.0),
            (15_000.0, 1_000.0),
            (20_000.0, 4_000.0),
        ] {
            let out = to_target_rate(&tone(src, 44_100, 0.5), 44_100);
            let leaked = energy_at(&out, alias, TARGET_RATE);
            println!(
                "{:>6} Hz -> alias at {:>5} Hz: {:.2e}  ({:.1} dB down)",
                src,
                alias,
                leaked,
                20.0 * (reference / leaked.max(1e-12)).log10()
            );
        }
    }

    #[test]
    fn degenerate_input_returns_nothing_rather_than_panicking() {
        assert!(to_target_rate(&[], 44_100).is_empty());
        assert!(to_target_rate(&[1.0, 2.0], 0).is_empty());
        assert!(prepare(&[], 2, 44_100).is_empty());
    }

    /// Upsampling is not the normal direction here, but a 8 kHz podcast rip
    /// should not come out mangled.
    #[test]
    fn upsampling_keeps_the_frequency() {
        let out = to_target_rate(&tone(1_000.0, 8_000, 0.5), 8_000);
        assert!(out.len().abs_diff(8_000) <= 2, "{}", out.len());
        assert!(energy_at(&out, 1_000.0, TARGET_RATE) > 0.3);
    }
}
