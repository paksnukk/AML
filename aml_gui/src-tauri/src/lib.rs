//! Tauri shell over `aml_engine`: commands start/stop engine handles kept in
//! app state, and one forwarder thread per handle turns `EngineEvent`s into
//! the frontend's Tauri events (`log`, `input_level`, `decoded_frame`,
//! `link_stats`, `alarm`, `alarm_cleared`).

pub mod settings;

use aml_core::modem::{decode_samples, encode_to_samples};
use aml_core::phy::PhyConfig;
use aml_engine::{
    audio, format_message, link_state_name, midi, AudioIn, AudioOut, DecoderConfig, DecoderHandle,
    DeviceSelector, EncoderConfig, EncoderHandle, EngineEvent, MidiIn, MidiOut, TransportMessage,
};
use settings::{load_settings, save_settings, AppSettings};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use tauri::{AppHandle, Emitter, State};

/// A running engine handle plus the thread forwarding its events.
struct Running<H> {
    handle: H,
    forwarder: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct AppState {
    decoder: Mutex<Option<Running<DecoderHandle>>>,
    encoder: Mutex<Option<Running<EncoderHandle>>>,
}

#[derive(Clone, serde::Serialize)]
struct DecodedFrameEvent {
    frame_type: String,
    payload: String,
    is_heartbeat: bool,
}

#[derive(Clone, serde::Serialize)]
struct LogEvent {
    message: String,
}

#[derive(Clone, serde::Serialize)]
struct LinkStatsEvent {
    state: String,
    crc_pass: u64,
    crc_fail: u64,
    confidence_db: f32,
    last_seq: Option<u8>,
    sync_lost: u64,
    crc_pass_rate: f32,
    est_latency_ms: f32,
    link_alive: bool,
    buffer_drops: u64,
}

#[derive(Clone, serde::Serialize)]
struct AlarmEvent {
    kind: String,
    message: String,
}

fn parse_profile(profile: &str) -> PhyConfig {
    PhyConfig::from_name(profile)
}

/// Map one engine event to the frontend's Tauri event (same names and
/// payloads as before the engine existed).
fn forward_event(app: &AppHandle, event: EngineEvent) {
    let _ = match event {
        EngineEvent::Log(message) => app.emit("log", LogEvent { message }),
        EngineEvent::Level(rms) => app.emit("input_level", rms),
        EngineEvent::Decoded(msg) => {
            let (frame_type, payload) = format_message(&msg);
            app.emit(
                "decoded_frame",
                DecodedFrameEvent {
                    frame_type,
                    payload,
                    is_heartbeat: matches!(msg, TransportMessage::Heartbeat),
                },
            )
        }
        EngineEvent::Stats(s) => app.emit(
            "link_stats",
            LinkStatsEvent {
                state: link_state_name(s.state).into(),
                crc_pass: s.crc_pass,
                crc_fail: s.crc_fail,
                confidence_db: s.confidence_db,
                last_seq: s.last_seq,
                sync_lost: s.sync_lost,
                crc_pass_rate: s.crc_pass_rate,
                est_latency_ms: s.est_latency_ms,
                link_alive: s.link_alive,
                buffer_drops: s.buffer_drops,
            },
        ),
        EngineEvent::Alarm { kind, message } => app.emit(
            "alarm",
            AlarmEvent {
                kind: kind.as_str().into(),
                message,
            },
        ),
        EngineEvent::AlarmCleared { kind, message } => app.emit(
            "alarm_cleared",
            AlarmEvent {
                kind: kind.as_str().into(),
                message,
            },
        ),
        // No frontend listener yet (queue/latency UI is D2).
        EngineEvent::TxStatus { .. } | EngineEvent::Stopped => Ok(()),
    };
}

/// Forward events until the worker drops its sender.
fn spawn_forwarder(app: AppHandle, events: Receiver<EngineEvent>) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("aml-gui-events".into())
        .spawn(move || {
            for event in events {
                forward_event(&app, event);
            }
        })
        .ok()
}

/// Stop a running handle; once the worker has exited, wait for its forwarder
/// so its last events reach the frontend before anything new starts.
fn shutdown<H>(running: Option<Running<H>>, stop: impl FnOnce(&mut H) -> bool) {
    if let Some(mut r) = running {
        if stop(&mut r.handle) {
            if let Some(f) = r.forwarder.take() {
                let _ = f.join();
            }
        }
    }
}

#[tauri::command]
fn get_input_devices() -> Result<Vec<String>, String> {
    audio::list_input_devices().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_output_devices() -> Result<Vec<String>, String> {
    audio::list_output_devices().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_midi_inputs() -> Result<Vec<String>, String> {
    midi::list_inputs().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_midi_outputs() -> Result<Vec<String>, String> {
    midi::list_outputs().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_settings() -> AppSettings {
    load_settings()
}

#[tauri::command]
fn set_settings(settings: AppSettings) -> Result<(), String> {
    save_settings(&settings)
}

#[tauri::command]
fn get_profile_info(profile: String) -> Result<serde_json::Value, String> {
    let cfg = parse_profile(&profile);
    Ok(serde_json::json!({
        "profile": profile,
        "est_latency_ms": cfg.estimated_first_copy_ms(),
        "symbol_duration_ms": cfg.symbol_duration_ms,
        "guard_gap_ms": cfg.guard_gap_ms,
        "repetition_k": cfg.repetition_k,
        "sample_rate_hz": cfg.sample_rate_hz,
    }))
}

#[tauri::command]
fn run_self_test(profile: String) -> Result<String, String> {
    let config = parse_profile(&profile);
    let messages = vec![
        TransportMessage::NoteOn {
            note: 60,
            velocity: 100,
        },
        TransportMessage::NoteOff { note: 60 },
        TransportMessage::Heartbeat,
    ];
    let samples = encode_to_samples(config.clone(), &messages);
    let decoded = decode_samples(config.clone(), &samples);
    let ok = decoded
        .iter()
        .any(|m| matches!(m, TransportMessage::NoteOn { .. }))
        && decoded
            .iter()
            .any(|m| matches!(m, TransportMessage::NoteOff { .. }));
    if ok {
        Ok(format!(
            "Self-test OK ({}). est latency {:.0} ms. Decoded {} msgs.",
            profile,
            config.estimated_first_copy_ms(),
            decoded.len()
        ))
    } else {
        Err(format!("Self-test FAILED. Got: {:?}", decoded))
    }
}

/// Start the decoder. `audio_in` / `midi_out` are device names as listed by
/// `get_input_devices` / `get_midi_outputs` (`None` = default audio input /
/// no MIDI out). A name that is not present fails in the engine (reported as
/// a log line); no other device is picked.
#[tauri::command]
fn start_decoder(
    app: AppHandle,
    state: State<'_, AppState>,
    audio_in: Option<String>,
    midi_out: Option<String>,
    profile: String,
) -> Result<(), String> {
    let mut slot = state.decoder.lock().unwrap();
    shutdown(slot.take(), DecoderHandle::stop);

    let mut handle = DecoderHandle::start(DecoderConfig {
        audio_in: AudioIn::Device(DeviceSelector::from_name(audio_in)),
        midi_out: match DeviceSelector::from_name(midi_out) {
            DeviceSelector::Default => MidiOut::None,
            sel => MidiOut::Device(sel),
        },
        profile,
        link_dead_ms: None,
    })
    .map_err(|e| e.to_string())?;
    let forwarder = spawn_forwarder(app, handle.take_events());
    *slot = Some(Running { handle, forwarder });
    Ok(())
}

#[tauri::command]
fn stop_decoder(state: State<'_, AppState>) -> Result<(), String> {
    let mut slot = state.decoder.lock().unwrap();
    shutdown(slot.take(), DecoderHandle::stop);
    Ok(())
}

/// Start the encoder. `audio_out` / `midi_in` are device names as listed by
/// `get_output_devices` / `get_midi_inputs` (`None` = default audio output /
/// no MIDI in). A name that is not present fails in the engine; no other
/// device is picked.
#[tauri::command]
fn start_encoder(
    app: AppHandle,
    state: State<'_, AppState>,
    audio_out: Option<String>,
    midi_in: Option<String>,
    profile: String,
    simulate: bool,
) -> Result<(), String> {
    let mut slot = state.encoder.lock().unwrap();
    shutdown(slot.take(), EncoderHandle::stop);

    let mut handle = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Device(DeviceSelector::from_name(audio_out)),
        midi_in: match DeviceSelector::from_name(midi_in) {
            DeviceSelector::Default => MidiIn::None,
            sel => MidiIn::Device(sel),
        },
        profile,
        simulate,
    })
    .map_err(|e| e.to_string())?;
    let forwarder = spawn_forwarder(app, handle.take_events());
    *slot = Some(Running { handle, forwarder });
    Ok(())
}

#[tauri::command]
fn stop_encoder(state: State<'_, AppState>) -> Result<(), String> {
    let mut slot = state.encoder.lock().unwrap();
    shutdown(slot.take(), EncoderHandle::stop);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_input_devices,
            get_output_devices,
            get_midi_inputs,
            get_midi_outputs,
            get_settings,
            set_settings,
            get_profile_info,
            run_self_test,
            start_decoder,
            stop_decoder,
            start_encoder,
            stop_encoder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
