//! Engine selection.
//!
//! Exactly one backend is compiled into any given binary, chosen by the
//! `sherpa` cargo feature. There is no runtime switch and no probing: a binary
//! either has the native library linked or it does not.

pub mod null;

#[cfg(feature = "sherpa")]
pub mod sherpa;

use crate::asr::Engine;

/// Build the backend this binary was compiled with.
#[cfg(feature = "sherpa")]
pub fn compiled_in() -> Box<dyn Engine> {
    Box::new(sherpa::SherpaEngine::new())
}

/// Build the backend this binary was compiled with.
#[cfg(not(feature = "sherpa"))]
pub fn compiled_in() -> Box<dyn Engine> {
    Box::new(null::NullEngine)
}
