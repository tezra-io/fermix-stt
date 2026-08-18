//! Container decode against committed fixtures. No network, no large assets.

mod support;

use std::path::Path;

use fermix_stt::decode::{DecodeError, decode_file};
use support::{junk_fixture, wav_fixture};

#[test]
fn a_wav_fixture_decodes_to_16k_mono_pcm() {
    let pcm = decode_file(Path::new(&wav_fixture())).expect("the WAV fixture must decode");

    // 300 ms at 44.1 kHz resampled to 16 kHz is 4800 samples.
    assert_eq!(pcm.samples().len(), 4800);
    assert_eq!(pcm.duration_ms(), 300);
}

#[test]
fn the_decoded_tone_keeps_its_amplitude() {
    let pcm = decode_file(Path::new(&wav_fixture())).unwrap();
    // Skip the filter's settling region, then check the 440 Hz tone survived.
    let peak = pcm.samples()[1000..]
        .iter()
        .map(|s| s.unsigned_abs())
        .max()
        .expect("non-empty");
    assert!(peak > 12_000, "peak was {peak}");
    assert!(peak < 20_000, "peak was {peak}");
}

#[test]
fn a_file_that_is_not_a_container_is_refused() {
    let err = decode_file(Path::new(&junk_fixture())).unwrap_err();
    assert!(
        matches!(
            err,
            DecodeError::Unsupported { .. } | DecodeError::Failed { .. }
        ),
        "{err}"
    );
}

#[test]
fn a_missing_file_is_an_io_error() {
    let err = decode_file(Path::new("/nonexistent/fermix-stt/absent.ogg")).unwrap_err();
    assert!(matches!(err, DecodeError::Io { .. }), "{err}");
}

#[test]
fn a_directory_is_not_decodable() {
    let err = decode_file(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap_err();
    // Opening a directory succeeds on some platforms and fails on others; both
    // outcomes must surface as a refusal, never as silent empty audio.
    assert!(
        matches!(
            err,
            DecodeError::Io { .. } | DecodeError::Unsupported { .. } | DecodeError::Failed { .. }
        ),
        "{err}"
    );
}
