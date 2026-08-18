//! The recognition seam.
//!
//! [`Engine`] is the compiled-in ASR backend. It is chosen once at build time
//! (see `crate::engine`), never per call, so there is exactly one recognition
//! path in any given binary.

use std::path::Path;

use crate::protocol::ErrorCode;
use crate::vad::{Segmenter, VadError};

/// Why the engine could not do its job.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The ASR model directory is missing, incomplete, or unreadable.
    #[error("cannot load model from {dir}: {detail}")]
    ModelLoad {
        /// The model directory that was rejected.
        dir: String,
        /// Why it was rejected.
        detail: String,
    },
    /// Recognition itself failed.
    #[error("recognition failed: {0}")]
    Recognition(String),
    /// The segmenter could not be created.
    #[error(transparent)]
    Vad(#[from] VadError),
}

impl EngineError {
    /// The wire error code this maps to.
    pub fn code(&self) -> ErrorCode {
        match self {
            EngineError::ModelLoad { .. } => ErrorCode::ModelLoadFailed,
            EngineError::Recognition(_) => ErrorCode::Internal,
            EngineError::Vad(_) => ErrorCode::ModelLoadFailed,
        }
    }
}

/// A loaded ASR model, bound to one request or one live stream.
///
/// Batch and streaming stay separate methods: today both run the same offline
/// decode, but the streaming call is the one a future online recognizer would
/// re-implement, and the session code should not have to change when it does.
pub trait Recognizer: Send {
    /// Recognize a whole file's PCM in one shot.
    fn transcribe_batch(&mut self, pcm: &[i16]) -> Result<String, EngineError>;

    /// Recognize one VAD-segmented utterance from a live stream.
    fn transcribe_segment(&mut self, pcm: &[i16]) -> Result<String, EngineError>;
}

/// The compiled-in backend: loads models and builds segmenters.
pub trait Engine: Send {
    /// Human-readable backend name, for diagnostics on stderr.
    fn name(&self) -> &'static str;

    /// Load the ASR model in `model_dir`. Called per request, never at startup:
    /// `hello` must go out before any model touches disk.
    fn load(&self, model_dir: &Path) -> Result<Box<dyn Recognizer>, EngineError>;

    /// A fresh segmenter for one live stream.
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, EngineError>;
}
