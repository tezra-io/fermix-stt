//! The shipped binary, spawned the way the daemon spawns it.
//!
//! These run against the default (null-engine) build: they prove framing,
//! ordering and exit status, not transcription quality.

mod support;

use std::io::Write;
use std::process::{Command, Stdio};

use fermix_stt::protocol::{ENGINE, ErrorCode, Event, PROTOCOL_VERSION};
use support::{audio_op, model_dir, silence, tone, wav_fixture};

/// Feed `input` to a freshly spawned sidecar and collect (events, exit code).
fn spawn_with(input: &str) -> (Vec<Event>, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fermix-stt"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the sidecar");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write ops");
    // Closing stdin is the daemon's EOF signal.
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait for the sidecar");
    let events = String::from_utf8(output.stdout)
        .expect("stdout must be UTF-8")
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("undecodable event {line}: {e}"))
        })
        .collect();

    (
        events,
        output
            .status
            .code()
            .expect("the sidecar must exit, not be signalled"),
    )
}

#[test]
fn hello_arrives_before_anything_else_and_eof_exits_zero() {
    let (events, code) = spawn_with("");
    assert_eq!(code, 0);
    assert_eq!(events.len(), 1);

    let Event::Hello {
        protocol_version,
        engine,
        stt_version,
    } = &events[0]
    else {
        panic!("expected hello, got {:?}", events[0]);
    };
    assert_eq!(*protocol_version, PROTOCOL_VERSION);
    assert_eq!(engine, ENGINE);
    assert_eq!(stt_version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn shutdown_exits_zero() {
    let (events, code) = spawn_with("{\"op\":\"shutdown\"}\n");
    assert_eq!(code, 0);
    assert_eq!(events.len(), 1, "{events:?}");
}

#[test]
fn a_batch_request_round_trips_through_the_binary() {
    let input = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\"model_dir\":\"{}\"}}\n\
         {{\"op\":\"shutdown\"}}\n",
        wav_fixture(),
        model_dir()
    );
    let (events, code) = spawn_with(&input);

    assert_eq!(code, 0);
    let Event::Result {
        id, duration_ms, ..
    } = &events[1]
    else {
        panic!("expected a result, got {:?}", events[1]);
    };
    assert_eq!(id, "b1");
    assert_eq!(*duration_ms, 300);
}

#[test]
fn a_stream_round_trips_through_the_binary() {
    let mut audio = silence(2);
    audio.extend(tone(8));

    let mut input = format!(
        "{{\"op\":\"stream_start\",\"id\":\"s1\",\"model_dir\":\"{}\",\
         \"sample_rate\":16000,\"format\":\"s16le\",\"channels\":1}}\n",
        model_dir()
    );
    for chunk in audio.chunks(16_384) {
        input.push_str(&audio_op("s1", chunk));
        input.push('\n');
    }
    input.push_str("{\"op\":\"stream_end\",\"id\":\"s1\"}\n{\"op\":\"shutdown\"}\n");

    let (events, code) = spawn_with(&input);
    assert_eq!(code, 0);
    assert!(matches!(&events[1], Event::StreamStarted { id } if id == "s1"));
    assert!(matches!(
        events.last(),
        Some(Event::StreamDone { segments: 1, .. })
    ));
}

#[test]
fn an_unknown_op_is_refused_by_the_binary() {
    let (events, code) = spawn_with("{\"op\":\"nope\",\"id\":\"q1\"}\n");
    assert_eq!(code, 0);
    assert!(
        matches!(&events[1], Event::Error { id, code, .. } if id == "q1" && *code == ErrorCode::BadRequest),
        "{:?}",
        events[1]
    );
}

#[test]
fn a_missing_model_directory_is_a_model_load_failure() {
    let input = format!(
        "{{\"op\":\"transcribe\",\"id\":\"b1\",\"path\":\"{}\",\
         \"model_dir\":\"/nonexistent/fermix-stt/models\"}}\n",
        wav_fixture()
    );
    let (events, code) = spawn_with(&input);
    assert_eq!(code, 0);
    assert!(
        matches!(&events[1], Event::Error { code, .. } if *code == ErrorCode::ModelLoadFailed),
        "{:?}",
        events[1]
    );
}

#[test]
fn the_binary_reports_its_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_fermix-stt"))
        .arg("--version")
        .output()
        .expect("run --version");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("fermix-stt {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn unexpected_arguments_exit_nonzero() {
    let output = Command::new(env!("CARGO_BIN_EXE_fermix-stt"))
        .arg("--transcribe-everything")
        .output()
        .expect("run with a bad argument");
    assert_eq!(output.status.code(), Some(2));
}
