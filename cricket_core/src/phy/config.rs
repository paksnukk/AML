//! Robust-profile physical-layer parameters (see docs/aml-v1-spec.md).

use super::bmc::{BmcParams, Modulation};
use alloc::vec;
use alloc::vec::Vec;

// Tone-purity thresholds (see `goertzel::tone_purity`). Measured on
// synthesized symbols: ~0.97 aligned, ~0.91 aligned at SNR 12 dB, >= 0.43 at a
// full search hop of misalignment (>= 0.39 at SNR 12 dB). White-noise windows
// peak at ~0.09 (robust, N = 288) and ~0.18 (fast, N = 144), so fast needs the
// higher threshold to keep margin above noise.
const PURITY_ROBUST: f32 = 0.25;
const PURITY_FAST: f32 = 0.3;

#[derive(Debug, Clone)]
pub struct PhyConfig {
    pub sample_rate_hz: u32,
    /// Line code. `Mfsk` for AML v1 (robust, fast); `Bmc` is experimental.
    pub modulation: Modulation,
    pub symbol_duration_ms: f32,
    pub guard_gap_ms: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub amplitude: f32,
    pub tone_base_hz: f32,
    pub tone_step_hz: f32,
    pub n_tones: usize,
    pub preamble: Vec<u8>,
    pub min_confidence_db: f32,
    /// Minimum level-independent tone purity (Goertzel power normalized by
    /// window energy, see `goertzel::tone_purity`; 1.0 = pure steady tone).
    pub min_tone_purity: f32,
    /// Silence floor: windows whose RMS is below this (dBFS, full-scale sine
    /// = -3 dBFS) are never detected as symbols.
    pub min_rms_dbfs: f32,
    pub repetition_k: usize,
    pub interleave_depth: usize,
    pub link_dead_ms: u32,
    pub heartbeat_interval_ms: u32,
}

impl Default for PhyConfig {
    fn default() -> Self {
        Self::robust()
    }
}

impl PhyConfig {
    pub fn robust() -> Self {
        Self {
            sample_rate_hz: 48_000,
            modulation: Modulation::Mfsk,
            symbol_duration_ms: 6.0,
            guard_gap_ms: 0.5,
            attack_ms: 0.3,
            release_ms: 0.3,
            amplitude: 0.8,
            tone_base_hz: 2500.0,
            tone_step_hz: 100.0,
            n_tones: 16,
            preamble: vec![0xA, 0x5, 0xC, 0x3],
            min_confidence_db: 3.0,
            min_tone_purity: PURITY_ROBUST,
            min_rms_dbfs: -60.0,
            repetition_k: 2,
            interleave_depth: 4,
            link_dead_ms: 2000,
            heartbeat_interval_ms: 500,
        }
    }

    /// Fast profile for clean dedicated chains (LTC-like). Targets ≤60 ms first-copy.
    /// 3 ms symbols, wider tone spacing (Goertzel resolution), short preamble, K=1.
    pub fn fast() -> Self {
        Self {
            sample_rate_hz: 48_000,
            modulation: Modulation::Mfsk,
            symbol_duration_ms: 3.0,
            guard_gap_ms: 0.0,
            attack_ms: 0.15,
            release_ms: 0.15,
            amplitude: 0.8,
            tone_base_hz: 2500.0,
            tone_step_hz: 200.0,
            n_tones: 16,
            preamble: vec![0xA, 0x5],
            min_confidence_db: 2.0,
            min_tone_purity: PURITY_FAST,
            min_rms_dbfs: -60.0,
            repetition_k: 1,
            interleave_depth: 4,
            link_dead_ms: 2000,
            heartbeat_interval_ms: 500,
        }
    }

    /// **Experimental** LTC-style profile: biphase-mark code at 4800 bit/s
    /// (see `docs/proposals/ltc-bmc.md`). Not part of the AML v1 spec.
    pub fn ltc() -> Self {
        Self::ltc_with_bit_rate(4800)
    }

    /// Experimental BMC profile at `bit_rate` bit/s. The symbol fields describe
    /// one nibble (4 bits) so generic helpers stay meaningful; the MFSK-only
    /// fields (tones, preamble, purity) are unused.
    pub fn ltc_with_bit_rate(bit_rate: u32) -> Self {
        Self {
            sample_rate_hz: 48_000,
            modulation: Modulation::Bmc(BmcParams::new(bit_rate)),
            symbol_duration_ms: 4000.0 / bit_rate as f32,
            guard_gap_ms: 0.0,
            attack_ms: 0.0,
            release_ms: 0.0,
            amplitude: 0.8,
            tone_base_hz: 0.0,
            tone_step_hz: 0.0,
            n_tones: 0,
            preamble: Vec::new(),
            min_confidence_db: 0.0,
            min_tone_purity: 0.0,
            min_rms_dbfs: -60.0,
            repetition_k: 1,
            interleave_depth: 4,
            link_dead_ms: 2000,
            heartbeat_interval_ms: 500,
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "fast" => Self::fast(),
            "ltc" => Self::ltc(),
            "ltc2400" => Self::ltc_with_bit_rate(2400),
            "ltc4800" => Self::ltc_with_bit_rate(4800),
            "ltc9600" => Self::ltc_with_bit_rate(9600),
            _ => Self::robust(),
        }
    }

    pub fn with_sample_rate(mut self, sample_rate_hz: u32) -> Self {
        self.sample_rate_hz = sample_rate_hz;
        self
    }

    /// Estimated first-copy on-wire duration for a max-size body (16 nibbles) + preamble.
    pub fn estimated_first_copy_ms(&self) -> f32 {
        let body_nibbles = 16usize; // MAX_BODY_NIBBLES
        if let Modulation::Bmc(p) = &self.modulation {
            return p.frame_ms(body_nibbles);
        }
        let symbols = self.preamble.len() + body_nibbles;
        let period_ms = self.symbol_duration_ms + self.guard_gap_ms;
        symbols as f32 * period_ms
    }

    pub fn symbol_samples(&self) -> usize {
        libm::roundf((self.symbol_duration_ms / 1000.0) * self.sample_rate_hz as f32) as usize
    }

    pub fn gap_samples(&self) -> usize {
        libm::roundf((self.guard_gap_ms / 1000.0) * self.sample_rate_hz as f32) as usize
    }

    pub fn samples_per_symbol_period(&self) -> usize {
        self.symbol_samples() + self.gap_samples()
    }

    pub fn hop_samples(&self) -> usize {
        (self.symbol_samples() / 4).max(1)
    }

    pub fn tone_hz(&self, index: u8) -> f32 {
        self.tone_base_hz + (index as f32) * self.tone_step_hz
    }

    pub fn tone_table(&self) -> Vec<f32> {
        (0..self.n_tones as u8).map(|i| self.tone_hz(i)).collect()
    }
}
