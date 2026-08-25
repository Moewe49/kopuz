//! Decoding a window of audio into what the model wants, without ffmpeg.
//!
//! The prototype shelled out to `ffmpeg -ss 45 -t 30 -ac 1 -ar 16000`. That
//! works and it is what the reference embeddings were made with, but it means
//! the feature only exists on machines that have ffmpeg — and `deps.rs` only
//! manages it on Windows and macOS, so a Linux listener would be told to go
//! and install something.
//!
//! symphonia is already compiled into every native target of this workspace
//! for playback, so the decoding is free. What is not free is being *correct*
//! about it: see [`crate::resample`] for the two things the player's own
//! helpers get wrong for this purpose.
//!
//! One further difference from the playback path: samples are accumulated at
//! the source rate and resampled **once**, at the end. The player resamples
//! each packet on its own, which is inaudible in a stream but puts a seam at
//! every packet boundary — the interpolation kernel cannot see across one.
//! Thirty seconds of audio is a few hundred packets, and a few hundred seams
//! is exactly the kind of texture a model happily reads as content.

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::registry::RegisterableAudioDecoder;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

/// Decode `len_secs` starting at `start_secs`, returned as 16 kHz mono f32.
///
/// The window starts inside the track on purpose: the opening seconds are an
/// intro, and an intro is not what a track sounds like.
///
/// A track shorter than `start_secs` yields whatever is there from the
/// beginning rather than nothing — a two-minute interlude is still worth
/// embedding.
pub fn window(
    source: Box<dyn MediaSource>,
    hint: &Hint,
    start_secs: f64,
    len_secs: f64,
) -> Result<Vec<f32>, String> {
    let mss = MediaSourceStream::new(source, Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let codec_params = track
        .codec_params
        .clone()
        .ok_or("track has no codec parameters")?;
    let mut audio_params = codec_params.audio().ok_or("track is not audio")?.clone();

    // YouTube Music's WebM/Opus reaches the codec layer with `channels` empty
    // — symphonia's matroska demuxer does not always propagate it, and both
    // Opus decoders then refuse with "channels required". Read it out of the
    // OpusHead in extra_data, falling back to stereo at 48 kHz. The playback
    // path learned this the hard way and carries the same fixup.
    if audio_params.channels.is_none() {
        let ch = audio_params
            .extra_data
            .as_deref()
            .and_then(opushead_channels)
            .unwrap_or(2);
        audio_params.channels = Some(symphonia::core::audio::Channels::Discrete(ch as u16));
        if audio_params.sample_rate.is_none() {
            audio_params.sample_rate = Some(48_000);
        }
    }
    let src_rate = audio_params.sample_rate.ok_or("track has no sample rate")?;

    // Opus is not among symphonia's bundled codecs — measured on a real
    // YouTube stream, which failed with "unsupported audio codec" before this
    // fallback existed. Every YouTube track would have silently produced no
    // vector at all.
    let mut decoder = match symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
    {
        Ok(d) => d,
        Err(_) => symphonia_adapter_libopus::OpusDecoder::try_registry_new(
            &audio_params,
            &AudioDecoderOptions::default(),
        )
        .map_err(|e| format!("decoder: {e}"))?,
    };

    // Coarse is right here: landing a few frames early or late inside a
    // thirty-second window changes nothing, and Accurate costs a re-decode
    // from the previous keyframe.
    if start_secs > 0.0
        && let Some(time) = Time::try_from_secs_f64(start_secs)
    {
        // A track shorter than the offset simply fails to seek; that is not an
        // error, it means "start from the beginning".
        let _ = format.seek(
            SeekMode::Coarse,
            SeekTo::Time {
                time,
                track_id: Some(track_id),
            },
        );
        decoder.reset();
    }

    let want = (len_secs * src_rate as f64) as usize;
    let mut channels = 0usize;
    let mut interleaved: Vec<f32> = Vec::with_capacity(want * 2);
    // `copy_to_vec_interleaved` REPLACES the vector it is given, it does not
    // append — the playback path hands it a fresh one per packet, which hides
    // this. Passing the accumulator directly kept only the last packet: thirty
    // seconds of audio came out as a single twenty-millisecond frame, and
    // nothing said so.
    let mut packet_samples: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(format!("demux: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // One bad packet is not a bad track. Skipping it leaves a small
            // gap; giving up leaves nothing.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        };
        if channels == 0 {
            channels = decoded.num_planes().max(1);
        }
        decoded.copy_to_vec_interleaved(&mut packet_samples);
        interleaved.extend_from_slice(&packet_samples);
        if interleaved.len() >= want * channels.max(1) {
            break;
        }
    }

    if channels == 0 || interleaved.is_empty() {
        return Err("no audio decoded".into());
    }
    interleaved.truncate(want * channels);
    // Downmixed and resampled once, over the whole window — see the note at
    // the top about per-packet seams.
    Ok(crate::resample::prepare(&interleaved, channels, src_rate))
}

/// Channel count out of an `OpusHead` block, if that is what this is.
fn opushead_channels(extra: &[u8]) -> Option<u8> {
    (extra.len() >= 10 && &extra[..8] == b"OpusHead").then(|| extra[9])
}

/// Decode a window from a file on disk.
pub fn window_from_file(
    path: &std::path::Path,
    start_secs: f64,
    len_secs: f64,
) -> Result<Vec<f32>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    window(Box::new(file), &hint, start_secs, len_secs)
}
