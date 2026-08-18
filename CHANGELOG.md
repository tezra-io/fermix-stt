# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

The wire `protocol_version` is versioned separately from this crate; see
`protocol/PROTOCOL.md`.

## [Unreleased]

### 0.1.0

First release. Implements `protocol_version` 1.

#### Added

- NDJSON sidecar over stdin/stdout: `hello`, `result`, `stream_started`,
  `segment`, `stream_done`, `error` out; `transcribe`, `stream_start`, `audio`,
  `stream_end`, `shutdown` in.
- Batch transcription: container decode plus one-shot recognition, replying with
  one `result` carrying the recognized audio duration.
- Streaming transcription: VAD-segmented recognition, one `segment` per
  utterance with times measured from stream start, then `stream_done`.
- In-process container decode via symphonia (ogg, mp3, m4a, mp4, wav) down-mixed
  to mono and resampled to 16 kHz through an anti-aliased path. No ffmpeg.
- sherpa-onnx engine behind the `sherpa` cargo feature: NeMo Parakeet TDT
  0.6B v3 int8 for recognition, Silero VAD embedded in the binary by `build.rs`.
- Hermetic default build with a null recognizer and an energy segmenter, so
  `cargo build` and `cargo test` need no native library, model, or network.
- Codec tests that round-trip every vendored `protocol/fixtures/*.json` to the
  same bytes, and pin `PROTOCOL_VERSION` to `protocol/PROTOCOL.md`.
- Conformance tests covering the daemon's fake-sidecar scenarios: batch success,
  decode failure, model-load failure, stream lifecycle, duplicate
  `stream_start`, unknown op, malformed and oversize lines, long transcripts,
  and exit 0 on `shutdown` or EOF.
- `scripts/fetch-silero-vad.sh` — checksum-pinned fetch of the embedded VAD
  model for `--features sherpa` builds.
- CI (fmt, clippy, test on macOS arm64 and Linux) and a tagged release workflow
  producing `fermix-stt` binaries for macos-aarch64, macos-x86_64,
  linux-x86_64 and linux-aarch64 with per-target checksums.
