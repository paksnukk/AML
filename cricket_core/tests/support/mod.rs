//! Shared integration-test helpers: a deterministic audio channel simulator
//! and a chunked streaming decoder.
//!
//! Use from a test file with `mod support;`.

#![allow(dead_code)]

use cricket_core::modem::{CricketModemRx, ModemOutput};
use cricket_core::phy::{ModemTx, PhyConfig};
use cricket_core::transport::TransportMessage;
use std::f32::consts::PI;

/// Small deterministic xorshift64* RNG (no external crates).
#[derive(Debug, Clone)]
pub struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        // State must never be zero; mix the seed so nearby seeds diverge quickly.
        let mixed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
        Self {
            state: if mixed == 0 { 1 } else { mixed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in [0, n). `n` must be > 0.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Standard normal sample (Box-Muller).
    pub fn gaussian(&mut self) -> f64 {
        let u1 = self.next_f64().max(f64::MIN_POSITIVE);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Deterministic channel impairments applied to mono samples.
///
/// Processing order: symbol corruption (on the clean, grid-aligned TX
/// signal) → clock drift → low-pass(es) → high-pass → peaking EQ → soft clip
/// → polarity → gain → hum → leading silence → AWGN.
#[derive(Debug, Clone)]
pub struct ChannelSim {
    pub config: PhyConfig,
    pub seed: u64,
    /// Leading silence inserted before the signal, in samples.
    pub offset_samples: usize,
    /// Linear gain.
    pub gain: f32,
    /// AWGN SNR in dB relative to the tone RMS (`amplitude * gain / sqrt(2)`).
    pub snr_db: Option<f32>,
    /// Receiver clock drift in ppm (positive = RX reads the signal faster).
    pub drift_ppm: f64,
    /// One-pole low-pass cutoff in Hz.
    pub lowpass_hz: Option<f32>,
    /// Probability per symbol period of replacing a tone with a different one.
    pub symbol_error_prob: f64,
    /// Second-order Butterworth low-pass cutoff in Hz (steeper band limit).
    pub lowpass2_hz: Option<f32>,
    /// One-pole high-pass cutoff in Hz (AC coupling / transformer).
    pub highpass_hz: Option<f32>,
    /// Peaking EQ: (centre Hz, gain dB, Q).
    pub peaking_eq: Option<(f32, f32, f32)>,
    /// Soft clipper `tanh(drive * x) / drive` (drive > 0; ~limiter).
    pub soft_clip_drive: Option<f32>,
    /// Invert polarity (pin 2/3 swap).
    pub invert: bool,
    /// Mains hum: (frequency Hz, level in dB relative to the tone RMS).
    pub hum: Option<(f32, f32)>,
}

impl ChannelSim {
    /// Identity channel for `config`.
    pub fn new(config: PhyConfig, seed: u64) -> Self {
        Self {
            config,
            seed,
            offset_samples: 0,
            gain: 1.0,
            snr_db: None,
            drift_ppm: 0.0,
            lowpass_hz: None,
            symbol_error_prob: 0.0,
            lowpass2_hz: None,
            highpass_hz: None,
            peaking_eq: None,
            soft_clip_drive: None,
            invert: false,
            hum: None,
        }
    }

    pub fn lowpass2_hz(mut self, cutoff_hz: f32) -> Self {
        self.lowpass2_hz = Some(cutoff_hz);
        self
    }

    pub fn highpass_hz(mut self, cutoff_hz: f32) -> Self {
        self.highpass_hz = Some(cutoff_hz);
        self
    }

    pub fn peaking_eq(mut self, centre_hz: f32, gain_db: f32, q: f32) -> Self {
        self.peaking_eq = Some((centre_hz, gain_db, q));
        self
    }

    pub fn soft_clip(mut self, drive: f32) -> Self {
        self.soft_clip_drive = Some(drive);
        self
    }

    pub fn invert(mut self) -> Self {
        self.invert = true;
        self
    }

    pub fn hum(mut self, freq_hz: f32, rel_db: f32) -> Self {
        self.hum = Some((freq_hz, rel_db));
        self
    }

    pub fn offset(mut self, samples: usize) -> Self {
        self.offset_samples = samples;
        self
    }

    pub fn gain(mut self, gain: f32) -> Self {
        self.gain = gain;
        self
    }

    pub fn snr_db(mut self, snr_db: f32) -> Self {
        self.snr_db = Some(snr_db);
        self
    }

    pub fn drift_ppm(mut self, ppm: f64) -> Self {
        self.drift_ppm = ppm;
        self
    }

    pub fn lowpass_hz(mut self, cutoff_hz: f32) -> Self {
        self.lowpass_hz = Some(cutoff_hz);
        self
    }

    pub fn symbol_error_prob(mut self, p: f64) -> Self {
        self.symbol_error_prob = p;
        self
    }

    /// Apply all configured impairments. Deterministic for a given seed.
    pub fn apply(&self, input: &[f32]) -> Vec<f32> {
        let mut rng = XorShift64::new(self.seed);
        let mut samples = input.to_vec();

        if self.symbol_error_prob > 0.0 {
            corrupt_symbols(&self.config, &mut samples, self.symbol_error_prob, &mut rng);
        }
        if self.drift_ppm != 0.0 {
            samples = resample_drift(&samples, self.drift_ppm);
        }
        let rate = self.config.sample_rate_hz;
        if let Some(cutoff) = self.lowpass_hz {
            one_pole_lowpass(&mut samples, cutoff, rate);
        }
        if let Some(cutoff) = self.lowpass2_hz {
            Biquad::lowpass(cutoff, rate).run(&mut samples);
        }
        if let Some(cutoff) = self.highpass_hz {
            one_pole_highpass(&mut samples, cutoff, rate);
        }
        if let Some((f, db, q)) = self.peaking_eq {
            Biquad::peaking(f, db, q, rate).run(&mut samples);
        }
        if let Some(drive) = self.soft_clip_drive {
            for s in samples.iter_mut() {
                *s = (drive * *s).tanh() / drive;
            }
        }
        let gain = if self.invert { -self.gain } else { self.gain };
        if gain != 1.0 {
            for s in samples.iter_mut() {
                *s *= gain;
            }
        }
        if let Some((f, rel_db)) = self.hum {
            let tone_rms = self.config.amplitude * self.gain / 2f32.sqrt();
            let amp = tone_rms * 2f32.sqrt() * 10f32.powf(rel_db / 20.0);
            let w = 2.0 * PI * f / rate as f32;
            for (i, s) in samples.iter_mut().enumerate() {
                *s += amp * (w * i as f32).sin();
            }
        }
        if self.offset_samples > 0 {
            let mut shifted = vec![0.0; self.offset_samples];
            shifted.extend_from_slice(&samples);
            samples = shifted;
        }
        if let Some(snr_db) = self.snr_db {
            let tone_rms = self.config.amplitude * self.gain / 2f32.sqrt();
            let sigma = tone_rms as f64 / 10f64.powf(snr_db as f64 / 20.0);
            for s in samples.iter_mut() {
                *s += (rng.gaussian() * sigma) as f32;
            }
        }
        samples
    }
}

/// Replace whole symbol periods (assumed aligned to index 0, as produced by
/// `encode_to_samples`) with a different MFSK tone, with probability `p` per
/// period. Silent periods are left untouched.
pub fn corrupt_symbols(config: &PhyConfig, samples: &mut [f32], p: f64, rng: &mut XorShift64) {
    let period = config.samples_per_symbol_period();
    let sym = config.symbol_samples();
    let tones = config.tone_table();
    let tx = ModemTx::new(config.clone());
    let silence_power = 1e-6 * sym as f32;

    for start in (0..samples.len()).step_by(period) {
        let end = (start + period).min(samples.len());
        if end - start < sym || rng.next_f64() >= p {
            continue;
        }
        let window = &samples[start..start + sym];
        let (current, power) = strongest_tone(window, &tones, config.sample_rate_hz);
        if power < silence_power {
            continue;
        }
        let replacement = (current as usize + 1 + rng.below(tones.len() - 1)) % tones.len();
        let audio = tx.synthesize_nibbles(&[replacement as u8]);
        samples[start..end].copy_from_slice(&audio[..end - start]);
    }
}

/// Linear-interpolation resampling simulating a receiver clock offset of `ppm`.
pub fn resample_drift(input: &[f32], ppm: f64) -> Vec<f32> {
    if input.len() < 2 {
        return input.to_vec();
    }
    let ratio = 1.0 + ppm * 1e-6;
    let last = (input.len() - 1) as f64;
    let mut out = Vec::with_capacity((input.len() as f64 / ratio) as usize + 1);
    let mut i = 0usize;
    loop {
        let pos = i as f64 * ratio;
        if pos > last {
            break;
        }
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input[idx];
        let b = if idx + 1 < input.len() {
            input[idx + 1]
        } else {
            a
        };
        out.push(a + (b - a) * frac);
        i += 1;
    }
    out
}

/// In-place one-pole IIR low-pass with cutoff `cutoff_hz`.
pub fn one_pole_lowpass(samples: &mut [f32], cutoff_hz: f32, sample_rate_hz: u32) {
    let alpha = 1.0 - (-2.0 * PI * cutoff_hz / sample_rate_hz as f32).exp();
    let mut y = 0.0f32;
    for s in samples.iter_mut() {
        y += alpha * (*s - y);
        *s = y;
    }
}

/// In-place one-pole high-pass (DC blocker) with cutoff `cutoff_hz`.
pub fn one_pole_highpass(samples: &mut [f32], cutoff_hz: f32, sample_rate_hz: u32) {
    let r = (-2.0 * PI * cutoff_hz / sample_rate_hz as f32).exp();
    let (mut x1, mut y1) = (0.0f32, 0.0f32);
    for s in samples.iter_mut() {
        let y = *s - x1 + r * y1;
        x1 = *s;
        y1 = y;
        *s = y;
    }
}

/// RBJ-cookbook biquad (direct form I).
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
}

impl Biquad {
    /// Butterworth low-pass.
    pub fn lowpass(cutoff_hz: f32, sample_rate_hz: u32) -> Self {
        let w0 = 2.0 * PI * cutoff_hz / sample_rate_hz as f32;
        let alpha = w0.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let c = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b: [(1.0 - c) / 2.0 / a0, (1.0 - c) / a0, (1.0 - c) / 2.0 / a0],
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
        }
    }

    /// Peaking EQ of `gain_db` at `centre_hz` with quality `q`.
    pub fn peaking(centre_hz: f32, gain_db: f32, q: f32, sample_rate_hz: u32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * centre_hz / sample_rate_hz as f32;
        let alpha = w0.sin() / (2.0 * q);
        let c = w0.cos();
        let a0 = 1.0 + alpha / a;
        Self {
            b: [
                (1.0 + alpha * a) / a0,
                -2.0 * c / a0,
                (1.0 - alpha * a) / a0,
            ],
            a: [-2.0 * c / a0, (1.0 - alpha / a) / a0],
        }
    }

    pub fn run(&self, samples: &mut [f32]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for s in samples.iter_mut() {
            let x = *s;
            let y =
                self.b[0] * x + self.b[1] * x1 + self.b[2] * x2 - self.a[0] * y1 - self.a[1] * y2;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
            *s = y;
        }
    }
}

fn strongest_tone(window: &[f32], tones: &[f32], sample_rate_hz: u32) -> (u8, f32) {
    let mut best = (0u8, 0.0f32);
    for (i, &freq) in tones.iter().enumerate() {
        let w = 2.0 * PI * freq / sample_rate_hz as f32;
        let coeff = 2.0 * w.cos();
        let (mut q1, mut q2) = (0.0f32, 0.0f32);
        for &x in window {
            let q0 = coeff * q1 - q2 + x;
            q2 = q1;
            q1 = q0;
        }
        let power = q1 * q1 + q2 * q2 - q1 * q2 * coeff;
        if power > best.1 {
            best = (i as u8, power);
        }
    }
    best
}

/// Trailing silence fed by [`decode_chunked`], in milliseconds. Longer than a
/// full Robust transmission (2 x 20 symbols of 6.5 ms ~= 150 ms), so any packet
/// whose start is in the input can finish even if the input is truncated.
pub const FLUSH_MS: usize = 250;

/// Feed `samples` followed by [`FLUSH_MS`] of silence into a fresh
/// `CricketModemRx` in chunks of exactly `chunk_size` samples (only the final
/// silence chunk may be shorter) and collect decoded messages.
pub fn decode_chunked(
    config: PhyConfig,
    samples: &[f32],
    chunk_size: usize,
) -> Vec<TransportMessage> {
    assert!(chunk_size > 0, "chunk_size must be > 0");
    let flush = config.sample_rate_hz as usize * FLUSH_MS / 1000;
    let mut stream = Vec::with_capacity(samples.len() + flush);
    stream.extend_from_slice(samples);
    stream.resize(samples.len() + flush, 0.0);

    let mut rx = CricketModemRx::new(config);
    let mut messages = Vec::new();
    for chunk in stream.chunks(chunk_size) {
        for out in rx.push_samples(chunk) {
            if let ModemOutput::Message(m) = out {
                messages.push(m);
            }
        }
    }
    messages
}
