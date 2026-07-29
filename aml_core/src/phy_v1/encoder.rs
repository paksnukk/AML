use crate::phy_v1::profile::ProfileDefinition;
use crate::phy_v1::protocol::SymbolValue;
use std::f32::consts::PI;

pub struct Encoder {
    profile: ProfileDefinition,
    sample_rate: u32,
}

impl Encoder {
    pub fn new(profile: ProfileDefinition) -> Self {
        let sample_rate = profile.audio.sample_rate_hz;
        Self {
            profile,
            sample_rate,
        }
    }

    /// Generates the audio samples for a sequence of symbols (a frame).
    /// Returns a vector of f32 samples (mono).
    pub fn encode_frame(&self, symbols: &[SymbolValue]) -> Vec<f32> {
        let mut buffer = Vec::new();

        for symbol in symbols {
            let freq = self.symbol_to_frequency(symbol);
            let samples = self.synthesize_symbol(freq);
            buffer.extend(samples);

            // Add guard gap if defined
            if self.profile.audio.guard_gap_ms > 0.0 {
                let gap_samples = self.ms_to_samples(self.profile.audio.guard_gap_ms);
                buffer.extend(vec![0.0; gap_samples]);
            }
        }

        buffer
    }

    fn symbol_to_frequency(&self, symbol: &SymbolValue) -> f32 {
        let bands = &self.profile.bands;
        match symbol {
            SymbolValue::Preamble => bands.preamble_hz,
            SymbolValue::Type(t) => {
                bands.type_band.base_hz + (t.to_u8() as f32 * bands.type_band.step_hz)
            }
            SymbolValue::Note(n) => bands.note_band.base_hz + (*n as f32 * bands.note_band.step_hz),
            SymbolValue::Octave(o) => {
                bands.octave_band.base_hz + (*o as f32 * bands.octave_band.step_hz)
            }
            SymbolValue::Value(v) => {
                bands.value_band.base_hz + (*v as f32 * bands.value_band.step_hz)
            }
            SymbolValue::MscCmd(c) => {
                bands.msc_cmd_band.base_hz + (c.to_u8() as f32 * bands.msc_cmd_band.step_hz)
            }
            SymbolValue::CueList(c) => {
                bands.cuelist_band.base_hz + (*c as f32 * bands.cuelist_band.step_hz)
            }
        }
    }

    fn synthesize_symbol(&self, freq: f32) -> Vec<f32> {
        let duration_ms = self.profile.audio.symbol_duration_ms;
        let total_samples = self.ms_to_samples(duration_ms);
        let attack_samples = self.ms_to_samples(self.profile.audio.ramp_ms.attack);
        let release_samples = self.ms_to_samples(self.profile.audio.ramp_ms.release);

        let mut buffer = Vec::with_capacity(total_samples);
        let phase_inc = 2.0 * PI * freq / self.sample_rate as f32;
        let mut phase: f32 = 0.0;

        for i in 0..total_samples {
            // Calculate envelope
            let envelope = if i < attack_samples {
                if attack_samples > 0 {
                    i as f32 / attack_samples as f32
                } else {
                    1.0
                }
            } else if i >= total_samples - release_samples {
                if release_samples > 0 {
                    (total_samples - i) as f32 / release_samples as f32
                } else {
                    1.0
                }
            } else {
                1.0
            };

            buffer.push(phase.sin() * envelope);
            phase += phase_inc;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
        }

        buffer
    }

    fn ms_to_samples(&self, ms: f32) -> usize {
        ((ms / 1000.0) * self.sample_rate as f32).round() as usize
    }
}
