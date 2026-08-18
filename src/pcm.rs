//! Conversions between the three PCM shapes this sidecar handles: the s16le
//! bytes on the wire, the `i16` samples it buffers, and the normalized `f32`
//! samples every engine consumes.

/// Why a wire PCM frame could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PcmError {
    /// s16le needs two bytes per sample.
    #[error("s16le payload has an odd length ({0} bytes)")]
    OddLength(usize),
}

/// Decode little-endian signed 16-bit samples.
pub fn from_le_bytes(bytes: &[u8]) -> Result<Vec<i16>, PcmError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(PcmError::OddLength(bytes.len()));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

/// Normalize `i16` samples to `[-1.0, 1.0]`.
pub fn to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|s| *s as f32 / i16::MAX as f32).collect()
}

/// Clamp and scale normalized samples back to `i16`.
pub fn from_f32(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|s| to_i16(*s)).collect()
}

/// Clamp and scale one normalized sample to `i16`.
pub fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_pairs_become_samples() {
        assert_eq!(
            from_le_bytes(&[0x00, 0x00, 0xff, 0x7f, 0x01, 0x80]).unwrap(),
            vec![0, 32767, -32767]
        );
    }

    #[test]
    fn an_odd_payload_is_rejected() {
        let err = from_le_bytes(&[0x00]).unwrap_err();
        assert!(err.to_string().contains("odd length"), "{err}");
    }

    #[test]
    fn an_empty_payload_is_an_empty_frame() {
        assert!(from_le_bytes(&[]).unwrap().is_empty());
    }

    #[test]
    fn float_conversion_round_trips_within_one_lsb() {
        let original: Vec<i16> = vec![0, 1, -1, 12345, -12345, i16::MAX, -i16::MAX];
        let restored = from_f32(&to_f32(&original));
        for (a, b) in original.iter().zip(restored.iter()) {
            assert!((a - b).abs() <= 1, "{a} vs {b}");
        }
    }

    #[test]
    fn out_of_range_floats_clamp() {
        assert_eq!(to_i16(9.0), i16::MAX);
        assert_eq!(to_i16(-9.0), -i16::MAX);
    }
}
