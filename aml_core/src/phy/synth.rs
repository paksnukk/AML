use super::config::PhyConfig;
use std::f32::consts::PI;

/// Encodes MFSK-16 nibble streams into mono audio samples.
pub struct ModemTx {
    config: PhyConfig,
}

impl ModemTx {
    pub fn new(config: PhyConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &PhyConfig {
        &self.config
    }

    /// Synthesize audio for a sequence of tone indices (0..15), each followed by guard gap.
    pub fn synthesize_nibbles(&self, nibbles: &[u8]) -> Vec<f32> {
        let mut out = Vec::new();
        for &n in nibbles {
            let idx = n & 0x0F;
            out.extend(self.synthesize_symbol(self.config.tone_hz(idx)));
            let gap = self.config.gap_samples();
            if gap > 0 {
                out.extend(std::iter::repeat_n(0.0, gap));
            }
        }
        out
    }

    /// Full on-wire burst: preamble + body nibbles.
    pub fn synthesize_framed(&self, body_nibbles: &[u8]) -> Vec<f32> {
        let mut all = Vec::with_capacity(4 + body_nibbles.len());
        all.extend_from_slice(&self.config.preamble);
        all.extend_from_slice(body_nibbles);
        self.synthesize_nibbles(&all)
    }

    fn synthesize_symbol(&self, freq: f32) -> Vec<f32> {
        let total = self.config.symbol_samples();
        let attack =
            ((self.config.attack_ms / 1000.0) * self.config.sample_rate_hz as f32).round() as usize;
        let release = ((self.config.release_ms / 1000.0) * self.config.sample_rate_hz as f32)
            .round() as usize;
        let amp = self.config.amplitude;

        let mut buffer = Vec::with_capacity(total);
        let phase_inc = 2.0 * PI * freq / self.config.sample_rate_hz as f32;
        let mut phase = 0.0f32;

        for i in 0..total {
            let envelope = if attack > 0 && i < attack {
                i as f32 / attack as f32
            } else if release > 0 && i >= total.saturating_sub(release) {
                (total - i) as f32 / release as f32
            } else {
                1.0
            };
            buffer.push(phase.sin() * envelope * amp);
            phase += phase_inc;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
        }
        buffer
    }
}
