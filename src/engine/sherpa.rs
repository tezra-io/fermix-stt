//! The real engine: sherpa-onnx offline recognition + Silero VAD.
//!
//! Compiled only under `--features sherpa`. Building it links the sherpa-onnx
//! native library and embeds `assets/silero_vad.onnx`; running it needs a
//! Parakeet TDT model directory, which the daemon's ModelStore supplies as
//! `model_dir`. See the README's "Owner gate" section.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig, SileroVadModelConfig,
    VadModelConfig, VoiceActivityDetector,
};

use crate::asr::{Engine, EngineError, Recognizer};
use crate::pcm;
use crate::resample::TARGET_RATE;
use crate::vad::{FRAME_SAMPLES, Segmenter, Utterance, VadError, samples_to_ms};

/// Silero VAD, embedded at build time so the sidecar never downloads a model.
/// `build.rs` stages the file into `OUT_DIR` and fails the build loudly if the
/// asset is missing.
const SILERO_VAD_ONNX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/silero_vad.onnx"));

/// Files a Parakeet TDT model directory must contain. Exact names, no probing:
/// a missing file is a loud `model_load_failed`, not a silent second guess.
const ENCODER: &str = "encoder.int8.onnx";
const DECODER: &str = "decoder.int8.onnx";
const JOINER: &str = "joiner.int8.onnx";
const TOKENS: &str = "tokens.txt";

/// Threads each recognizer and the VAD may use.
const NUM_THREADS: i32 = 2;

/// Seconds of audio the VAD may buffer before it must emit or drop.
const VAD_BUFFER_SECONDS: f32 = 60.0;

/// Upper bound on one drain pass. The queue holds at most a few segments per
/// audio frame; this only exists so the loop cannot run away.
const MAX_SEGMENTS_PER_DRAIN: usize = 1024;

/// The sherpa-onnx backend.
#[derive(Default)]
pub struct SherpaEngine {
    vad_model: OnceLock<StagedAsset>,
}

impl SherpaEngine {
    /// A backend that has not yet touched the filesystem.
    pub fn new() -> Self {
        Self::default()
    }

    /// Path to the embedded Silero model, written to disk on first use.
    fn vad_model_path(&self) -> Result<&Path, VadError> {
        if let Some(asset) = self.vad_model.get() {
            return Ok(asset.path());
        }
        let staged = StagedAsset::write("silero_vad.onnx", SILERO_VAD_ONNX)
            .map_err(|e| VadError::Unavailable(format!("cannot stage Silero VAD: {e}")))?;
        let _ = self.vad_model.set(staged);
        Ok(self.vad_model.get().expect("just set").path())
    }
}

impl Engine for SherpaEngine {
    fn name(&self) -> &'static str {
        "sherpa-onnx"
    }

    fn load(&self, model_dir: &Path) -> Result<Box<dyn Recognizer>, EngineError> {
        let missing: Vec<&str> = [ENCODER, DECODER, JOINER, TOKENS]
            .into_iter()
            .filter(|name| !model_dir.join(name).is_file())
            .collect();
        if !missing.is_empty() {
            return Err(EngineError::ModelLoad {
                dir: model_dir.display().to_string(),
                detail: format!("missing {}", missing.join(", ")),
            });
        }

        let named = |name: &str| Some(model_dir.join(name).display().to_string());

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer = OfflineTransducerModelConfig {
            encoder: named(ENCODER),
            decoder: named(DECODER),
            joiner: named(JOINER),
        };
        config.model_config.tokens = named(TOKENS);
        config.model_config.model_type = Some("nemo_transducer".to_string());
        config.model_config.num_threads = NUM_THREADS;
        config.model_config.provider = Some("cpu".to_string());
        config.decoding_method = Some("greedy_search".to_string());

        let recognizer =
            OfflineRecognizer::create(&config).ok_or_else(|| EngineError::ModelLoad {
                dir: model_dir.display().to_string(),
                detail: "sherpa-onnx rejected the model configuration".to_string(),
            })?;

        Ok(Box::new(SherpaRecognizer { recognizer }))
    }

    fn segmenter(&self) -> Result<Box<dyn Segmenter>, EngineError> {
        let model = self.vad_model_path()?.display().to_string();
        Ok(Box::new(SileroSegmenter::create(&model)?))
    }
}

/// A Parakeet TDT model held open for one request or one stream.
struct SherpaRecognizer {
    recognizer: OfflineRecognizer,
}

impl SherpaRecognizer {
    fn recognize(&self, pcm_samples: &[i16]) -> Result<String, EngineError> {
        if pcm_samples.is_empty() {
            return Ok(String::new());
        }
        let samples = pcm::to_f32(pcm_samples);
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(TARGET_RATE as i32, &samples);
        self.recognizer.decode(&stream);
        let result = stream.get_result().ok_or_else(|| {
            EngineError::Recognition("sherpa-onnx returned no result".to_string())
        })?;
        Ok(result.text.trim().to_string())
    }
}

impl Recognizer for SherpaRecognizer {
    fn transcribe_batch(&mut self, pcm_samples: &[i16]) -> Result<String, EngineError> {
        self.recognize(pcm_samples)
    }

    fn transcribe_segment(&mut self, pcm_samples: &[i16]) -> Result<String, EngineError> {
        self.recognize(pcm_samples)
    }
}

/// Silero VAD. Its own state machine owns onset, hangover and maximum-duration
/// policy, so this type only converts sample offsets into stream times.
struct SileroSegmenter {
    vad: VoiceActivityDetector,
}

impl SileroSegmenter {
    fn create(model_path: &str) -> Result<Self, VadError> {
        let mut config = VadModelConfig {
            sample_rate: TARGET_RATE as i32,
            num_threads: NUM_THREADS,
            provider: Some("cpu".to_string()),
            ..VadModelConfig::default()
        };
        config.silero_vad = SileroVadModelConfig {
            model: Some(model_path.to_string()),
            threshold: 0.5,
            min_silence_duration: 0.35,
            min_speech_duration: 0.25,
            window_size: FRAME_SAMPLES as i32,
            max_speech_duration: 30.0,
        };

        let vad = VoiceActivityDetector::create(&config, VAD_BUFFER_SECONDS)
            .ok_or_else(|| VadError::Unavailable(format!("Silero VAD refused {model_path}")))?;
        Ok(Self { vad })
    }

    /// Drain every segment the detector has queued.
    fn drain(&self) -> Vec<Utterance> {
        let mut out = Vec::new();
        for _ in 0..MAX_SEGMENTS_PER_DRAIN {
            let Some(segment) = self.vad.front() else {
                return out;
            };
            let start = segment.start().max(0) as u64;
            let pcm = pcm::from_f32(segment.samples());
            let t0_ms = samples_to_ms(start);
            let t1_ms = t0_ms + samples_to_ms(pcm.len() as u64);
            out.push(Utterance { pcm, t0_ms, t1_ms });
            self.vad.pop();
        }
        out
    }
}

impl Segmenter for SileroSegmenter {
    fn push(&mut self, samples: &[i16]) -> Result<Vec<Utterance>, VadError> {
        self.vad.accept_waveform(&pcm::to_f32(samples));
        Ok(self.drain())
    }

    fn flush(&mut self) -> Result<Vec<Utterance>, VadError> {
        self.vad.flush();
        Ok(self.drain())
    }
}

/// An embedded asset written to a private temporary file for the process
/// lifetime, and removed when the engine drops.
struct StagedAsset {
    path: PathBuf,
}

impl StagedAsset {
    fn write(name: &str, bytes: &[u8]) -> std::io::Result<Self> {
        let unique = format!("fermix-stt-{}-{name}", std::process::id());
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, bytes)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedAsset {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            eprintln!(
                "fermix-stt: could not remove staged asset {}: {e}",
                self.path.display()
            );
        }
    }
}
