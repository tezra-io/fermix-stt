//! Shared helpers for the integration tests: scripted sessions and stub
//! engines. Nothing here touches the network, a model, or a native library.

#![allow(dead_code)]

use std::io::BufReader;
use std::path::Path;

use fermix_stt::asr::{Engine, EngineError, Recognizer};
use fermix_stt::protocol::Event;
use fermix_stt::session;
use fermix_stt::vad::{EnergyParams, EnergySegmenter, Segmenter};

/// What a [`StubEngine`] should do when asked to recognize.
#[derive(Debug, Clone)]
pub enum Behavior {
    /// Return this text for every utterance.
    Text(String),
    /// Fail to load the model.
    FailLoad,
    /// Load fine, then fail recognition.
    FailRecognize,
}

/// An engine with no model behind it, used to drive the wire.
#[derive(Debug, Clone)]
pub struct StubEngine {
    behavior: Behavior,
}

impl StubEngine {
    /// An engine that transcribes everything as `text`.
    pub fn saying(text: &str) -> Box<dyn Engine> {
        Box::new(Self {
            behavior: Behavior::Text(text.to_string()),
        })
    }

    /// An engine whose model directory never loads.
    pub fn failing_to_load() -> Box<dyn Engine> {
        Box::new(Self {
            behavior: Behavior::FailLoad,
        })
    }

    /// An engine that loads but cannot recognize.
    pub fn failing_to_recognize() -> Box<dyn Engine> {
        Box::new(Self {
            behavior: Behavior::FailRecognize,
        })
    }
}

impl Engine for StubEngine {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn load(&self, model_dir: &Path) -> Result<Box<dyn Recognizer>, EngineError> {
        match &self.behavior {
            Behavior::FailLoad => Err(EngineError::ModelLoad {
                dir: model_dir.display().to_string(),
                detail: "stub refuses every model".to_string(),
            }),
            other => Ok(Box::new(StubRecognizer {
                behavior: other.clone(),
            })),
        }
    }

    fn segmenter(&self) -> Result<Box<dyn Segmenter>, EngineError> {
        Ok(Box::new(EnergySegmenter::new(EnergyParams::default())))
    }
}

struct StubRecognizer {
    behavior: Behavior,
}

impl StubRecognizer {
    fn answer(&self) -> Result<String, EngineError> {
        match &self.behavior {
            Behavior::Text(text) => Ok(text.clone()),
            Behavior::FailRecognize => Err(EngineError::Recognition(
                "stub cannot recognize".to_string(),
            )),
            Behavior::FailLoad => unreachable!("a failing load never produces a recognizer"),
        }
    }
}

impl Recognizer for StubRecognizer {
    fn transcribe_batch(&mut self, _pcm: &[i16]) -> Result<String, EngineError> {
        self.answer()
    }

    fn transcribe_segment(&mut self, _pcm: &[i16]) -> Result<String, EngineError> {
        self.answer()
    }
}

/// Run a whole session in-process against `input` and return the raw stdout.
pub fn drive_raw(engine: Box<dyn Engine>, input: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    session::run(engine, BufReader::new(input.as_bytes()), &mut out, "9.9.9")
        .expect("the session must not fail on a healthy pipe");
    out
}

/// Run a whole session in-process and decode every event it emitted.
pub fn drive(engine: Box<dyn Engine>, input: &str) -> Vec<Event> {
    let raw = drive_raw(engine, input);
    String::from_utf8(raw)
        .expect("stdout must be UTF-8")
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("undecodable event {line}: {e}"))
        })
        .collect()
}

/// One `audio` op carrying `samples` as base64 s16le.
pub fn audio_op(id: &str, samples: &[i16]) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let pcm = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("{{\"op\":\"audio\",\"id\":\"{id}\",\"pcm\":\"{pcm}\"}}")
}

/// Loud audio: `frames` frames of a tone the energy segmenter counts as speech.
pub fn tone(frames: usize) -> Vec<i16> {
    (0..frames * fermix_stt::vad::FRAME_SAMPLES)
        .map(|i| ((i as f32 * 0.2).sin() * 8000.0) as i16)
        .collect()
}

/// Digital silence, `frames` frames long.
pub fn silence(frames: usize) -> Vec<i16> {
    vec![0; frames * fermix_stt::vad::FRAME_SAMPLES]
}

/// The committed WAV fixture: 300 ms of 440 Hz, mono, 44.1 kHz.
pub fn wav_fixture() -> String {
    fixture_path("sine_440_44k1_mono.wav")
}

/// A file that is not a media container.
pub fn junk_fixture() -> String {
    fixture_path("not_audio.amr")
}

fn fixture_path(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

/// A directory that certainly exists, for ops that must carry a `model_dir`.
pub fn model_dir() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}
