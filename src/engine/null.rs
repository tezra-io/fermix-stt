//! The hermetic engine used by the default build.
//!
//! It exercises the whole wire, decode and segmentation path without a native
//! library, a model, or a network fetch — which is what makes `cargo test`
//! runnable anywhere. It does not recognize speech: every transcript is a
//! deterministic placeholder naming the sample count it was handed. A binary
//! built without `--features sherpa` is a protocol conformance harness, not a
//! transcription product.

use std::path::Path;

use crate::asr::{Engine, EngineError, Recognizer};
use crate::vad::{EnergyParams, EnergySegmenter, Segmenter};

/// The default-build engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullEngine;

impl Engine for NullEngine {
    fn name(&self) -> &'static str {
        "null"
    }

    fn load(&self, model_dir: &Path) -> Result<Box<dyn Recognizer>, EngineError> {
        if !model_dir.is_dir() {
            return Err(EngineError::ModelLoad {
                dir: model_dir.display().to_string(),
                detail: "not a directory".to_string(),
            });
        }
        Ok(Box::new(NullRecognizer))
    }

    fn segmenter(&self) -> Result<Box<dyn Segmenter>, EngineError> {
        Ok(Box::new(EnergySegmenter::new(EnergyParams::default())))
    }
}

/// Returns a placeholder transcript instead of recognizing speech.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullRecognizer;

impl NullRecognizer {
    fn placeholder(kind: &str, pcm: &[i16]) -> String {
        format!("[null-recognizer {kind} {} samples]", pcm.len())
    }
}

impl Recognizer for NullRecognizer {
    fn transcribe_batch(&mut self, pcm: &[i16]) -> Result<String, EngineError> {
        Ok(Self::placeholder("batch", pcm))
    }

    fn transcribe_segment(&mut self, pcm: &[i16]) -> Result<String, EngineError> {
        Ok(Self::placeholder("segment", pcm))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_a_missing_directory_is_a_model_load_failure() {
        let Err(err) = NullEngine.load(Path::new("/nonexistent/fermix-stt/models")) else {
            panic!("a missing directory must not load");
        };
        assert_eq!(err.code(), crate::protocol::ErrorCode::ModelLoadFailed);
    }

    #[test]
    fn loading_an_existing_directory_succeeds() {
        let mut recognizer = NullEngine
            .load(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        assert_eq!(
            recognizer.transcribe_batch(&[0, 0, 0]).unwrap(),
            "[null-recognizer batch 3 samples]"
        );
    }
}
