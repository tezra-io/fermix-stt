# assets/

Build-time assets for the `sherpa` feature. Nothing here is committed except
this file.

- `silero_vad.onnx` — the Silero voice-activity model, embedded into the binary
  by `build.rs` so the sidecar never downloads anything at runtime. Fetch it
  with `scripts/fetch-silero-vad.sh` (checksum-pinned). The default build does
  not need it and `build.rs` stages nothing.

The Parakeet TDT ASR model is **not** an asset: the Fermix daemon's ModelStore
manages it and passes its directory as `model_dir` on every request.
