//! Entry point. Reads NDJSON ops on stdin, writes NDJSON events on stdout.
//!
//! stdout carries the protocol and nothing else; diagnostics go to stderr,
//! which the daemon does not read.

use std::io::{self, BufReader};
use std::process::ExitCode;

use fermix_stt::{VERSION, engine, session};

/// Exit code for a usage error. Protocol errors are reported on the wire, not
/// through the exit status.
const EXIT_USAGE: u8 = 2;
/// Exit code for a failed stdin/stdout pipe.
const EXIT_IO: u8 = 3;

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)) {
        Ok(Mode::Serve) => serve(),
        Ok(Mode::Version) => {
            println!("fermix-stt {VERSION}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("fermix-stt: {message}");
            eprintln!("usage: fermix-stt [--version]  (speaks NDJSON on stdin/stdout)");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

/// What this invocation should do.
enum Mode {
    /// Speak the protocol on stdio.
    Serve,
    /// Print the version and exit.
    Version,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Mode, String> {
    let args: Vec<String> = args.collect();
    match args.len() {
        0 => Ok(Mode::Serve),
        1 if args[0] == "--version" || args[0] == "-V" => Ok(Mode::Version),
        _ => Err(format!("unexpected arguments: {}", args.join(" "))),
    }
}

fn serve() -> ExitCode {
    let engine = engine::compiled_in();
    eprintln!("fermix-stt {VERSION} starting, engine={}", engine.name());

    let stdin = BufReader::new(io::stdin().lock());
    let stdout = io::stdout().lock();

    match session::run(engine, stdin, stdout, VERSION) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fermix-stt: pipe failed: {e}");
            ExitCode::from(EXIT_IO)
        }
    }
}
