//! Stages the embedded Silero VAD model for the `sherpa` build.
//!
//! The protocol requires the VAD to ship inside the binary — the sidecar never
//! downloads anything at runtime. The asset is not committed (it is a binary
//! blob owned by the Silero project), so the `sherpa` build resolves exactly
//! one path, `assets/silero_vad.onnx`, and fails loudly when it is absent.
//! The default build stages nothing and needs nothing.

use std::path::{Path, PathBuf};

/// The one place the asset may live. No search path, no fallback.
const ASSET: &str = "assets/silero_vad.onnx";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ASSET}");

    if std::env::var_os("CARGO_FEATURE_SHERPA").is_none() {
        return;
    }

    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let source = manifest.join(ASSET);
    if !source.is_file() {
        panic!(
            "the `sherpa` feature needs {ASSET}, which is not committed.\n\
             Fetch it once before building:\n\
             \n\
             \tscripts/fetch-silero-vad.sh\n\
             \n\
             The default (hermetic) build does not need it."
        );
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let dest: &Path = &out_dir.join("silero_vad.onnx");
    std::fs::copy(&source, dest).unwrap_or_else(|e| {
        panic!(
            "cannot stage {} into {}: {e}",
            source.display(),
            dest.display()
        )
    });
}
