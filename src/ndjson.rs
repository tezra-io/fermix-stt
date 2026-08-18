//! NDJSON framing over stdio.
//!
//! One JSON object per line, UTF-8, `\n`-terminated, no pretty-printing, in
//! both directions. Lines are routinely large (a 64 KiB PCM chunk is ~87 KB of
//! base64), so the reader streams into a growable buffer with a hard ceiling
//! rather than trusting the peer.

use std::io::{BufRead, Write};

use crate::protocol::{Event, MAX_LINE_BYTES};

/// One read from the peer.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// A complete line, newline stripped. May be empty.
    Text(String),
    /// The line exceeded [`MAX_LINE_BYTES`] and was discarded up to its
    /// terminating newline. The stream stays framed; the caller reports it.
    Oversize,
    /// The line was not valid UTF-8 and was discarded up to its newline.
    NotUtf8,
    /// The peer closed stdin.
    Eof,
}

/// Reads NDJSON lines with a hard per-line ceiling.
pub struct LineReader<R: BufRead> {
    inner: R,
    max_bytes: usize,
}

impl<R: BufRead> LineReader<R> {
    /// A reader with the protocol's [`MAX_LINE_BYTES`] ceiling.
    pub fn new(inner: R) -> Self {
        Self::with_max_bytes(inner, MAX_LINE_BYTES)
    }

    /// A reader with an explicit ceiling. `max_bytes` must be non-zero.
    pub fn with_max_bytes(inner: R, max_bytes: usize) -> Self {
        assert!(max_bytes > 0, "max_bytes must be non-zero");
        Self { inner, max_bytes }
    }

    /// Read the next line. Consumes bytes up to and including the newline even
    /// when the line is rejected, so framing survives a bad line.
    pub fn next_line(&mut self) -> std::io::Result<Line> {
        let mut buf: Vec<u8> = Vec::new();
        let mut overflowed = false;

        loop {
            let chunk = self.inner.fill_buf()?;
            if chunk.is_empty() {
                break;
            }

            match chunk.iter().position(|b| *b == b'\n') {
                Some(idx) => {
                    Self::extend(&mut buf, &chunk[..idx], self.max_bytes, &mut overflowed);
                    self.inner.consume(idx + 1);
                    return Ok(Self::finish(buf, overflowed));
                }
                None => {
                    let len = chunk.len();
                    Self::extend(&mut buf, chunk, self.max_bytes, &mut overflowed);
                    self.inner.consume(len);
                }
            }
        }

        // EOF. A trailing fragment without a newline is still a line.
        if buf.is_empty() && !overflowed {
            return Ok(Line::Eof);
        }
        Ok(Self::finish(buf, overflowed))
    }

    fn extend(buf: &mut Vec<u8>, chunk: &[u8], max_bytes: usize, overflowed: &mut bool) {
        if *overflowed {
            return;
        }
        if buf.len() + chunk.len() > max_bytes {
            *overflowed = true;
            buf.clear();
            buf.shrink_to_fit();
            return;
        }
        buf.extend_from_slice(chunk);
    }

    fn finish(buf: Vec<u8>, overflowed: bool) -> Line {
        if overflowed {
            return Line::Oversize;
        }
        match String::from_utf8(buf) {
            Ok(text) => Line::Text(text),
            Err(_) => Line::NotUtf8,
        }
    }
}

/// Writes events as compact NDJSON, flushing every line so the daemon sees a
/// reply as soon as it exists.
pub struct EventWriter<W: Write> {
    inner: W,
}

impl<W: Write> EventWriter<W> {
    /// Wrap a sink.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Serialize one event, terminate it with a newline, and flush.
    pub fn write(&mut self, event: &Event) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(event).map_err(std::io::Error::other)?;
        line.push(b'\n');
        self.inner.write_all(&line)?;
        self.inner.flush()
    }

    /// Consume the writer and return the sink (used by tests).
    pub fn into_inner(self) -> W {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ErrorCode;
    use std::io::BufReader;

    fn read_all(input: &str, max: usize) -> Vec<Line> {
        let mut reader = LineReader::with_max_bytes(BufReader::new(input.as_bytes()), max);
        let mut out = Vec::new();
        loop {
            let line = reader.next_line().expect("read");
            let done = line == Line::Eof;
            out.push(line);
            if done {
                return out;
            }
        }
    }

    #[test]
    fn splits_lines_and_reports_eof() {
        let lines = read_all("{\"a\":1}\n{\"b\":2}\n", 1024);
        assert_eq!(
            lines,
            vec![
                Line::Text("{\"a\":1}".into()),
                Line::Text("{\"b\":2}".into()),
                Line::Eof,
            ]
        );
    }

    #[test]
    fn a_trailing_fragment_without_a_newline_is_a_line() {
        let lines = read_all("{\"a\":1}", 1024);
        assert_eq!(lines, vec![Line::Text("{\"a\":1}".into()), Line::Eof]);
    }

    #[test]
    fn an_empty_line_is_reported_not_skipped() {
        let lines = read_all("\n{\"a\":1}\n", 1024);
        assert_eq!(
            lines,
            vec![
                Line::Text(String::new()),
                Line::Text("{\"a\":1}".into()),
                Line::Eof,
            ]
        );
    }

    #[test]
    fn an_oversize_line_is_discarded_and_framing_survives() {
        let huge = "x".repeat(200);
        let input = format!("{huge}\n{{\"ok\":1}}\n");
        let lines = read_all(&input, 64);
        assert_eq!(
            lines,
            vec![Line::Oversize, Line::Text("{\"ok\":1}".into()), Line::Eof]
        );
    }

    #[test]
    fn an_oversize_line_spanning_many_buffer_fills_is_still_one_line() {
        // 4 MiB of payload read through a small BufReader: the reader must not
        // accumulate it and must resynchronize on the next newline.
        let huge = "y".repeat(4 * 1024 * 1024);
        let input = format!("{huge}\n{{\"ok\":1}}\n");
        let mut reader = LineReader::with_max_bytes(
            BufReader::with_capacity(8 * 1024, input.as_bytes()),
            1024 * 1024,
        );
        assert_eq!(reader.next_line().unwrap(), Line::Oversize);
        assert_eq!(reader.next_line().unwrap(), Line::Text("{\"ok\":1}".into()));
        assert_eq!(reader.next_line().unwrap(), Line::Eof);
    }

    #[test]
    fn a_line_at_exactly_the_ceiling_is_accepted() {
        let exact = "z".repeat(64);
        let lines = read_all(&format!("{exact}\n"), 64);
        assert_eq!(lines, vec![Line::Text(exact), Line::Eof]);
    }

    #[test]
    fn invalid_utf8_is_reported_and_framing_survives() {
        let mut input: Vec<u8> = vec![0x7b, 0xff, 0xfe, 0x7d];
        input.push(b'\n');
        input.extend_from_slice(b"{\"ok\":1}\n");
        let mut reader = LineReader::with_max_bytes(BufReader::new(&input[..]), 1024);
        assert_eq!(reader.next_line().unwrap(), Line::NotUtf8);
        assert_eq!(reader.next_line().unwrap(), Line::Text("{\"ok\":1}".into()));
    }

    #[test]
    fn writes_compact_lines_terminated_by_one_newline() {
        let mut writer = EventWriter::new(Vec::new());
        writer
            .write(&Event::StreamStarted { id: "s1".into() })
            .unwrap();
        writer
            .write(&Event::error("s1", ErrorCode::Internal, "boom"))
            .unwrap();

        let out = String::from_utf8(writer.into_inner()).unwrap();
        assert_eq!(
            out,
            "{\"event\":\"stream_started\",\"id\":\"s1\"}\n\
             {\"event\":\"error\",\"id\":\"s1\",\"code\":\"internal\",\"message\":\"boom\"}\n"
        );
    }
}
