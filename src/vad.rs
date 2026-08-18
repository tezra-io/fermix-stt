//! Voice-activity segmentation for stream mode.
//!
//! Stream mode recognizes utterances, not frames: audio arrives as a byte
//! stream, a segmenter cuts it into speech runs, and each run is recognized on
//! its own. [`Segmenter`] is that seam.
//!
//! Two implementations exist, one per build configuration:
//!
//! - [`EnergySegmenter`] — the default (hermetic) build. Pure Rust, no model,
//!   no native library; it is what the wire tests drive.
//! - `engine::sherpa::SileroSegmenter` — the `sherpa` build. Silero VAD, whose
//!   own state machine emits speech segments directly.
//!
//! The session code depends only on this trait, so the two never coexist at
//! runtime and there is no per-call branching between them.

use crate::resample::TARGET_RATE;

/// Samples per analysis frame: 32 ms at 16 kHz, matching Silero's window.
pub const FRAME_SAMPLES: usize = 512;

/// One detected speech run, with times measured from stream start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    /// 16 kHz mono samples for this run.
    pub pcm: Vec<i16>,
    /// Start time in milliseconds from stream start.
    pub t0_ms: u64,
    /// End time in milliseconds from stream start.
    pub t1_ms: u64,
}

/// Why segmentation failed.
#[derive(Debug, thiserror::Error)]
pub enum VadError {
    /// The VAD model could not be initialized.
    #[error("voice activity detector unavailable: {0}")]
    Unavailable(String),
}

/// Cuts a continuous 16 kHz mono stream into utterances.
pub trait Segmenter: Send {
    /// Feed samples; returns every utterance completed by this chunk.
    fn push(&mut self, pcm: &[i16]) -> Result<Vec<Utterance>, VadError>;

    /// End of audio: return the utterance still open, if any.
    fn flush(&mut self) -> Result<Vec<Utterance>, VadError>;
}

/// Tuning for [`EnergySegmenter`]. Every field is a hard bound on the state
/// machine, not a preference knob.
#[derive(Debug, Clone, Copy)]
pub struct EnergyParams {
    /// RMS above which a frame counts as speech, on the s16 scale.
    pub speech_rms: f32,
    /// Silent frames that close an open utterance (~384 ms by default).
    pub hangover_frames: usize,
    /// Frames kept before onset so a word is not clipped (~128 ms).
    pub pre_roll_frames: usize,
    /// Voiced frames an utterance needs to be worth recognizing (~64 ms).
    pub min_voiced_frames: usize,
    /// Frames after which an utterance is force-closed (~30 s).
    pub max_frames: usize,
}

impl Default for EnergyParams {
    fn default() -> Self {
        Self {
            speech_rms: 350.0,
            hangover_frames: 12,
            pre_roll_frames: 4,
            min_voiced_frames: 2,
            max_frames: 937,
        }
    }
}

/// Short-term-energy segmenter: the hermetic default-build detector.
pub struct EnergySegmenter {
    params: EnergyParams,
    partial: Vec<i16>,
    pre_roll: std::collections::VecDeque<Vec<i16>>,
    active: Option<Active>,
    frames_seen: u64,
}

struct Active {
    start_frame: u64,
    pcm: Vec<i16>,
    voiced_frames: usize,
    silence_run: usize,
}

impl EnergySegmenter {
    /// A segmenter with the given bounds.
    ///
    /// # Panics
    /// Panics if any bound is degenerate; these are compile-time-ish constants
    /// and a zero would make the state machine unbounded or trivial.
    pub fn new(params: EnergyParams) -> Self {
        assert!(params.speech_rms > 0.0, "speech_rms must be positive");
        assert!(
            params.hangover_frames > 0,
            "hangover_frames must be non-zero"
        );
        assert!(
            params.min_voiced_frames > 0,
            "min_voiced_frames must be non-zero"
        );
        assert!(
            params.max_frames > params.hangover_frames,
            "max_frames must exceed hangover_frames"
        );
        Self {
            params,
            partial: Vec::with_capacity(FRAME_SAMPLES),
            pre_roll: std::collections::VecDeque::new(),
            active: None,
            frames_seen: 0,
        }
    }

    fn is_speech(&self, frame: &[i16]) -> bool {
        let sum: f64 = frame.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        (sum / frame.len() as f64).sqrt() >= self.params.speech_rms as f64
    }

    fn consume_frame(&mut self, frame: &[i16], out: &mut Vec<Utterance>) {
        let speech = self.is_speech(frame);
        let index = self.frames_seen;
        self.frames_seen += 1;

        let Some(active) = self.active.as_mut() else {
            if speech {
                self.open(index, frame);
            } else {
                self.pre_roll.push_back(frame.to_vec());
                if self.pre_roll.len() > self.params.pre_roll_frames {
                    self.pre_roll.pop_front();
                }
            }
            return;
        };

        active.pcm.extend_from_slice(frame);
        if speech {
            active.voiced_frames += 1;
            active.silence_run = 0;
        } else {
            active.silence_run += 1;
        }

        let frames = active.pcm.len() / FRAME_SAMPLES;
        if active.silence_run >= self.params.hangover_frames {
            self.close(true, out);
        } else if frames >= self.params.max_frames {
            self.close(false, out);
        }
    }

    fn open(&mut self, index: u64, frame: &[i16]) {
        let lead = self.pre_roll.len() as u64;
        let mut pcm = Vec::with_capacity((lead as usize + 1) * FRAME_SAMPLES);
        for buffered in self.pre_roll.drain(..) {
            pcm.extend_from_slice(&buffered);
        }
        pcm.extend_from_slice(frame);

        self.active = Some(Active {
            start_frame: index - lead,
            pcm,
            voiced_frames: 1,
            silence_run: 0,
        });
    }

    fn close(&mut self, trim_trailing_silence: bool, out: &mut Vec<Utterance>) {
        let Some(mut active) = self.active.take() else {
            return;
        };

        if trim_trailing_silence {
            let trim = active.silence_run * FRAME_SAMPLES;
            let keep = active.pcm.len().saturating_sub(trim);
            active.pcm.truncate(keep);
        }

        if active.voiced_frames < self.params.min_voiced_frames || active.pcm.is_empty() {
            return;
        }

        let t0_ms = samples_to_ms(active.start_frame * FRAME_SAMPLES as u64);
        let t1_ms = t0_ms + samples_to_ms(active.pcm.len() as u64);
        out.push(Utterance {
            pcm: active.pcm,
            t0_ms,
            t1_ms,
        });
    }
}

impl Segmenter for EnergySegmenter {
    fn push(&mut self, pcm: &[i16]) -> Result<Vec<Utterance>, VadError> {
        let mut out = Vec::new();
        self.partial.extend_from_slice(pcm);

        let mut offset = 0;
        while offset + FRAME_SAMPLES <= self.partial.len() {
            let frame: Vec<i16> = self.partial[offset..offset + FRAME_SAMPLES].to_vec();
            self.consume_frame(&frame, &mut out);
            offset += FRAME_SAMPLES;
        }
        self.partial.drain(..offset);
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<Utterance>, VadError> {
        let mut out = Vec::new();
        if !self.partial.is_empty() {
            let mut frame = std::mem::take(&mut self.partial);
            frame.resize(FRAME_SAMPLES, 0);
            self.consume_frame(&frame, &mut out);
        }
        self.close(true, &mut out);
        self.pre_roll.clear();
        Ok(out)
    }
}

/// Convert a 16 kHz sample count to milliseconds.
pub fn samples_to_ms(samples: u64) -> u64 {
    samples * 1000 / TARGET_RATE as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize) -> Vec<i16> {
        (0..frames * FRAME_SAMPLES)
            .map(|i| ((i as f32 * 0.2).sin() * 8000.0) as i16)
            .collect()
    }

    fn silence(frames: usize) -> Vec<i16> {
        vec![0; frames * FRAME_SAMPLES]
    }

    fn segmenter() -> EnergySegmenter {
        EnergySegmenter::new(EnergyParams::default())
    }

    #[test]
    fn silence_alone_produces_nothing() {
        let mut seg = segmenter();
        assert!(seg.push(&silence(30)).unwrap().is_empty());
        assert!(seg.flush().unwrap().is_empty());
    }

    #[test]
    fn one_speech_run_is_one_utterance_closed_by_flush() {
        let mut seg = segmenter();
        assert!(seg.push(&tone(10)).unwrap().is_empty());
        let out = seg.flush().unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].t0_ms, 0);
        assert!(out[0].t1_ms > out[0].t0_ms);
    }

    #[test]
    fn a_long_gap_splits_two_utterances() {
        let mut seg = segmenter();
        let mut audio = tone(10);
        audio.extend(silence(20));
        audio.extend(tone(10));

        let first = seg.push(&audio).unwrap();
        assert_eq!(first.len(), 1, "the gap should have closed the first run");

        let rest = seg.flush().unwrap();
        assert_eq!(rest.len(), 1);
        assert!(
            rest[0].t0_ms > first[0].t1_ms,
            "utterances must not overlap: {:?} then {:?}",
            first[0],
            rest[0]
        );
    }

    #[test]
    fn a_short_gap_does_not_split() {
        let mut seg = segmenter();
        let mut audio = tone(6);
        audio.extend(silence(3));
        audio.extend(tone(6));
        assert!(seg.push(&audio).unwrap().is_empty());
        assert_eq!(seg.flush().unwrap().len(), 1);
    }

    #[test]
    fn chunked_delivery_matches_one_shot_delivery() {
        let mut audio = silence(3);
        audio.extend(tone(8));
        audio.extend(silence(20));
        audio.extend(tone(8));

        let mut whole = segmenter();
        let mut expected = whole.push(&audio).unwrap();
        expected.extend(whole.flush().unwrap());

        let mut chunked = segmenter();
        let mut actual = Vec::new();
        for chunk in audio.chunks(333) {
            actual.extend(chunked.push(chunk).unwrap());
        }
        actual.extend(chunked.flush().unwrap());

        assert_eq!(actual, expected);
    }

    #[test]
    fn an_utterance_is_force_closed_at_the_frame_ceiling() {
        let params = EnergyParams {
            max_frames: 10,
            hangover_frames: 4,
            ..EnergyParams::default()
        };
        let mut seg = EnergySegmenter::new(params);
        let out = seg.push(&tone(25)).unwrap();
        assert_eq!(out.len(), 2, "expected two force-closed utterances");
        for utterance in &out {
            assert_eq!(utterance.pcm.len(), 10 * FRAME_SAMPLES);
        }
        assert_eq!(
            seg.flush().unwrap().len(),
            1,
            "the tail should still be flushed"
        );
    }

    #[test]
    fn a_blip_shorter_than_the_voiced_minimum_is_dropped() {
        let params = EnergyParams {
            min_voiced_frames: 5,
            ..EnergyParams::default()
        };
        let mut seg = EnergySegmenter::new(params);
        seg.push(&tone(2)).unwrap();
        seg.push(&silence(20)).unwrap();
        assert!(seg.flush().unwrap().is_empty());
    }

    #[test]
    fn pre_roll_backdates_the_start_of_a_run() {
        let mut seg = segmenter();
        let mut audio = silence(10);
        audio.extend(tone(5));
        seg.push(&audio).unwrap();
        let out = seg.flush().unwrap();
        assert_eq!(out.len(), 1);
        // Onset is at frame 10; four pre-roll frames back that up to frame 6.
        assert_eq!(out[0].t0_ms, samples_to_ms(6 * FRAME_SAMPLES as u64));
    }

    #[test]
    #[should_panic(expected = "hangover_frames must be non-zero")]
    fn a_degenerate_hangover_is_rejected() {
        EnergySegmenter::new(EnergyParams {
            hangover_frames: 0,
            ..EnergyParams::default()
        });
    }
}
