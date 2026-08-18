//! Container decode: ogg / mp3 / m4a / mp4 / wav → 16 kHz mono s16le PCM.
//!
//! Entirely in-process via symphonia. The protocol forbids shelling out to
//! ffmpeg, and the sidecar ships no external decoder.

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::pcm::to_i16;
use crate::resample::{MAX_INPUT_RATE, TARGET_RATE, to_target_rate};

/// Longest input accepted. A voice note is seconds; this is a memory bound, not
/// a product limit, and the daemon's own batch timeout bites long before it.
pub const MAX_DURATION_SECS: u64 = 3600;

/// Consecutive corrupt packets tolerated before the whole decode is refused.
const MAX_BAD_PACKETS: u32 = 32;

/// Decoded audio, always mono at [`TARGET_RATE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    samples: Vec<i16>,
}

impl Pcm {
    /// Wrap already-16 kHz mono samples.
    pub fn new(samples: Vec<i16>) -> Self {
        Self { samples }
    }

    /// The samples, mono, 16 kHz.
    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    /// Duration in milliseconds, rounded down.
    pub fn duration_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / TARGET_RATE as u64
    }
}

/// Why a decode did not produce PCM.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The file could not be opened or read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The path that could not be read.
        path: String,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The container or codec is not one this build can decode.
    #[error("unsupported container: {detail}")]
    Unsupported {
        /// What was unsupported.
        detail: String,
    },
    /// The container was recognized but the audio could not be decoded.
    #[error("decode failed: {detail}")]
    Failed {
        /// What went wrong.
        detail: String,
    },
}

/// Decode the container at `path` into 16 kHz mono PCM.
pub fn decode_file(path: &Path) -> Result<Pcm, DecodeError> {
    let file = File::open(path).map_err(|source| DecodeError::Io {
        path: path.display().to_string(),
        source,
    })?;

    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| DecodeError::Unsupported {
            detail: format!("{} ({e})", path.display()),
        })?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| DecodeError::Unsupported {
            detail: format!("{} has no audio track", path.display()),
        })?;
    let track_id = track.id;
    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| DecodeError::Unsupported {
            detail: format!("{} has no audio codec parameters", path.display()),
        })?
        .clone();

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .map_err(|e| DecodeError::Unsupported {
            detail: format!("no decoder for {}: {e}", path.display()),
        })?;

    let mut mono = MonoAccumulator::default();
    let mut interleaved: Vec<f32> = Vec::new();
    let mut bad_packets: u32 = 0;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(e) => {
                return Err(DecodeError::Failed {
                    detail: format!("demux error: {e}"),
                });
            }
        };
        if packet.track_id != track_id {
            continue;
        }

        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(e @ (SymphoniaError::DecodeError(_) | SymphoniaError::IoError(_)))
                if bad_packets < MAX_BAD_PACKETS =>
            {
                bad_packets += 1;
                eprintln!("fermix-stt: skipping corrupt packet ({bad_packets}): {e}");
                continue;
            }
            Err(e) => {
                return Err(DecodeError::Failed {
                    detail: format!("decoder error after {bad_packets} skipped packets: {e}"),
                });
            }
        };

        let spec = buffer.spec();
        let (rate, channels) = (spec.rate(), spec.channels().count());
        interleaved.resize(buffer.samples_interleaved(), 0.0);
        buffer.copy_to_slice_interleaved(interleaved.as_mut_slice());
        mono.push(rate, channels, &interleaved)?;
    }

    mono.finish()
}

/// Accumulates interleaved packets as mono at the source rate, asserting the
/// stream's rate and channel count never change mid-file.
#[derive(Default)]
struct MonoAccumulator {
    rate: Option<u32>,
    channels: Option<usize>,
    samples: Vec<f32>,
}

impl MonoAccumulator {
    fn push(&mut self, rate: u32, channels: usize, interleaved: &[f32]) -> Result<(), DecodeError> {
        if rate == 0 || rate > MAX_INPUT_RATE {
            return Err(DecodeError::Unsupported {
                detail: format!("implausible sample rate {rate}"),
            });
        }
        if channels == 0 {
            return Err(DecodeError::Unsupported {
                detail: "stream declares zero channels".to_string(),
            });
        }
        if *self.rate.get_or_insert(rate) != rate
            || *self.channels.get_or_insert(channels) != channels
        {
            return Err(DecodeError::Failed {
                detail: "sample rate or channel count changed mid-stream".to_string(),
            });
        }

        let limit = (rate as u64).saturating_mul(MAX_DURATION_SECS) as usize;
        let frames = interleaved.len() / channels;
        if self.samples.len() + frames > limit {
            return Err(DecodeError::Failed {
                detail: format!("audio exceeds the {MAX_DURATION_SECS}s ceiling"),
            });
        }

        let scale = 1.0 / channels as f32;
        self.samples.extend(
            interleaved
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() * scale),
        );
        Ok(())
    }

    fn finish(self) -> Result<Pcm, DecodeError> {
        let rate = self.rate.ok_or_else(|| DecodeError::Failed {
            detail: "stream produced no audio".to_string(),
        })?;

        let resampled = to_target_rate(&self.samples, rate);
        Ok(Pcm::new(resampled.into_iter().map(to_i16).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_derived_from_the_target_rate() {
        let pcm = Pcm::new(vec![0; 8000]);
        assert_eq!(pcm.duration_ms(), 500);
    }

    #[test]
    fn a_channel_count_change_mid_stream_is_refused() {
        let mut acc = MonoAccumulator::default();
        acc.push(16_000, 1, &[0.0, 0.0]).unwrap();
        let err = acc.push(16_000, 2, &[0.0, 0.0]).unwrap_err();
        assert!(matches!(err, DecodeError::Failed { .. }), "{err}");
    }

    #[test]
    fn a_rate_change_mid_stream_is_refused() {
        let mut acc = MonoAccumulator::default();
        acc.push(16_000, 1, &[0.0]).unwrap();
        let err = acc.push(44_100, 1, &[0.0]).unwrap_err();
        assert!(matches!(err, DecodeError::Failed { .. }), "{err}");
    }

    #[test]
    fn an_implausible_rate_is_refused_before_it_reaches_the_resampler() {
        let mut acc = MonoAccumulator::default();
        let err = acc.push(MAX_INPUT_RATE + 1, 1, &[0.0]).unwrap_err();
        assert!(matches!(err, DecodeError::Unsupported { .. }), "{err}");
    }

    #[test]
    fn an_over_long_stream_is_refused() {
        let mut acc = MonoAccumulator::default();
        let rate = 16_000u32;
        let chunk = vec![0.0f32; rate as usize];
        for _ in 0..MAX_DURATION_SECS {
            acc.push(rate, 1, &chunk).unwrap();
        }
        let err = acc.push(rate, 1, &chunk).unwrap_err();
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    #[test]
    fn stereo_is_downmixed_to_mono() {
        let mut acc = MonoAccumulator::default();
        acc.push(16_000, 2, &[1.0, -1.0, 0.5, 0.5]).unwrap();
        let pcm = acc.finish().unwrap();
        assert_eq!(pcm.samples(), &[0, to_i16(0.5)]);
    }

    #[test]
    fn a_stream_with_no_audio_is_an_error() {
        let err = MonoAccumulator::default().finish().unwrap_err();
        assert!(matches!(err, DecodeError::Failed { .. }), "{err}");
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let err = decode_file(Path::new("/nonexistent/fermix-stt/missing.wav")).unwrap_err();
        assert!(matches!(err, DecodeError::Io { .. }), "{err}");
    }
}
