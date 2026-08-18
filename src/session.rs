//! The dispatch loop: one request in flight, one reply per request.
//!
//! Every obligation the daemon encodes in its test fake lives here — hello
//! first, `bad_request` for anything unexpected (never silence), a terminal
//! event per request, and exit 0 on `shutdown` or stdin EOF.

use std::io::{BufRead, Write};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::asr::{Engine, Recognizer};
use crate::decode::{self, DecodeError};
use crate::ndjson::{EventWriter, Line, LineReader};
use crate::pcm;
use crate::protocol::{
    ErrorCode, Event, MAX_AUDIO_FRAME_BYTES, Op, STREAM_CHANNELS, STREAM_FORMAT, STREAM_SAMPLE_RATE,
};
use crate::vad::Segmenter;

/// Whether the loop should keep reading.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Flow {
    /// Read the next line.
    Continue,
    /// Exit 0.
    Stop,
}

/// The live stream, if one is open. Exactly one may exist at a time.
struct Stream {
    id: String,
    recognizer: Box<dyn Recognizer>,
    segmenter: Box<dyn Segmenter>,
    emitted: u64,
}

/// One sidecar process's protocol state.
pub struct Session<W: Write> {
    engine: Box<dyn Engine>,
    out: EventWriter<W>,
    stream: Option<Stream>,
}

impl<W: Write> Session<W> {
    /// A session over `writer`, backed by `engine`. Nothing is loaded yet.
    pub fn new(engine: Box<dyn Engine>, writer: W) -> Self {
        Self {
            engine,
            out: EventWriter::new(writer),
            stream: None,
        }
    }

    /// Emit `hello`. Must be the first line and must precede any model load.
    pub fn announce(&mut self, stt_version: &str) -> std::io::Result<()> {
        self.out.write(&Event::hello(stt_version))
    }

    /// Handle one framed line.
    pub fn handle(&mut self, line: Line) -> std::io::Result<Flow> {
        match line {
            Line::Eof => Ok(Flow::Stop),
            Line::Oversize => {
                self.refuse("", "line exceeds the 8 MiB protocol ceiling")?;
                Ok(Flow::Continue)
            }
            Line::NotUtf8 => {
                self.refuse("", "line is not valid UTF-8")?;
                Ok(Flow::Continue)
            }
            Line::Text(text) => self.handle_text(&text),
        }
    }

    fn handle_text(&mut self, text: &str) -> std::io::Result<Flow> {
        let op: Op = match serde_json::from_str(text) {
            Ok(op) => op,
            Err(e) => {
                let id = id_hint(text);
                self.refuse(&id, format!("unhandled op: {e}"))?;
                return Ok(Flow::Continue);
            }
        };

        match op {
            Op::Shutdown => Ok(Flow::Stop),
            Op::Transcribe {
                id,
                path,
                model_dir,
            } => {
                self.on_transcribe(&id, &path, &model_dir)?;
                Ok(Flow::Continue)
            }
            Op::StreamStart {
                id,
                model_dir,
                sample_rate,
                format,
                channels,
            } => {
                self.on_stream_start(&id, &model_dir, sample_rate, &format, channels)?;
                Ok(Flow::Continue)
            }
            Op::Audio { id, pcm } => {
                self.on_audio(&id, &pcm)?;
                Ok(Flow::Continue)
            }
            Op::StreamEnd { id } => {
                self.on_stream_end(&id)?;
                Ok(Flow::Continue)
            }
        }
    }

    fn on_transcribe(&mut self, id: &str, path: &str, model_dir: &str) -> std::io::Result<()> {
        if self.stream.is_some() {
            return self.refuse(id, "a stream is already in flight");
        }

        let pcm_data = match decode::decode_file(Path::new(path)) {
            Ok(pcm_data) => pcm_data,
            Err(e) => {
                let code = match e {
                    DecodeError::Io { .. } => ErrorCode::IoError,
                    DecodeError::Unsupported { .. } | DecodeError::Failed { .. } => {
                        ErrorCode::DecodeFailed
                    }
                };
                return self.out.write(&Event::error(id, code, e.to_string()));
            }
        };

        let mut recognizer = match self.engine.load(Path::new(model_dir)) {
            Ok(recognizer) => recognizer,
            Err(e) => {
                return self.out.write(&Event::error(id, e.code(), e.to_string()));
            }
        };

        let duration_ms = pcm_data.duration_ms();
        match recognizer.transcribe_batch(pcm_data.samples()) {
            Ok(text) => self.out.write(&Event::Result {
                id: id.to_string(),
                text,
                duration_ms,
            }),
            Err(e) => self.out.write(&Event::error(id, e.code(), e.to_string())),
        }
    }

    fn on_stream_start(
        &mut self,
        id: &str,
        model_dir: &str,
        sample_rate: u32,
        format: &str,
        channels: u16,
    ) -> std::io::Result<()> {
        if self.stream.is_some() {
            return self.refuse(id, "a stream is already in flight");
        }
        if sample_rate != STREAM_SAMPLE_RATE
            || format != STREAM_FORMAT
            || channels != STREAM_CHANNELS
        {
            return self.refuse(
                id,
                format!(
                    "stream must be {STREAM_SAMPLE_RATE} Hz {STREAM_FORMAT} \
                     x{STREAM_CHANNELS}, got {sample_rate} Hz {format} x{channels}"
                ),
            );
        }

        let segmenter = match self.engine.segmenter() {
            Ok(segmenter) => segmenter,
            Err(e) => {
                return self.out.write(&Event::error(id, e.code(), e.to_string()));
            }
        };
        let recognizer = match self.engine.load(Path::new(model_dir)) {
            Ok(recognizer) => recognizer,
            Err(e) => {
                return self.out.write(&Event::error(id, e.code(), e.to_string()));
            }
        };

        self.stream = Some(Stream {
            id: id.to_string(),
            recognizer,
            segmenter,
            emitted: 0,
        });
        self.out.write(&Event::StreamStarted { id: id.to_string() })
    }

    fn on_audio(&mut self, id: &str, payload: &str) -> std::io::Result<()> {
        if !self.stream_matches(id) {
            return self.refuse(id, "no live stream with that id");
        }

        let bytes = match BASE64.decode(payload) {
            Ok(bytes) => bytes,
            Err(e) => return self.refuse(id, format!("pcm is not valid base64: {e}")),
        };
        if bytes.len() > MAX_AUDIO_FRAME_BYTES {
            return self.refuse(
                id,
                format!(
                    "audio frame is {} bytes, over the {MAX_AUDIO_FRAME_BYTES}-byte ceiling",
                    bytes.len()
                ),
            );
        }
        let samples = match pcm::from_le_bytes(&bytes) {
            Ok(samples) => samples,
            Err(e) => return self.refuse(id, e.to_string()),
        };

        let pushed = {
            let stream = self.stream.as_mut().expect("checked above");
            stream.segmenter.push(&samples)
        };
        match pushed {
            Ok(utterances) => self.emit_segments(utterances),
            Err(e) => self.fail_stream(ErrorCode::Internal, e.to_string()),
        }
    }

    fn on_stream_end(&mut self, id: &str) -> std::io::Result<()> {
        if !self.stream_matches(id) {
            return self.refuse(id, "no live stream with that id");
        }

        let flushed = {
            let stream = self.stream.as_mut().expect("checked above");
            stream.segmenter.flush()
        };
        let utterances = match flushed {
            Ok(utterances) => utterances,
            Err(e) => return self.fail_stream(ErrorCode::Internal, e.to_string()),
        };
        self.emit_segments(utterances)?;

        let Some(stream) = self.stream.take() else {
            // emit_segments tore the stream down on a recognition error; that
            // error was already the terminal event for this stream.
            return Ok(());
        };
        self.out.write(&Event::StreamDone {
            id: stream.id,
            segments: stream.emitted,
        })
    }

    /// Recognize each utterance in order and emit a `segment` for every one
    /// that produced text. A recognition failure is terminal for the stream.
    fn emit_segments(&mut self, utterances: Vec<crate::vad::Utterance>) -> std::io::Result<()> {
        for utterance in utterances {
            let Some(stream) = self.stream.as_mut() else {
                return Ok(());
            };
            let id = stream.id.clone();
            let recognized = stream.recognizer.transcribe_segment(&utterance.pcm);

            let text = match recognized {
                Ok(text) => text,
                Err(e) => return self.fail_stream(e.code(), e.to_string()),
            };
            if text.trim().is_empty() {
                continue;
            }

            if let Some(stream) = self.stream.as_mut() {
                stream.emitted += 1;
            }
            self.out.write(&Event::Segment {
                id,
                text,
                t0_ms: utterance.t0_ms,
                t1_ms: utterance.t1_ms,
            })?;
        }
        Ok(())
    }

    /// Terminate the live stream with an error event.
    fn fail_stream(&mut self, code: ErrorCode, message: String) -> std::io::Result<()> {
        let Some(stream) = self.stream.take() else {
            return Ok(());
        };
        self.out.write(&Event::error(&stream.id, code, message))
    }

    fn stream_matches(&self, id: &str) -> bool {
        self.stream.as_ref().is_some_and(|s| s.id == id)
    }

    fn refuse(&mut self, id: &str, message: impl Into<String>) -> std::io::Result<()> {
        self.out
            .write(&Event::error(id, ErrorCode::BadRequest, message))
    }
}

/// Best-effort correlation id for a line that did not parse as an `Op`.
fn id_hint(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Announce, then dispatch until `shutdown` or EOF.
pub fn run<R: BufRead, W: Write>(
    engine: Box<dyn Engine>,
    reader: R,
    writer: W,
    stt_version: &str,
) -> std::io::Result<()> {
    let mut session = Session::new(engine, writer);
    session.announce(stt_version)?;

    let mut lines = LineReader::new(reader);
    loop {
        let line = lines.next_line()?;
        if session.handle(line)? == Flow::Stop {
            return Ok(());
        }
    }
}
