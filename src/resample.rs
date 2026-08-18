//! Sample-rate conversion to the 16 kHz the ASR model expects.
//!
//! Downsampling runs a fourth-order Butterworth low-pass (two cascaded biquads)
//! at 0.45 × 16 kHz before linear interpolation, so energy above the new
//! Nyquist is attenuated instead of folding back into the speech band.
//! Upsampling needs no such filter and interpolates directly.

/// Target rate for every recognizer in this sidecar.
pub const TARGET_RATE: u32 = 16_000;

/// Highest input rate accepted. Above this the input is not plausible audio.
pub const MAX_INPUT_RATE: u32 = 768_000;

/// Resample mono `input` from `src_rate` to [`TARGET_RATE`].
///
/// # Panics
/// Panics if `src_rate` is zero or above [`MAX_INPUT_RATE`]; callers validate
/// the rate against the container before reaching here.
pub fn to_target_rate(input: &[f32], src_rate: u32) -> Vec<f32> {
    assert!(src_rate > 0, "sample rate must be non-zero");
    assert!(
        src_rate <= MAX_INPUT_RATE,
        "sample rate {src_rate} above the {MAX_INPUT_RATE} ceiling"
    );

    if src_rate == TARGET_RATE || input.is_empty() {
        return input.to_vec();
    }

    if src_rate > TARGET_RATE {
        let cutoff = 0.45 * TARGET_RATE as f32;
        let filtered = low_pass(input, src_rate as f32, cutoff);
        return interpolate(&filtered, src_rate);
    }

    interpolate(input, src_rate)
}

/// Linear interpolation onto the target grid.
fn interpolate(input: &[f32], src_rate: u32) -> Vec<f32> {
    let ratio = TARGET_RATE as f64 / src_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    if out_len == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(out_len);
    let step = 1.0 / ratio;
    let last = input.len() - 1;

    for n in 0..out_len {
        let pos = n as f64 * step;
        let i = pos as usize;
        if i >= last {
            out.push(input[last]);
            continue;
        }
        let frac = (pos - i as f64) as f32;
        out.push(input[i] + (input[i + 1] - input[i]) * frac);
    }
    out
}

/// Two cascaded Butterworth biquads (Q = 1/√2 each) run forward once.
fn low_pass(input: &[f32], sample_rate: f32, cutoff: f32) -> Vec<f32> {
    let biquad = Biquad::low_pass(sample_rate, cutoff);
    let mut stage_one = biquad.clone();
    let mut stage_two = biquad;

    input
        .iter()
        .map(|x| stage_two.step(stage_one.step(*x)))
        .collect()
}

/// A direct-form-I biquad section.
#[derive(Clone)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// RBJ cookbook low-pass at `cutoff` with Q = 1/√2.
    fn low_pass(sample_rate: f32, cutoff: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let w0 = 2.0 * std::f32::consts::PI * (cutoff / sample_rate);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);

        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cos_w0) / 2.0) / a0,
            b1: (1.0 - cos_w0) / a0,
            b2: ((1.0 - cos_w0) / 2.0) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn step(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, millis: u32) -> Vec<f32> {
        let n = (rate as u64 * millis as u64 / 1000) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    /// Peak amplitude over the second half, after filter settling.
    fn tail_peak(samples: &[f32]) -> f32 {
        let start = samples.len() / 2;
        samples[start..]
            .iter()
            .fold(0.0f32, |acc, s| acc.max(s.abs()))
    }

    #[test]
    fn a_matching_rate_is_returned_unchanged() {
        let input = sine(440.0, 16_000, 50);
        assert_eq!(to_target_rate(&input, 16_000), input);
    }

    #[test]
    fn downsampling_produces_the_expected_length() {
        let input = sine(440.0, 48_000, 1000);
        let out = to_target_rate(&input, 48_000);
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn upsampling_produces_the_expected_length() {
        let input = sine(440.0, 8_000, 1000);
        let out = to_target_rate(&input, 8_000);
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn an_in_band_tone_survives_downsampling() {
        let input = sine(440.0, 44_100, 500);
        let out = to_target_rate(&input, 44_100);
        // The passband tone keeps most of its amplitude.
        assert!(tail_peak(&out) > 0.4, "peak was {}", tail_peak(&out));
    }

    #[test]
    fn an_out_of_band_tone_is_attenuated_instead_of_aliasing() {
        // 15 kHz at 44.1 kHz would fold to ~1 kHz at 16 kHz without the filter.
        let input = sine(15_000.0, 44_100, 500);
        let out = to_target_rate(&input, 44_100);
        assert!(
            tail_peak(&out) < 0.05,
            "alias energy was {}",
            tail_peak(&out)
        );
    }

    #[test]
    fn empty_input_stays_empty() {
        assert!(to_target_rate(&[], 44_100).is_empty());
    }

    #[test]
    #[should_panic(expected = "sample rate must be non-zero")]
    fn a_zero_rate_is_rejected() {
        to_target_rate(&[0.0], 0);
    }

    #[test]
    #[should_panic(expected = "above the")]
    fn an_implausible_rate_is_rejected() {
        to_target_rate(&[0.0], MAX_INPUT_RATE + 1);
    }
}
