# Fermix STT sidecar protocol

The wire contract between the Fermix daemon and the `fermix-stt` sidecar — a
Rust process that decodes an audio container to PCM and runs local speech
recognition (sherpa-onnx + Parakeet TDT + Silero VAD), returning transcript text
and, in stream mode, VAD-segmented recognition results.

**Source of truth:** `FermixCore.Transcription.Local.Sidecar`
(`lib/fermix_core/transcription/local/sidecar.ex`) and
`FermixCore.Transcription.Local.StreamSession` in the fermix repo. This file and
`fixtures/*.json` are the machine-readable export of those modules; the fixtures
are the exact bytes the daemon's codec tests decode. This repo **vendors this
file and the fixtures** and tests its own codec against them, so a drift on
either side fails CI. Fermix defines the protocol; the sidecar implements it.

## Transport

- **Channel:** the sidecar's stdin/stdout, spawned by the daemon as an Erlang
  Port. Nothing is listened on and no socket is opened.
- **Framing:** **NDJSON** — one JSON object per line, UTF-8, `\n`-terminated, no
  pretty-printing — in both directions. PCM rides base64 inside `audio` frames.
  At 16 kHz mono s16le that is ~43 KB/s over a local pipe, so a binary framing
  would save nothing worth running two framing disciplines on one pipe. (The
  compux sidecar set this precedent.)
- Lines are routinely long: a 64 KB PCM chunk is ~87 KB of base64 and a batch
  transcript can reach ~1 MB. The daemon reassembles across read boundaries up
  to **8 MiB** (`@max_line_bytes`); a line past that is a protocol error. Keep
  every line one compact JSON object followed by exactly one `\n`.
- **stderr is not part of the channel.** The daemon does not read it. Emit
  diagnostics there for a human tailing the process, or as nothing at all — a
  stray byte on stdout desyncs NDJSON parsing.

## Versioning

A single integer, `protocol_version`, currently **1**. The sidecar declares it
in `hello`; the daemon refuses anything but its own version and tears down. There
is no negotiation — the sidecar binary is pinned by the fermix build that
downloads it, so both halves ship together. Bump this only on a wire-incompatible
change, in lockstep with the daemon's `@protocol_version`.

## Frames the sidecar sends (each carries `"event"`)

```json
{"event":"hello","protocol_version":1,"engine":"sherpa-onnx","stt_version":"0.1.0"}
{"event":"result","id":"b1","text":"The quick brown fox jumps over the lazy dog.","duration_ms":1840}
{"event":"stream_started","id":"s1"}
{"event":"segment","id":"s1","text":"the quick brown fox","t0_ms":0,"t1_ms":1200}
{"event":"stream_done","id":"s1","segments":2}
{"event":"error","id":"b1","code":"decode_failed","message":"unsupported container: audio/amr"}
```

| Event | Fields | Semantics |
|---|---|---|
| `hello` | `protocol_version`, `engine`, `stt_version` | MUST be the first line, within **10 s** of spawn (before model load — see below). `model` is NOT in hello; models load per request. `engine` is `"sherpa-onnx"`. |
| `result` | `id`, `text`, `duration_ms` | Batch success. `id` echoes the request. `duration_ms` = audio duration recognized. |
| `stream_started` | `id` | Stream mode accepted and the model is loaded; ready for `audio`. |
| `segment` | `id`, `text`, `t0_ms`, `t1_ms` | One Silero-VAD-segmented recognition result. `t0_ms`/`t1_ms` are milliseconds **from stream start**. Emitted in order. |
| `stream_done` | `id`, `segments` | Flush complete after `stream_end`. `segments` = count emitted. |
| `error` | `id`, `code`, `message` | Terminal for that request/stream. `code` ∈ `model_load_failed`, `decode_failed`, `io_error`, `bad_request`, `internal`. |

The daemon treats an event other than the one it awaits as a protocol error
(`{:unexpected_event, …}`), so never interleave a `segment` into a batch reply or
a `result` into a stream.

## Frames the daemon sends (each carries `"op"`)

```json
{"op":"transcribe","id":"b1","path":"/abs/input.ogg","model_dir":"/abs/models/dir"}
{"op":"stream_start","id":"s1","model_dir":"/abs/models/dir","sample_rate":16000,"format":"s16le","channels":1}
{"op":"audio","id":"s1","pcm":"<base64 s16le>"}
{"op":"stream_end","id":"s1"}
{"op":"shutdown"}
```

| Op | Fields | Meaning |
|---|---|---|
| `transcribe` | `id`, `path`, `model_dir` | Batch: decode the container at `path` (symphonia: ogg/mp3/m4a/mp4/wav) → PCM → one-shot recognition → one `result` (or `error`). |
| `stream_start` | `id`, `model_dir`, `sample_rate` (16000), `format` (`"s16le"`), `channels` (1) | Enter stream mode. Exactly one live stream per process; a second `stream_start` before `stream_done` → `error` `bad_request`. Reply `stream_started` once the model is loaded. |
| `audio` | `id`, `pcm` | Base64 s16le PCM for the live stream, ≤ 65536 raw bytes per frame. Feed Silero VAD; emit a `segment` per detected utterance. |
| `stream_end` | `id` | No more audio: flush the pending VAD run → remaining `segment`s → `stream_done`. |
| `shutdown` | — | Exit 0 promptly. stdin EOF means the same. |

The daemon uses fixed correlation ids: `"b1"` for the batch call and `"s1"` for
the stream. Echo the received `id` in every event.

## Obligations

- **hello within 10 s of spawn, before loading any model.** The daemon's hello
  deadline covers process start only; model load happens per `transcribe` /
  `stream_start` and is bounded separately (batch 300 s, stream start shares the
  hello budget — keep cold model load comfortably under 10 s or the stream open
  will time out).
- **Silero VAD ships embedded in the binary.** Fermix's ModelStore manages only
  the ASR model directory passed as `model_dir`; the sidecar does not download
  anything.
- **One request in flight at a time** (one batch, or one live stream). An
  unknown `op` → `error` `bad_request` (never silent). A crash → non-zero exit;
  the daemon reads the exit status.
- **No container decoding via ffmpeg** — decode ogg/mp3/m4a/mp4/wav in-process
  (symphonia). MP4/M4A video-note containers must decode too.
- Exit 0 on `shutdown` or stdin EOF; close cleanly so the daemon's port teardown
  is a formality.

## Fixtures

`fixtures/*.json` are the exact bytes for `hello`, `result`, `stream_started`,
`segment`, `stream_done`, and `error`. This repo's codec test asserts each
round-trips through the sidecar's own serializer/deserializer to the same bytes,
so a field rename or type change here breaks CI before it ships.
