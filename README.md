# fermix-stt

Local speech-to-text sidecar for the [Fermix](https://github.com/tezra-io/fermix)
daemon. Fermix spawns it as a child process and speaks NDJSON over stdin/stdout;
`fermix-stt` decodes an audio container to 16 kHz mono PCM and runs local ASR —
batch (a whole voice note) and streaming (VAD-segmented, for a live mic).

Nothing is listened on, no socket is opened, and no model is ever downloaded at
runtime. The ASR model directory arrives on the wire; the VAD model is compiled
into the binary.

| | |
|---|---|
| Wire | NDJSON on stdio, `protocol_version` 1 |
| Container decode | [symphonia](https://github.com/pdeljanov/Symphonia) — ogg, mp3, m4a, mp4, wav. Never ffmpeg. |
| Recognition | [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) + NeMo Parakeet TDT 0.6B v3 int8 |
| Segmentation | Silero VAD, embedded in the binary |

## The protocol is not ours

`protocol/` is the daemon's export, vendored verbatim: `PROTOCOL.md` plus
byte-exact `fixtures/*.json`. **Do not edit anything under `protocol/`.** Fermix
defines the wire; this repo implements it, and the daemon refuses a mismatched
`hello` and tears the process down.

`src/protocol.rs` is tested against those fixtures: every one must decode into
this crate's types and re-encode to the *same bytes*, trailing newline included,
and `PROTOCOL_VERSION` is parsed back out of `PROTOCOL.md`. A rename or a type
change on either side fails CI before it ships.

## Build and test

The default build is hermetic — pure Rust, no native library, no model, no
network:

```sh
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

The default binary links the **null recognizer**: it speaks the entire protocol,
decodes containers for real, segments a live stream for real, and returns a
placeholder string instead of a transcript. It is a conformance harness and a
CI target, not a transcription product. Every test in this repo runs against it
or against a stub engine, which is why the suite needs no model and no network.

### Real recognition

```sh
scripts/fetch-silero-vad.sh          # once: stages assets/silero_vad.onnx
cargo build --release --features sherpa
```

`--features sherpa` swaps in the sherpa-onnx engine. That pulls the sherpa-onnx
native library (its build script downloads a prebuilt static archive) and embeds
`assets/silero_vad.onnx` via `build.rs`, which fails loudly if the asset is
absent. Released binaries are always built with this feature; that is what
`release.yml` does.

At runtime the engine expects `model_dir` to contain a Parakeet TDT export,
by exact filename — a missing file is a loud `model_load_failed`, never a
silent guess at another layout:

```
encoder.int8.onnx
decoder.int8.onnx
joiner.int8.onnx
tokens.txt
```

## Owner gate

Two things cannot be proven by this repo's hermetic suite, and both are on the
owner before a release ships:

1. **A `--features sherpa` build on each release target.** The native library is
   downloaded and linked by the sherpa-onnx build script; CI runs default
   features only, so the first real compile of the engine happens in
   `release.yml`.
2. **The RTF spike.** Run a real Parakeet model over a known clip on the target
   hardware and record the real-time factor (wall seconds ÷ audio seconds) for
   both batch and streaming. This is the acceptance gate: the daemon's batch
   budget is 300 s and its stream open shares the 10 s hello budget, so a cold
   model load must stay comfortably under 10 s and streaming RTF must stay well
   below 1.0 on the slowest supported machine. A build that links is not a build
   that keeps up.

## Paired release with fermix

`fermix-stt` and `fermix` ship as a coupled pair, the same way the compux
sidecar does.

- The wire `protocol_version` pins them. There is no negotiation: the daemon
  refuses any other version and tears the sidecar down. Bump it only on a
  wire-incompatible change, in lockstep with the daemon's `@protocol_version`,
  and re-vendor `protocol/` here in the same change.
- **Release order is sidecar first.** Tag `v<version>` here, let `release.yml`
  build the four targets, then pin that tag *and* the per-target `sha256` in
  fermix's downloader. Fermix pins a tag, not a range — an installed fermix
  keeps downloading the sidecar it was built against.
- Regenerating the pins is a normal PR in the fermix repo. Never hand-write a
  checksum: take it from the release artifacts.

## Architecture

```
src/
  main.rs        entry point; stdout is the protocol, stderr is diagnostics
  session.rs     dispatch loop — one request in flight, one reply per request
  ndjson.rs      line framing both ways, with the 8 MiB ceiling
  protocol.rs    serde types for every event and op (+ the fixture round-trip)
  decode.rs      symphonia container → mono PCM
  resample.rs    anti-aliased conversion to 16 kHz
  pcm.rs         s16le bytes ↔ i16 ↔ normalized f32
  vad.rs         Segmenter trait + the hermetic energy segmenter
  asr.rs         Engine / Recognizer traits
  engine/
    null.rs      the hermetic backend (default build)
    sherpa.rs    sherpa-onnx + Silero (feature `sherpa`)
```

Exactly one engine is compiled into any binary. There is no runtime probing and
no degrade-to-something-else path: if the model will not load, the request fails
with `model_load_failed` and the daemon says so.

### Obligations the suite pins

- `hello` is the first line, before any model touches disk.
- `transcribe` → decode + recognize → one `result`, or one `error`.
- `stream_start` → load → `stream_started`; `audio` feeds the VAD;
  `stream_end` flushes → `segment`s → `stream_done{segments:N}`.
- A second `stream_start` before `stream_done`, an unknown op, a malformed
  line, an oversize line, a bad PCM frame — every one gets an `error` with code
  `bad_request`. Nothing is ever ignored silently.
- A long transcript stays one compact JSON object plus one `\n`, well inside the
  daemon's 8 MiB line reassembly.
- `shutdown` or stdin EOF exits 0. A crash exits non-zero.

## License

MIT — see [LICENSE](LICENSE).
