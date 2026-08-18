//! Full NDJSON sessions driven in-process.
//!
//! These mirror the scenarios the daemon's test fake encodes, so the real
//! sidecar and the fake stay interchangeable from the daemon's point of view.

mod support;

use fermix_stt::protocol::{ENGINE, ErrorCode, Event, PROTOCOL_VERSION};
use support::{
    StubEngine, audio_op, drive, drive_raw, junk_fixture, model_dir, silence, tone, wav_fixture,
};

fn assert_hello(event: &Event) {
    let Event::Hello {
        protocol_version,
        engine,
        stt_version,
    } = event
    else {
        panic!("the first event must be hello, got {event:?}");
    };
    assert_eq!(*protocol_version, PROTOCOL_VERSION);
    assert_eq!(engine, ENGINE);
    assert_eq!(stt_version, "9.9.9");
}

#[test]
fn hello_is_the_first_line_even_with_no_input() {
    let events = drive(StubEngine::saying("hi"), "");
    assert_eq!(events.len(), 1);
    assert_hello(&events[0]);
}

#[test]
fn transcribe_decodes_and_replies_with_one_result() {
    let input = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n\
         {{\"op\":\"shutdown\"}}\n",
        wav_fixture(),
        model_dir()
    );
    let events = drive(StubEngine::saying("the quick brown fox"), &input);

    assert_eq!(events.len(), 2, "{events:?}");
    assert_hello(&events[0]);
    let Event::Result {
        id,
        text,
        duration_ms,
    } = &events[1]
    else {
        panic!("expected a result, got {:?}", events[1]);
    };
    assert_eq!(id, "b1");
    assert_eq!(text, "the quick brown fox");
    // The fixture is 300 ms of 44.1 kHz audio, resampled to 16 kHz.
    assert!(
        (295..=305).contains(duration_ms),
        "duration_ms was {duration_ms}"
    );
}

#[test]
fn an_unsupported_container_is_a_decode_failure() {
    let input = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n",
        junk_fixture(),
        model_dir()
    );
    let events = drive(StubEngine::saying("unused"), &input);

    let Event::Error { id, code, message } = &events[1] else {
        panic!("expected an error, got {:?}", events[1]);
    };
    assert_eq!(id, "b1");
    assert_eq!(*code, ErrorCode::DecodeFailed);
    assert!(!message.is_empty());
}

#[test]
fn a_missing_file_is_an_io_error() {
    let input = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"/nonexistent/x.wav\",\"model_dir\":\"{}\"}}\n",
        model_dir()
    );
    let events = drive(StubEngine::saying("unused"), &input);
    let Event::Error { code, .. } = &events[1] else {
        panic!("expected an error, got {:?}", events[1]);
    };
    assert_eq!(*code, ErrorCode::IoError);
}

#[test]
fn a_model_that_will_not_load_is_a_model_load_failure() {
    let input = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n",
        wav_fixture(),
        model_dir()
    );
    let events = drive(StubEngine::failing_to_load(), &input);
    let Event::Error { code, .. } = &events[1] else {
        panic!("expected an error, got {:?}", events[1]);
    };
    assert_eq!(*code, ErrorCode::ModelLoadFailed);
}

/// The daemon's 8 MiB reassembly budget must hold for a realistic long
/// transcript: one compact object, one trailing newline, no interior newline.
#[test]
fn a_long_transcript_is_delivered_as_one_line() {
    let long = "supercalifragilistic ".repeat(60_000);
    let input = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n",
        wav_fixture(),
        model_dir()
    );
    let raw = drive_raw(StubEngine::saying(&long), &input);

    let lines: Vec<&[u8]> = raw.split(|b| *b == b'\n').collect();
    // hello, result, and the empty tail after the final newline.
    assert_eq!(lines.len(), 3);
    assert!(lines[2].is_empty());
    assert!(
        lines[1].len() > 1_000_000,
        "line was {} bytes",
        lines[1].len()
    );
    assert!(lines[1].len() < 8 * 1024 * 1024);

    let event: Event = serde_json::from_slice(lines[1]).expect("the long line must be valid JSON");
    let Event::Result { text, .. } = event else {
        panic!("expected a result");
    };
    assert_eq!(text.len(), long.len());
}

fn stream_script(id: &str) -> String {
    let mut audio = silence(3);
    audio.extend(tone(8));
    audio.extend(silence(20));
    audio.extend(tone(8));

    let mut script = format!(
        "{{\"op\":\"stream_start\",\"id\":\"{id}\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    // 16384 samples is 32768 bytes: half the per-frame ceiling.
    for chunk in audio.chunks(16_384) {
        script.push_str(&audio_op(id, chunk));
        script.push('\n');
    }
    script.push_str(&format!("{{\"op\":\"stream_end\",\"id\":\"{id}\"}}\n"));
    script.push_str("{\"op\":\"shutdown\"}\n");
    script
}

#[test]
fn a_stream_reports_started_then_segments_then_done() {
    let events = drive(StubEngine::saying("hello there"), &stream_script("s1"));

    assert_hello(&events[0]);
    assert!(
        matches!(&events[1], Event::StreamStarted { id } if id == "s1"),
        "{:?}",
        events[1]
    );

    let segments: Vec<&Event> = events[2..events.len() - 1].iter().collect();
    assert_eq!(segments.len(), 2, "expected two utterances: {events:?}");

    let mut previous_end = 0;
    for event in &segments {
        let Event::Segment {
            id,
            text,
            t0_ms,
            t1_ms,
        } = event
        else {
            panic!("expected a segment, got {event:?}");
        };
        assert_eq!(id, "s1");
        assert_eq!(text, "hello there");
        assert!(t1_ms > t0_ms);
        assert!(*t0_ms >= previous_end, "segments overlap: {event:?}");
        previous_end = *t1_ms;
    }

    let last = events.last().expect("stream_done");
    assert!(
        matches!(last, Event::StreamDone { id, segments } if id == "s1" && *segments == 2),
        "{last:?}"
    );
}

#[test]
fn a_silent_stream_still_completes_with_zero_segments() {
    let id = "s1";
    let mut script = format!(
        "{{\"op\":\"stream_start\",\"id\":\"{id}\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    script.push_str(&audio_op(id, &silence(20)));
    script.push('\n');
    script.push_str(&format!("{{\"op\":\"stream_end\",\"id\":\"{id}\"}}\n"));

    let events = drive(StubEngine::saying("never spoken"), &script);
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(events[2], Event::StreamDone { segments: 0, .. }));
}

#[test]
fn a_second_stream_start_before_stream_done_is_refused() {
    let start = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s1\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    let second = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s2\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    let events = drive(StubEngine::saying("x"), &format!("{start}{second}"));

    assert!(matches!(&events[1], Event::StreamStarted { .. }));
    let Event::Error { id, code, .. } = &events[2] else {
        panic!("expected an error, got {:?}", events[2]);
    };
    assert_eq!(id, "s2");
    assert_eq!(*code, ErrorCode::BadRequest);
}

#[test]
fn a_batch_request_during_a_live_stream_is_refused() {
    let start = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s1\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    let batch = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n",
        wav_fixture(),
        model_dir()
    );
    let events = drive(StubEngine::saying("x"), &format!("{start}{batch}"));
    assert!(
        matches!(&events[2], Event::Error { id, code, .. } if id == "b1" && *code == ErrorCode::BadRequest),
        "{:?}",
        events[2]
    );
}

#[test]
fn a_stream_start_with_the_wrong_audio_shape_is_refused() {
    let input = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s1\",\"model_dir\":\"{}\",\
         \"sample_rate\":48000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    let events = drive(StubEngine::saying("x"), &input);
    assert!(
        matches!(&events[1], Event::Error { code, .. } if *code == ErrorCode::BadRequest),
        "{:?}",
        events[1]
    );
}

#[test]
fn audio_without_a_live_stream_is_refused() {
    let input = format!("{}\n", audio_op("s1", &tone(1)));
    let events = drive(StubEngine::saying("x"), &input);
    assert!(
        matches!(&events[1], Event::Error { id, code, .. } if id == "s1" && *code == ErrorCode::BadRequest),
        "{:?}",
        events[1]
    );
}

#[test]
fn audio_for_a_different_stream_id_is_refused() {
    let start = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s1\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    let input = format!("{start}{}\n", audio_op("s9", &tone(1)));
    let events = drive(StubEngine::saying("x"), &input);
    assert!(
        matches!(&events[2], Event::Error { id, code, .. } if id == "s9" && *code == ErrorCode::BadRequest),
        "{:?}",
        events[2]
    );
}

#[test]
fn an_oversize_audio_frame_is_refused() {
    let start = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s1\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    // 33_000 samples is 66_000 bytes, past the 65_536-byte frame ceiling.
    let input = format!("{start}{}\n", audio_op("s1", &vec![0i16; 33_000]));
    let events = drive(StubEngine::saying("x"), &input);
    let Event::Error { code, message, .. } = &events[2] else {
        panic!("expected an error, got {:?}", events[2]);
    };
    assert_eq!(*code, ErrorCode::BadRequest);
    assert!(message.contains("ceiling"), "{message}");
}

#[test]
fn malformed_base64_pcm_is_refused() {
    let start = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s1\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    let input = format!("{start}{{\"op\":\"audio\",\"id\":\"s1\",\"pcm\":\"not base64!!\"}}\n");
    let events = drive(StubEngine::saying("x"), &input);
    assert!(
        matches!(&events[2], Event::Error { code, .. } if *code == ErrorCode::BadRequest),
        "{:?}",
        events[2]
    );
}

#[test]
fn a_recognition_failure_ends_the_stream_with_an_error() {
    let events = drive(StubEngine::failing_to_recognize(), &stream_script("s1"));
    assert!(matches!(&events[1], Event::StreamStarted { .. }));
    let Event::Error { id, code, .. } = &events[2] else {
        panic!("expected an error, got {:?}", events[2]);
    };
    assert_eq!(id, "s1");
    assert_eq!(*code, ErrorCode::Internal);

    // The stream is gone. Anything the daemon still had in flight is refused,
    // loudly, and no stream_done ever appears.
    assert!(
        !events.iter().any(|e| matches!(e, Event::StreamDone { .. })),
        "{events:?}"
    );
    for event in &events[3..] {
        assert!(
            matches!(event, Event::Error { code, .. } if *code == ErrorCode::BadRequest),
            "{event:?}"
        );
    }
}

#[test]
fn an_unknown_op_is_refused_and_never_silent() {
    let events = drive(
        StubEngine::saying("x"),
        "{\"op\":\"teleport\",\"id\":\"z9\"}\n",
    );
    let Event::Error { id, code, message } = &events[1] else {
        panic!("expected an error, got {:?}", events[1]);
    };
    assert_eq!(id, "z9");
    assert_eq!(*code, ErrorCode::BadRequest);
    assert!(message.contains("unhandled op"), "{message}");
}

#[test]
fn a_non_json_line_is_refused() {
    let events = drive(StubEngine::saying("x"), "this is not json\n\n");
    assert_eq!(events.len(), 3, "{events:?}");
    for event in &events[1..] {
        assert!(
            matches!(event, Event::Error { code, .. } if *code == ErrorCode::BadRequest),
            "{event:?}"
        );
    }
}

#[test]
fn an_oversize_line_is_refused_and_the_session_continues() {
    let huge = "a".repeat(9 * 1024 * 1024);
    let input =
        format!("{{\"op\":\"audio\",\"id\":\"s1\",\"pcm\":\"{huge}\"}}\n{{\"op\":\"shutdown\"}}\n");
    let events = drive(StubEngine::saying("x"), &input);

    assert_eq!(events.len(), 2, "{events:?}");
    let Event::Error { code, message, .. } = &events[1] else {
        panic!("expected an error, got {:?}", events[1]);
    };
    assert_eq!(*code, ErrorCode::BadRequest);
    assert!(message.contains("8 MiB"), "{message}");
}

#[test]
fn shutdown_stops_reading_immediately() {
    let input = format!(
        "{{\"op\":\"shutdown\"}}\n\
         {{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n",
        wav_fixture(),
        model_dir()
    );
    let events = drive(StubEngine::saying("x"), &input);
    assert_eq!(events.len(), 1, "nothing after shutdown: {events:?}");
}

#[test]
fn two_batch_requests_in_a_row_each_get_one_reply() {
    let one = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n",
        wav_fixture(),
        model_dir()
    );
    let two = one.replace("\"b1\"", "\"b2\"");
    let events = drive(StubEngine::saying("ok"), &format!("{one}{two}"));

    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(&events[1], Event::Result { id, .. } if id == "b1"));
    assert!(matches!(&events[2], Event::Result { id, .. } if id == "b2"));
}

#[test]
fn a_stream_can_be_followed_by_a_batch_request() {
    let mut script = stream_script("s1").replace("{\"op\":\"shutdown\"}\n", "");
    script.push_str(&format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n",
        wav_fixture(),
        model_dir()
    ));
    let events = drive(StubEngine::saying("ok"), &script);
    assert!(
        matches!(events.last(), Some(Event::Result { id, .. }) if id == "b1"),
        "{:?}",
        events.last()
    );
}
