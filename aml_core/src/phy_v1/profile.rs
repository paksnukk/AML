use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmlConfig {
    pub protocol: String,
    pub version: String,
    pub active_profile: String,
    pub profiles: HashMap<String, ProfileDefinition>,
    // We can add symbols, encoding, frames structs later if we need to parse them dynamically.
    // For V1, the logic is often hardcoded to the BandSettings structure.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDefinition {
    pub description: String,
    pub audio: AudioSettings,
    pub detection: DetectionSettings,
    pub bands: BandSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    pub sample_rate_hz: u32,
    pub symbol_duration_ms: f32,
    pub guard_gap_ms: f32,
    pub ramp_ms: RampSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampSettings {
    pub attack: f32,
    pub release: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionSettings {
    pub method: String,
    pub min_confidence_db_over_runnerup: f32,
    pub absolute_threshold: ThresholdSettings,
    pub timeouts_ms: TimeoutSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdSettings {
    #[serde(rename = "type")]
    pub threshold_type: String,
    pub margin_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutSettings {
    pub frame_payload: u32,
    pub link_dead: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandSettings {
    pub preamble_hz: f32,
    pub type_band: BandRange,
    pub note_band: BandRange,
    pub octave_band: BandRange,
    pub value_band: BandRange,
    pub msc_cmd_band: BandRange,
    pub cuelist_band: BandRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandRange {
    pub base_hz: f32,
    pub step_hz: f32,
}

impl AmlConfig {
    pub fn from_json(json_str: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json_str)
    }

    pub fn get_active_profile(&self) -> Option<&ProfileDefinition> {
        self.profiles.get(&self.active_profile)
    }
}

impl ProfileDefinition {
    pub fn default_pro_clean() -> Self {
        Self {
            description: "Default Pro Clean Profile".to_string(),
            audio: AudioSettings {
                sample_rate_hz: 48000,
                symbol_duration_ms: 6.0,
                guard_gap_ms: 0.5,
                ramp_ms: RampSettings {
                    attack: 0.3,
                    release: 0.3,
                },
            },
            detection: DetectionSettings {
                method: "goertzel".to_string(),
                min_confidence_db_over_runnerup: 5.0,
                absolute_threshold: ThresholdSettings {
                    threshold_type: "relative_to_noise_floor".to_string(),
                    margin_db: 10.0,
                },
                timeouts_ms: TimeoutSettings {
                    frame_payload: 25,
                    link_dead: 2000,
                },
            },
            bands: BandSettings {
                preamble_hz: 2400.0,
                type_band: BandRange {
                    base_hz: 3000.0,
                    step_hz: 50.0,
                },
                note_band: BandRange {
                    base_hz: 200.0,
                    step_hz: 50.0,
                },
                octave_band: BandRange {
                    base_hz: 1300.0,
                    step_hz: 80.0,
                },
                value_band: BandRange {
                    base_hz: 4500.0,
                    step_hz: 50.0,
                },
                msc_cmd_band: BandRange {
                    base_hz: 3500.0,
                    step_hz: 50.0,
                },
                cuelist_band: BandRange {
                    base_hz: 4200.0,
                    step_hz: 50.0,
                },
            },
        }
    }
}
