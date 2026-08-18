//! `fermix-stt` — the local speech-to-text sidecar the Fermix daemon spawns.
//!
//! The daemon owns the wire contract; this crate implements it. The contract is
//! vendored at `protocol/PROTOCOL.md` with byte-exact samples in
//! `protocol/fixtures/`, and [`protocol`] is tested against them so the two
//! repos cannot drift apart silently.
//!
//! Layering, outermost first:
//!
//! - [`session`] — the dispatch loop and every protocol obligation.
//! - [`ndjson`] — line framing in both directions, with the 8 MiB ceiling.
//! - [`protocol`] — serde types for every event and op.
//! - [`decode`] / [`resample`] / [`pcm`] — container to 16 kHz mono s16le.
//! - [`vad`] — segmentation of a live stream into utterances.
//! - [`asr`] / [`engine`] — the recognition backend, picked at build time.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod asr;
pub mod decode;
pub mod engine;
pub mod ndjson;
pub mod pcm;
pub mod protocol;
pub mod resample;
pub mod session;
pub mod vad;

/// This sidecar's version, reported in `hello` as `stt_version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
