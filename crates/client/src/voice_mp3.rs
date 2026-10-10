//! Voice messages as MP3: the microphone's samples, resampled to 24 kHz mono and encoded at
//! 48 kbps with `rusty_mp3` (pure Rust). MP3 is the one format every browser and every
//! phone's own player opens, whatever browser recorded it. Pure Rust, no browser APIs.

use rusty_mp3::{Error, Mp3Encoder, Mp3EncoderConfig};

/// The rate voice messages are encoded at: clear speech, one of MP3's own rates.
pub const VOICE_RATE: u32 = 24_000;
/// About 350 KB a minute.
pub const VOICE_KBPS: u32 = 48;
pub const VOICE_MIME: &str = "audio/mpeg";

/// Kernel half-width, in zero crossings of the low-pass sinc.
const ZERO_CROSSINGS: f64 = 16.0;
/// The low-pass sits this far below the lower Nyquist frequency.
const ROLL_OFF: f64 = 0.9;

/// Streaming sample-rate converter: windowed-sinc interpolation with a low-pass at the lower
/// of the two Nyquist frequencies (so downsampling doesn't alias).
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Low-pass cutoff in cycles per input sample.
    cutoff: f64,
    /// Kernel half-width in input samples.
    half: f64,
    /// Input still needed, starting at absolute input index `base`.
    buf: Vec<f32>,
    base: usize,
    /// Absolute input position of the next output sample.
    pos: f64,
    /// Real (not padding) input samples taken so far.
    taken: usize,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let step = f64::from(in_rate.max(1)) / f64::from(out_rate.max(1));
        let cutoff = 0.5 * (1.0 / step).min(1.0) * ROLL_OFF;
        let half = (ZERO_CROSSINGS / (2.0 * cutoff)).ceil();
        // Zeros before the first sample: the kernel can center on it straight away.
        let lead = half as usize;
        Self { step, cutoff, half, buf: vec![0.0; lead], base: 0, pos: lead as f64, taken: 0 }
    }

    /// Resample `input`, appending what can be computed so far to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.taken += input.len();
        self.buf.extend(input.iter().map(|s| if s.is_finite() { *s } else { 0.0 }));
        self.produce(self.base + self.buf.len(), out);
    }

    /// End of input: the samples up to the last real one.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let end = self.half as usize + self.taken;
        self.buf.extend(std::iter::repeat(0.0).take(self.half as usize + 1));
        // Output only up to the end of real input (no ringing tail of padding).
        let available = self.base + self.buf.len();
        while self.pos < end as f64 && self.pos + self.half < available as f64 {
            out.push(self.sample_at(self.pos));
            self.pos += self.step;
        }
    }

    fn produce(&mut self, available: usize, out: &mut Vec<f32>) {
        while self.pos + self.half < available as f64 {
            out.push(self.sample_at(self.pos));
            self.pos += self.step;
        }
        // Drop input the kernel can no longer reach.
        let keep_from = (self.pos - self.half).floor().max(0.0) as usize;
        if keep_from > self.base {
            let drop = (keep_from - self.base).min(self.buf.len());
            self.buf.drain(..drop);
            self.base += drop;
        }
    }

    fn sample_at(&self, t: f64) -> f32 {
        let first = (t - self.half).ceil().max(self.base as f64) as usize;
        let last = (t + self.half).floor() as usize;
        let (mut acc, mut weight) = (0.0_f64, 0.0_f64);
        for k in first..=last {
            let Some(x) = self.buf.get(k - self.base) else {
                break;
            };
            let d = t - k as f64;
            let h = self.kernel(d);
            acc += f64::from(*x) * h;
            weight += h;
        }
        if weight.abs() < 1e-9 {
            0.0
        } else {
            (acc / weight) as f32
        }
    }

    /// Blackman-windowed sinc low-pass, `d` input samples from the center.
    fn kernel(&self, d: f64) -> f64 {
        if d.abs() >= self.half {
            return 0.0;
        }
        let x = 2.0 * self.cutoff * d;
        let sinc = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
        let w = 2.0 * std::f64::consts::PI * (d / (2.0 * self.half) + 0.5);
        let window = 0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos();
        sinc * window
    }
}

/// Microphone samples in, a voice message's MP3 bytes out. Encodes as samples arrive, so
/// `finish` only flushes the last frame and the header.
pub struct VoiceEncoder {
    resampler: Resampler,
    mp3: Mp3Encoder,
    in_rate: u32,
    /// Microphone samples pushed (for the length).
    samples: u64,
    scratch: Vec<f32>,
    failed: bool,
}

impl VoiceEncoder {
    /// `in_rate`: the microphone's (the audio context's) sample rate.
    pub fn new(in_rate: u32) -> Self {
        Self {
            resampler: Resampler::new(in_rate, VOICE_RATE),
            mp3: Mp3Encoder::new(Mp3EncoderConfig { bitrate_kbps: VOICE_KBPS, vbr_quality: None }),
            in_rate: in_rate.max(1),
            samples: 0,
            scratch: Vec::new(),
            failed: false,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.samples += samples.len() as u64;
        self.scratch.clear();
        self.resampler.push(samples, &mut self.scratch);
        self.encode_scratch();
    }

    /// Length so far, in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.samples * 1000 / u64::from(self.in_rate)
    }

    /// The whole MP3 (its Info header first, so players know the length), or `None` if
    /// encoding failed or nothing was recorded.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        self.scratch.clear();
        self.resampler.finish(&mut self.scratch);
        self.encode_scratch();
        if self.failed || self.samples == 0 {
            return None;
        }
        self.mp3.finish();
        let mut out = Vec::new();
        loop {
            match self.mp3.next_packet() {
                Ok(packet) => out.extend(packet),
                Err(Error::Eof) => break,
                Err(_) => return None,
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn encode_scratch(&mut self) {
        if self.scratch.is_empty() || self.failed {
            return;
        }
        // Packets stay queued until `finish`: the Info header it adds goes in front of them.
        if self.mp3.push_pcm_f32(&self.scratch, 1, VOICE_RATE).is_err() {
            self.failed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f64, seconds: f64, amplitude: f32) -> Vec<f32> {
        (0..(f64::from(rate) * seconds) as usize)
            .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin() as f32 * amplitude)
            .collect()
    }

    /// Resample in uneven chunks, as audio arrives.
    fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        let mut r = Resampler::new(from, to);
        let mut out = Vec::new();
        for chunk in input.chunks(1000) {
            r.push(chunk, &mut out);
        }
        r.finish(&mut out);
        out
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn test_resampler_keeps_speech_and_length() {
        for (from, to) in [(48_000, 24_000), (44_100, 24_000), (16_000, 24_000), (24_000, 24_000)] {
            let input = tone(from, 1000.0, 1.0, 0.5);
            let out = resample(&input, from, to);
            let expected = (input.len() as f64 * f64::from(to) / f64::from(from)).round() as i64;
            assert!((out.len() as i64 - expected).abs() <= 1, "{from}→{to}: {} vs {expected}", out.len());
            // Away from the edges, a 1 kHz tone keeps its level.
            let middle = &out[out.len() / 4..out.len() * 3 / 4];
            assert!((peak(middle) - 0.5).abs() < 0.01, "{from}→{to}: peak {}", peak(middle));
        }
    }

    #[test]
    fn test_resampler_removes_what_the_lower_rate_cannot_carry() {
        // 15 kHz is above 24 kHz's Nyquist (12 kHz): it must not fold back into speech.
        let out = resample(&tone(48_000, 15_000.0, 1.0, 0.5), 48_000, 24_000);
        let middle = &out[out.len() / 4..out.len() * 3 / 4];
        assert!(peak(middle) < 0.01, "aliased: {}", peak(middle));
    }

    #[test]
    fn test_resampler_survives_odd_input() {
        let mut r = Resampler::new(48_000, 24_000);
        let mut out = Vec::new();
        r.push(&[], &mut out);
        r.push(&[f32::NAN, f32::INFINITY, 0.5], &mut out);
        r.finish(&mut out);
        assert!(out.iter().all(|s| s.is_finite()));
        let mut empty = Resampler::new(44_100, 24_000);
        let mut none = Vec::new();
        empty.finish(&mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn test_voice_encoder_makes_a_decodable_mp3_of_the_right_length() {
        let mut enc = VoiceEncoder::new(48_000);
        let speech: Vec<f32> = tone(48_000, 220.0, 2.0, 0.4)
            .iter()
            .zip(tone(48_000, 660.0, 2.0, 0.2))
            .map(|(a, b)| a + b)
            .collect();
        for chunk in speech.chunks(4096) {
            enc.push(chunk);
        }
        assert_eq!(enc.duration_ms(), 2000);
        let mp3 = enc.finish().expect("mp3");
        // About 48 kbps: 2 s ≈ 12 KB (plus the header and the last frame).
        assert!((10_000..16_000).contains(&mp3.len()), "{} bytes", mp3.len());
        // Starts with an MPEG audio frame sync.
        assert_eq!(mp3[0], 0xFF);
        assert_eq!(mp3[1] & 0xE0, 0xE0);
        let decoded = rusty_mp3::decode_pipelined(&mp3);
        let rate = decoded.first().map(|d| d.sample_rate).unwrap();
        assert_eq!(rate, VOICE_RATE);
        let pcm: Vec<f32> = decoded.into_iter().flat_map(|d| d.samples).collect();
        let seconds = pcm.len() as f64 / f64::from(rate);
        assert!((seconds - 2.0).abs() < 0.15, "decoded {seconds} s");
        assert!(peak(&pcm) > 0.3, "silent: {}", peak(&pcm));
    }

    #[test]
    fn test_nothing_recorded_is_no_file() {
        assert!(VoiceEncoder::new(48_000).finish().is_none());
    }
}
