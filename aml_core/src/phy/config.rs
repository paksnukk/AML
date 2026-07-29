//! Robust-profile physical-layer parameters (see docs/aml-v2-spec.md).

#[derive(Debug, Clone)]
pub struct PhyConfig {
    pub sample_rate_hz: u32,
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
    pub absolute_power_min: f32,
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
            absolute_power_min: 50.0,
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
            absolute_power_min: 20.0,
            repetition_k: 1,
            interleave_depth: 4,
            link_dead_ms: 2000,
            heartbeat_interval_ms: 500,
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "fast" => Self::fast(),
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
        let symbols = self.preamble.len() + body_nibbles;
        let period_ms = self.symbol_duration_ms + self.guard_gap_ms;
        symbols as f32 * period_ms
    }

    pub fn symbol_samples(&self) -> usize {
        ((self.symbol_duration_ms / 1000.0) * self.sample_rate_hz as f32).round() as usize
    }

    pub fn gap_samples(&self) -> usize {
        ((self.guard_gap_ms / 1000.0) * self.sample_rate_hz as f32).round() as usize
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
