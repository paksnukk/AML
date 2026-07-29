use crate::phy_v1::profile::ProfileDefinition;
use crate::phy_v1::protocol::{FrameType, MscCommand, SymbolValue};
use std::f32::consts::PI;

#[derive(Debug, Clone, PartialEq)]
pub enum DecoderState {
    WaitPreamble,
    ReadType,
    ReadPayload {
        frame_type: FrameType,
        collected: Vec<SymbolValue>,
    },
}

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub frame_type: FrameType,
    pub payload: Vec<SymbolValue>,
}

pub struct Decoder {
    profile: ProfileDefinition,
    sample_rate: u32,
    pub state: DecoderState,
}

impl Decoder {
    pub fn new(profile: ProfileDefinition) -> Self {
        let sample_rate = profile.audio.sample_rate_hz;
        Self {
            profile,
            sample_rate,
            state: DecoderState::WaitPreamble,
        }
    }

    /// Reset the decoder to the initial state (WaitPreamble)
    pub fn reset_state(&mut self) {
        self.state = DecoderState::WaitPreamble;
    }

    /// Run the Goertzel algorithm on a slice of samples for a specific frequency.
    /// Returns the power (magnitude squared) of the signal at that frequency.
    pub fn goertzel_power(&self, samples: &[f32], target_freq: f32) -> f32 {
        let n = samples.len() as f32;
        // Standard Goertzel optimized for specific frequency detection
        let k = (n * target_freq) / self.sample_rate as f32;

        let w = 2.0 * PI * k / n;
        let cosine = w.cos();
        let coeff = 2.0 * cosine;

        let mut q1 = 0.0;
        let mut q2 = 0.0;

        for &sample in samples {
            let q0 = coeff * q1 - q2 + sample;
            q2 = q1;
            q1 = q0;
        }

        // Power = q1^2 + q2^2 - q1*q2*coeff
        q1 * q1 + q2 * q2 - q1 * q2 * coeff
    }

    /// Detect which symbol from a set of candidates has the highest power in the audio slice.
    /// Returns (SymbolValue, Power) if the best candidate exceeds the threshold.
    pub fn detect_symbol(
        &self,
        samples: &[f32],
        candidates: &[(SymbolValue, f32)],
    ) -> Option<(SymbolValue, f32)> {
        let mut best_symbol = None;
        let mut max_power = 0.0;
        let mut runner_up_power = 0.0;

        // Calculate power for all candidates
        for (symbol, freq) in candidates {
            let power = self.goertzel_power(samples, *freq);

            if power > max_power {
                runner_up_power = max_power;
                max_power = power;
                best_symbol = Some(*symbol);
            } else if power > runner_up_power {
                runner_up_power = power;
            }
        }

        // Check thresholds from profile

        // 1. Minimum absolute power check (simple noise floor gate)
        // In a real implementation, we might track average noise floor.
        // For now, use a safe constant or derived from profile margin if applicable.
        if max_power < 0.0001 {
            return None;
        }

        // 2. Confidence (Ratio over runner up)
        // The profile specifies "min_confidence_db_over_runnerup"
        // Power is squared amplitude, so dB = 10 * log10(P1/P2)
        // Ratio P1/P2 = 10^(dB/10)
        let min_confidence_db = self.profile.detection.min_confidence_db_over_runnerup;
        let min_ratio = 10.0f32.powf(min_confidence_db / 10.0);

        if runner_up_power > 0.0 {
            let ratio = max_power / runner_up_power;
            if ratio < min_ratio {
                return None; // Not confident enough; ambiguous signal
            }
        }

        best_symbol.map(|s| (s, max_power))
    }

    /// Returns the list of candidates (Symbol, Frequency) the decoder should listen for
    /// based on the current state.
    pub fn get_scan_candidates(&self) -> Vec<(SymbolValue, f32)> {
        match &self.state {
            DecoderState::WaitPreamble => {
                vec![(SymbolValue::Preamble, self.profile.bands.preamble_hz)]
            }
            DecoderState::ReadType => self.get_type_candidates(),
            DecoderState::ReadPayload {
                frame_type,
                collected,
            } => self.get_next_payload_candidates(*frame_type, collected.len()),
        }
    }

    /// Advance the state machine with a detected symbol.
    /// Returns Some(DecodedFrame) if a frame was successfully completed.
    pub fn handle_symbol(&mut self, symbol: SymbolValue) -> Option<DecodedFrame> {
        match &self.state {
            DecoderState::WaitPreamble => {
                if symbol == SymbolValue::Preamble {
                    self.state = DecoderState::ReadType;
                }
                None
            }
            DecoderState::ReadType => {
                if let SymbolValue::Type(ft) = symbol {
                    if self.get_payload_length(ft) == 0 {
                        // Zero-payload frame (e.g. Heartbeat), emit immediately
                        self.state = DecoderState::WaitPreamble;
                        Some(DecodedFrame {
                            frame_type: ft,
                            payload: Vec::new(),
                        })
                    } else {
                        // Move to reading payload
                        self.state = DecoderState::ReadPayload {
                            frame_type: ft,
                            collected: Vec::new(),
                        };
                        None
                    }
                } else {
                    // Unexpected symbol after Preamble (was expecting Type)
                    // Reset to wait for next preamble
                    self.reset_state();
                    None
                }
            }
            DecoderState::ReadPayload {
                frame_type,
                collected,
            } => {
                // Check if symbol matches expectation?
                // Implicitly assumed correct band since get_scan_candidates was used.
                // TODO: Add stricter type checking if necessary.

                let mut new_collected = collected.clone();
                new_collected.push(symbol);

                let expected_len = self.get_payload_length(*frame_type);

                if new_collected.len() >= expected_len {
                    // Frame Complete
                    let frame = DecodedFrame {
                        frame_type: *frame_type,
                        payload: new_collected,
                    };
                    self.state = DecoderState::WaitPreamble;
                    Some(frame)
                } else {
                    // Continue collecting
                    self.state = DecoderState::ReadPayload {
                        frame_type: *frame_type,
                        collected: new_collected,
                    };
                    None
                }
            }
        }
    }

    // --- Helpers for Candidates ---

    pub fn get_preamble_freq(&self) -> f32 {
        self.profile.bands.preamble_hz
    }

    fn get_type_candidates(&self) -> Vec<(SymbolValue, f32)> {
        let band = &self.profile.bands.type_band;
        let mut candidates = Vec::new();
        // Types 0 to 5
        for i in 0..=5 {
            let freq = band.base_hz + (i as f32 * band.step_hz);
            if let Some(ft) = FrameType::from_u8(i) {
                candidates.push((SymbolValue::Type(ft), freq));
            }
        }
        candidates
    }

    fn get_payload_length(&self, frame_type: FrameType) -> usize {
        match frame_type {
            FrameType::NoteOn => 3,        // Note, Octave, Value
            FrameType::NoteOff => 2,       // Note, Octave
            FrameType::ProgramChange => 1, // Value
            FrameType::MscLite => 3,       // Cmd, Cuelist, Value
            FrameType::Heartbeat => 0,
            FrameType::Panic => 0,
        }
    }

    fn get_next_payload_candidates(
        &self,
        frame_type: FrameType,
        current_index: usize,
    ) -> Vec<(SymbolValue, f32)> {
        match frame_type {
            FrameType::NoteOn => match current_index {
                0 => self.get_note_candidates(),
                1 => self.get_octave_candidates(),
                2 => self.get_value_candidates(),
                _ => vec![],
            },
            FrameType::NoteOff => match current_index {
                0 => self.get_note_candidates(),
                1 => self.get_octave_candidates(),
                _ => vec![],
            },
            FrameType::ProgramChange => match current_index {
                0 => self.get_value_candidates(),
                _ => vec![],
            },
            FrameType::MscLite => match current_index {
                0 => self.get_msc_cmd_candidates(),
                1 => self.get_cuelist_candidates(),
                2 => self.get_value_candidates(),
                _ => vec![],
            },
            _ => vec![],
        }
    }

    fn get_note_candidates(&self) -> Vec<(SymbolValue, f32)> {
        let band = &self.profile.bands.note_band;
        (0..12)
            .map(|i| {
                let freq = band.base_hz + (i as f32 * band.step_hz);
                (SymbolValue::Note(i), freq)
            })
            .collect()
    }

    fn get_octave_candidates(&self) -> Vec<(SymbolValue, f32)> {
        let band = &self.profile.bands.octave_band;
        (0..=10)
            .map(|i| {
                let freq = band.base_hz + (i as f32 * band.step_hz);
                (SymbolValue::Octave(i), freq)
            })
            .collect()
    }

    fn get_value_candidates(&self) -> Vec<(SymbolValue, f32)> {
        let band = &self.profile.bands.value_band;
        (0..=15)
            .map(|i| {
                let freq = band.base_hz + (i as f32 * band.step_hz);
                (SymbolValue::Value(i), freq)
            })
            .collect()
    }

    fn get_msc_cmd_candidates(&self) -> Vec<(SymbolValue, f32)> {
        let band = &self.profile.bands.msc_cmd_band;
        (0..=3)
            .filter_map(|i| {
                let freq = band.base_hz + (i as f32 * band.step_hz);
                MscCommand::from_u8(i).map(|cmd| (SymbolValue::MscCmd(cmd), freq))
            })
            .collect()
    }

    fn get_cuelist_candidates(&self) -> Vec<(SymbolValue, f32)> {
        let band = &self.profile.bands.cuelist_band;
        (0..=15)
            .map(|i| {
                let freq = band.base_hz + (i as f32 * band.step_hz);
                (SymbolValue::CueList(i), freq)
            })
            .collect()
    }
}
