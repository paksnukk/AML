//! Persistent GUI settings (device indices, profile, mode).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub profile: String,
    pub mode: String,
    pub audio_in: Option<usize>,
    pub audio_out: Option<usize>,
    pub midi_in: Option<usize>,
    pub midi_out: Option<usize>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            profile: "robust".into(),
            mode: "both".into(),
            audio_in: None,
            audio_out: None,
            midi_in: None,
            midi_out: None,
        }
    }
}

fn settings_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aml")
}

pub fn settings_path() -> PathBuf {
    settings_dir().join("settings.json")
}

pub fn load_settings() -> AppSettings {
    let path = settings_path();
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => AppSettings::default(),
    }
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let dir = settings_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(settings_path(), json).map_err(|e| e.to_string())
}
