//! Wire types for the Fermix STT sidecar protocol.
//!
//! The contract is defined by the daemon and vendored into this repo at
//! `protocol/PROTOCOL.md` with byte-exact samples in `protocol/fixtures/`.
//! Nothing in this module may drift from those files; the tests at the bottom
//! round-trip every fixture through these types and compare bytes.

use serde::{Deserialize, Serialize};

/// The single integer version carried in `hello`. Pinned to `protocol/PROTOCOL.md`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The engine name the daemon expects in `hello`.
pub const ENGINE: &str = "sherpa-onnx";

/// The daemon reassembles NDJSON lines up to this length; beyond it, a line is
/// a protocol error. Applies in both directions.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Maximum raw PCM bytes carried by one `audio` frame, before base64.
pub const MAX_AUDIO_FRAME_BYTES: usize = 65536;

/// The sample rate, format and channel count the daemon streams at.
pub const STREAM_SAMPLE_RATE: u32 = 16000;
/// The only PCM format accepted on the stream.
pub const STREAM_FORMAT: &str = "s16le";
/// The only channel count accepted on the stream.
pub const STREAM_CHANNELS: u16 = 1;

/// Terminal error codes. The set is closed; the daemon rejects anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The ASR model directory could not be loaded.
    ModelLoadFailed,
    /// The container could not be demuxed or decoded to PCM.
    DecodeFailed,
    /// A filesystem or pipe operation failed.
    IoError,
    /// The daemon sent something this sidecar cannot honour.
    BadRequest,
    /// An unexpected failure inside the sidecar.
    Internal,
}

/// Frames the sidecar sends. Serialized as one compact JSON object per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// First line after spawn, before any model load.
    Hello {
        /// The wire version this build speaks.
        protocol_version: u32,
        /// The recognition backend, `"sherpa-onnx"`.
        engine: String,
        /// This sidecar's semver.
        stt_version: String,
    },
    /// Batch success.
    Result {
        /// Echoes the request id.
        id: String,
        /// The full transcript.
        text: String,
        /// Duration of the audio that was recognized.
        duration_ms: u64,
    },
    /// Stream mode accepted and the model is loaded.
    StreamStarted {
        /// Echoes the `stream_start` id.
        id: String,
    },
    /// One VAD-segmented recognition result, in order.
    Segment {
        /// Echoes the stream id.
        id: String,
        /// Transcript for this utterance.
        text: String,
        /// Utterance start, milliseconds from stream start.
        t0_ms: u64,
        /// Utterance end, milliseconds from stream start.
        t1_ms: u64,
    },
    /// Flush complete after `stream_end`.
    StreamDone {
        /// Echoes the stream id.
        id: String,
        /// How many `segment` events were emitted.
        segments: u64,
    },
    /// Terminal for that request or stream.
    Error {
        /// Echoes the failing request's id, empty when none could be read.
        id: String,
        /// The closed-set error code.
        code: ErrorCode,
        /// Human-readable detail; the daemon surfaces it to operators.
        message: String,
    },
}

impl Event {
    /// The `hello` line for this build.
    pub fn hello(stt_version: &str) -> Self {
        Event::Hello {
            protocol_version: PROTOCOL_VERSION,
            engine: ENGINE.to_string(),
            stt_version: stt_version.to_string(),
        }
    }

    /// A terminal error for `id`.
    pub fn error(id: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        Event::Error {
            id: id.to_string(),
            code,
            message: message.into(),
        }
    }
}

/// Frames the daemon sends. Deserialized from one JSON object per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Decode the container at `path` and recognize it in one shot.
    Transcribe {
        /// Correlation id to echo.
        id: String,
        /// Absolute path to the audio container.
        path: String,
        /// Absolute path to the ASR model directory.
        model_dir: String,
    },
    /// Enter stream mode. Exactly one live stream per process.
    StreamStart {
        /// Correlation id to echo.
        id: String,
        /// Absolute path to the ASR model directory.
        model_dir: String,
        /// Always 16000.
        sample_rate: u32,
        /// Always `"s16le"`.
        format: String,
        /// Always 1.
        channels: u16,
    },
    /// Base64 s16le PCM for the live stream.
    Audio {
        /// The live stream's id.
        id: String,
        /// Base64-encoded s16le PCM.
        pcm: String,
    },
    /// No more audio: flush the pending VAD run.
    StreamEnd {
        /// The live stream's id.
        id: String,
    },
    /// Exit 0 promptly.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn protocol_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("protocol")
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = protocol_dir().join("fixtures").join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Every vendored fixture must decode into an `Event` and re-encode to the
    /// exact same bytes, trailing newline included. This is the anti-drift gate
    /// between this repo and the daemon's export.
    #[test]
    fn fixtures_round_trip_byte_for_byte() {
        let names = [
            "hello.json",
            "result.json",
            "stream_started.json",
            "segment.json",
            "stream_done.json",
            "error.json",
        ];

        for name in names {
            let raw = fixture(name);
            let event: Event = serde_json::from_slice(&raw)
                .unwrap_or_else(|e| panic!("{name} does not decode as an Event: {e}"));

            let mut encoded = serde_json::to_vec(&event).expect("serialize");
            encoded.push(b'\n');

            assert_eq!(
                String::from_utf8_lossy(&encoded),
                String::from_utf8_lossy(&raw),
                "{name} did not round-trip byte-for-byte"
            );
        }
    }

    #[test]
    fn fixture_variants_decode_to_the_expected_shapes() {
        let hello: Event = serde_json::from_slice(&fixture("hello.json")).unwrap();
        assert_eq!(
            hello,
            Event::Hello {
                protocol_version: PROTOCOL_VERSION,
                engine: ENGINE.to_string(),
                stt_version: "0.1.0".to_string(),
            }
        );

        let error: Event = serde_json::from_slice(&fixture("error.json")).unwrap();
        let Event::Error { id, code, .. } = error else {
            panic!("error.json is not an Error event");
        };
        assert_eq!(id, "b1");
        assert_eq!(code, ErrorCode::DecodeFailed);

        let done: Event = serde_json::from_slice(&fixture("stream_done.json")).unwrap();
        assert_eq!(
            done,
            Event::StreamDone {
                id: "s1".to_string(),
                segments: 2,
            }
        );
    }

    /// `PROTOCOL_VERSION` is pinned to the number the vendored spec declares.
    #[test]
    fn protocol_version_matches_the_vendored_spec() {
        let spec =
            std::fs::read_to_string(protocol_dir().join("PROTOCOL.md")).expect("PROTOCOL.md");

        let marker = "A single integer, `protocol_version`, currently **";
        let start = spec
            .find(marker)
            .expect("PROTOCOL.md no longer states the protocol version in the expected sentence")
            + marker.len();
        let rest = &spec[start..];
        let end = rest.find("**").expect("unterminated version marker");
        let declared: u32 = rest[..end]
            .trim()
            .parse()
            .expect("declared protocol version is not an integer");

        assert_eq!(
            declared, PROTOCOL_VERSION,
            "protocol/PROTOCOL.md declares version {declared} but PROTOCOL_VERSION is {PROTOCOL_VERSION}"
        );
    }

    /// The line budget and the engine name are load-bearing constants that the
    /// spec states in prose; keep them honest.
    #[test]
    fn line_budget_and_engine_match_the_vendored_spec() {
        let spec =
            std::fs::read_to_string(protocol_dir().join("PROTOCOL.md")).expect("PROTOCOL.md");
        assert!(
            spec.contains("**8 MiB**"),
            "PROTOCOL.md no longer states the 8 MiB line budget"
        );
        assert_eq!(MAX_LINE_BYTES, 8 * 1024 * 1024);
        assert!(spec.contains("`engine` is `\"sherpa-onnx\"`"));
        assert!(
            spec.contains("≤ 65536 raw bytes per frame"),
            "PROTOCOL.md no longer states the 65536-byte audio frame budget"
        );
        assert_eq!(MAX_AUDIO_FRAME_BYTES, 65536);
    }

    #[test]
    fn ops_decode_from_the_shapes_the_spec_documents() {
        let cases: Vec<(&str, Op)> = vec![
            (
                r#"{"op":"transcribe","id":"b1","path":"/abs/input.ogg","model_dir":"/abs/models/dir"}"#,
                Op::Transcribe {
                    id: "b1".into(),
                    path: "/abs/input.ogg".into(),
                    model_dir: "/abs/models/dir".into(),
                },
            ),
            (
                r#"{"op":"stream_start","id":"s1","model_dir":"/abs/models/dir","sample_rate":16000,"format":"s16le","channels":1}"#,
                Op::StreamStart {
                    id: "s1".into(),
                    model_dir: "/abs/models/dir".into(),
                    sample_rate: 16000,
                    format: "s16le".into(),
                    channels: 1,
                },
            ),
            (
                r#"{"op":"audio","id":"s1","pcm":"AAA="}"#,
                Op::Audio {
                    id: "s1".into(),
                    pcm: "AAA=".into(),
                },
            ),
            (
                r#"{"op":"stream_end","id":"s1"}"#,
                Op::StreamEnd { id: "s1".into() },
            ),
            (r#"{"op":"shutdown"}"#, Op::Shutdown),
        ];

        for (line, expected) in cases {
            let parsed: Op = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(parsed, expected);
            let reencoded = serde_json::to_string(&parsed).unwrap();
            assert_eq!(reencoded, line, "op re-encode drifted");
        }
    }

    #[test]
    fn an_unknown_op_fails_to_decode() {
        let err = serde_json::from_str::<Op>(r#"{"op":"teleport","id":"x"}"#).unwrap_err();
        assert!(err.to_string().contains("teleport"), "{err}");
    }

    /// A long transcript must serialize to exactly one line: no pretty-printing,
    /// no embedded newline, one trailing `\n`.
    #[test]
    fn a_long_result_is_one_line() {
        let text = "word ".repeat(400_000);
        let event = Event::Result {
            id: "b1".into(),
            text,
            duration_ms: 1_800_000,
        };

        let mut encoded = serde_json::to_vec(&event).unwrap();
        encoded.push(b'\n');

        assert!(encoded.len() > 1_000_000);
        assert!(encoded.len() < MAX_LINE_BYTES);
        assert_eq!(encoded.iter().filter(|b| **b == b'\n').count(), 1);
        assert_eq!(encoded.last(), Some(&b'\n'));
    }
}
