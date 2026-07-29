pub mod audio;
pub mod settings;

use aml_core::modem::{decode_samples, encode_to_samples, AmlModemRx, AmlModemTx, ModemOutput};
use aml_core::phy::{LinkState, PhyConfig};
use aml_core::transport::{midi_to_message, TransportMessage};
use cpal::traits::StreamTrait;
use midir::{MidiInput, MidiOutput};
use ringbuf::HeapRb;
use settings::{load_settings, save_settings, AppSettings};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

struct WorkerGate {
    stop: AtomicBool,
    running: AtomicBool,
}

impl WorkerGate {
    fn new() -> Self {
        Self {
            stop: AtomicBool::new(true),
            running: AtomicBool::new(false),
        }
    }

    fn request_stop_and_wait(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.running.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn mark_stopped(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

struct AppState {
    decode_gate: Arc<WorkerGate>,
    encode_gate: Arc<WorkerGate>,
    /// Shared stop flags so commands can flip them
    decode_stop: Mutex<Option<Arc<AtomicBool>>>,
    encode_stop: Mutex<Option<Arc<AtomicBool>>>,
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
    let midi_in = MidiInput::new("aml list in").map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for port in midi_in.ports() {
        names.push(
            midi_in
                .port_name(&port)
                .unwrap_or_else(|_| "Unknown".into()),
        );
    }
    Ok(names)
}

#[tauri::command]
fn get_midi_outputs() -> Result<Vec<String>, String> {
    let midi_out = MidiOutput::new("aml list out").map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for port in midi_out.ports() {
        names.push(
            midi_out
                .port_name(&port)
                .unwrap_or_else(|_| "Unknown".into()),
        );
    }
    Ok(names)
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
    let ok = decoded.iter().any(|m| matches!(m, TransportMessage::NoteOn { .. }))
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
        Err(format!(
            "Self-test FAILED. Got: {:?}",
            decoded
        ))
    }
}

#[tauri::command]
fn start_decoder(
    app: AppHandle,
    state: State<'_, AppState>,
    audio_in: Option<usize>,
    midi_out: Option<usize>,
    profile: String,
) -> Result<(), String> {
    state.decode_gate.request_stop_and_wait();
    if let Some(prev) = state.decode_stop.lock().unwrap().take() {
        prev.store(true, Ordering::SeqCst);
    }
    state.decode_gate.request_stop_and_wait();

    let stop = Arc::new(AtomicBool::new(false));
    *state.decode_stop.lock().unwrap() = Some(stop.clone());
    state.decode_gate.stop.store(false, Ordering::SeqCst);
    state.decode_gate.running.store(true, Ordering::SeqCst);

    let gate = state.decode_gate.clone();
    let app_handle = app.clone();

    thread::spawn(move || {
        let _ = app_handle.emit(
            "log",
            LogEvent {
                message: format!("Starting decoder (profile={})...", profile),
            },
        );

        let mut config = parse_profile(&profile);
        let est_latency = config.estimated_first_copy_ms();
        let preferred_sample_rate = config.sample_rate_hz;

        let midi_conn = match midi_out {
            Some(idx) => match open_midi_out(idx) {
                Ok(c) => Some(Arc::new(Mutex::new(c))),
                Err(e) => {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("MIDI out error: {}", e),
                        },
                    );
                    None
                }
            },
            None => {
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: "No MIDI out selected — decode to UI only.".into(),
                    },
                );
                None
            }
        };

        let drops = Arc::new(AtomicU64::new(0));
        let drops_cb = drops.clone();
        let ring = HeapRb::<f32>::new(preferred_sample_rate as usize * 2);
        let (mut producer, mut consumer) = ring.split();

        let stream_result = audio::setup_input_stream(
            audio_in,
            Some(preferred_sample_rate),
            move |data: &[f32], channels: usize| {
                for frame in data.chunks(channels.max(1)) {
                    if let Some(&s) = frame.first() {
                        if producer.push(s).is_err() {
                            drops_cb.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            },
        );

        match stream_result {
            Ok((stream, stream_sample_rate, channels)) => {
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: format!(
                            "Audio in {} Hz, {} ch (ch0). est latency {:.0} ms.",
                            stream_sample_rate, channels, est_latency
                        ),
                    },
                );
                if stream_sample_rate != preferred_sample_rate {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!(
                                "Sample rate adapted {} → {} Hz",
                                preferred_sample_rate, stream_sample_rate
                            ),
                        },
                    );
                    config = config.with_sample_rate(stream_sample_rate);
                }

                if let Err(e) = stream.play() {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("Failed to play input: {}", e),
                        },
                    );
                    gate.mark_stopped();
                    return;
                }

                let mut rx = AmlModemRx::new(config);
                let mut rms_accumulator = 0.0f32;
                let mut sample_count_for_meter = 0usize;
                let meter_report_interval = (stream_sample_rate as usize) / 20;
                let mut last_stats_emit = Instant::now();
                let mut sync_lost: u64 = 0;
                let mut link_alive = false;

                while !stop.load(Ordering::SeqCst) {
                    let mut chunk = Vec::new();
                    while let Some(sample) = consumer.pop() {
                        chunk.push(sample);
                        rms_accumulator += sample * sample;
                        sample_count_for_meter += 1;
                        if sample_count_for_meter >= meter_report_interval {
                            let rms = (rms_accumulator / sample_count_for_meter as f32).sqrt();
                            let _ = app_handle.emit("input_level", rms);
                            rms_accumulator = 0.0;
                            sample_count_for_meter = 0;
                        }
                        if chunk.len() >= 512 {
                            break;
                        }
                    }

                    if chunk.is_empty() {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }

                    for out in rx.push_samples(&chunk) {
                        match out {
                            ModemOutput::Message(msg) => {
                                let is_hb = matches!(msg, TransportMessage::Heartbeat);
                                if is_hb {
                                    link_alive = true;
                                }
                                let (frame_type, payload) = format_message(&msg);
                                let _ = app_handle.emit(
                                    "decoded_frame",
                                    DecodedFrameEvent {
                                        frame_type,
                                        payload,
                                        is_heartbeat: is_hb,
                                    },
                                );
                            }
                            ModemOutput::Midi(bytes) => {
                                if let Some(conn) = &midi_conn {
                                    if let Ok(mut c) = conn.lock() {
                                        let _ = c.send(&bytes);
                                    }
                                }
                            }
                            ModemOutput::LinkDead => {
                                link_alive = false;
                                let _ = app_handle.emit(
                                    "alarm",
                                    AlarmEvent {
                                        kind: "link_dead".into(),
                                        message: "Link dead — All Notes Off sent".into(),
                                    },
                                );
                                let _ = app_handle.emit(
                                    "log",
                                    LogEvent {
                                        message: "Link dead — All Notes Off".into(),
                                    },
                                );
                            }
                            ModemOutput::SyncLost => {
                                sync_lost = sync_lost.saturating_add(1);
                            }
                            ModemOutput::Stats(stats) => {
                                if last_stats_emit.elapsed() >= Duration::from_millis(200) {
                                    let total = stats.crc_pass + stats.crc_fail;
                                    let rate = if total == 0 {
                                        100.0
                                    } else {
                                        100.0 * stats.crc_pass as f32 / total as f32
                                    };
                                    if matches!(stats.link_state, LinkState::Locked) {
                                        link_alive = true;
                                    }
                                    if matches!(stats.link_state, LinkState::Dead) {
                                        link_alive = false;
                                    }
                                    let _ = app_handle.emit(
                                        "link_stats",
                                        LinkStatsEvent {
                                            state: link_state_str(stats.link_state).into(),
                                            crc_pass: stats.crc_pass,
                                            crc_fail: stats.crc_fail,
                                            confidence_db: stats.last_confidence_db,
                                            last_seq: stats.last_seq,
                                            sync_lost,
                                            crc_pass_rate: rate,
                                            est_latency_ms: est_latency,
                                            link_alive,
                                            buffer_drops: drops.load(Ordering::Relaxed),
                                        },
                                    );
                                    last_stats_emit = Instant::now();
                                }
                            }
                        }
                    }
                }

                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: "Decoder stopped.".into(),
                    },
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: format!("Audio input error: {}", e),
                    },
                );
            }
        }
        gate.mark_stopped();
    });

    Ok(())
}

#[tauri::command]
fn stop_decoder(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(stop) = state.decode_stop.lock().unwrap().as_ref() {
        stop.store(true, Ordering::SeqCst);
    }
    state.decode_gate.request_stop_and_wait();
    Ok(())
}

#[tauri::command]
fn start_encoder(
    app: AppHandle,
    state: State<'_, AppState>,
    audio_out: Option<usize>,
    midi_in: Option<usize>,
    profile: String,
    simulate: bool,
) -> Result<(), String> {
    state.encode_gate.request_stop_and_wait();
    if let Some(prev) = state.encode_stop.lock().unwrap().take() {
        prev.store(true, Ordering::SeqCst);
    }
    state.encode_gate.request_stop_and_wait();

    let stop = Arc::new(AtomicBool::new(false));
    *state.encode_stop.lock().unwrap() = Some(stop.clone());
    state.encode_gate.stop.store(false, Ordering::SeqCst);
    state.encode_gate.running.store(true, Ordering::SeqCst);

    let gate = state.encode_gate.clone();
    let app_handle = app.clone();

    thread::spawn(move || {
        let _ = app_handle.emit(
            "log",
            LogEvent {
                message: format!("Starting encoder (profile={}, simulate={})...", profile, simulate),
            },
        );

        let mut config = parse_profile(&profile);
        let est_latency = config.estimated_first_copy_ms();
        let preferred_sample_rate = config.sample_rate_hz;
        let heartbeat_ms = config.heartbeat_interval_ms as u64;

        let pending: Arc<Mutex<VecDeque<TransportMessage>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        // Keep MIDI connection alive for the thread lifetime
        let _midi_conn = if let Some(idx) = midi_in {
            match open_midi_in(idx, pending.clone()) {
                Ok(c) => {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("MIDI in index {} connected.", idx),
                        },
                    );
                    Some(c)
                }
                Err(e) => {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("MIDI in error: {}", e),
                        },
                    );
                    None
                }
            }
        } else {
            None
        };

        let drops = Arc::new(AtomicU64::new(0));
        let ring = HeapRb::<f32>::new(preferred_sample_rate as usize * 4);
        let (mut producer, mut consumer) = ring.split();

        let stream_result = audio::setup_output_stream(
            audio_out,
            Some(preferred_sample_rate),
            move |data: &mut [f32], channels: usize| {
                for frame in data.chunks_mut(channels.max(1)) {
                    let sample = consumer.pop().unwrap_or(0.0);
                    for s in frame.iter_mut() {
                        *s = sample;
                    }
                }
            },
        );

        match stream_result {
            Ok((stream, stream_sample_rate, channels)) => {
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: format!(
                            "Audio out {} Hz, {} ch. est latency {:.0} ms.",
                            stream_sample_rate, channels, est_latency
                        ),
                    },
                );
                if stream_sample_rate != preferred_sample_rate {
                    config = config.with_sample_rate(stream_sample_rate);
                }
                if let Err(e) = stream.play() {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("Failed to play output: {}", e),
                        },
                    );
                    gate.mark_stopped();
                    return;
                }

                let mut tx = AmlModemTx::new(config.clone());
                let mut last_heartbeat = Instant::now();
                let mut note_idx = 0usize;
                let scale = [0u8, 2, 4, 5, 7, 9, 11, 12];
                let mut last_sim = Instant::now() - Duration::from_secs(1);
                let use_sim = simulate || midi_in.is_none();

                while !stop.load(Ordering::SeqCst) {
                    let msgs: Vec<TransportMessage> = {
                        let mut q = pending.lock().unwrap();
                        q.drain(..).collect()
                    };
                    for msg in msgs {
                        let _ = app_handle.emit(
                            "log",
                            LogEvent {
                                message: format!("TX {:?}", msg),
                            },
                        );
                        push_audio(&mut producer, &tx.encode_message(&msg), &stop, &drops);
                    }

                    if use_sim && last_sim.elapsed() >= Duration::from_millis(1000) {
                        let note = 60 + scale[note_idx % scale.len()];
                        let on = TransportMessage::NoteOn {
                            note,
                            velocity: 100,
                        };
                        push_audio(&mut producer, &tx.encode_message(&on), &stop, &drops);
                        let off = TransportMessage::NoteOff { note };
                        push_audio(&mut producer, &tx.encode_message(&off), &stop, &drops);
                        let _ = app_handle.emit(
                            "log",
                            LogEvent {
                                message: format!("TX sim Note {}", note),
                            },
                        );
                        note_idx += 1;
                        last_sim = Instant::now();
                    }

                    if last_heartbeat.elapsed() >= Duration::from_millis(heartbeat_ms) {
                        push_audio(&mut producer, &tx.encode_heartbeat(), &stop, &drops);
                        last_heartbeat = Instant::now();
                    }

                    let d = drops.load(Ordering::Relaxed);
                    if d > 0 && d % 1000 == 0 {
                        let _ = app_handle.emit(
                            "log",
                            LogEvent {
                                message: format!("Audio buffer drops: {}", d),
                            },
                        );
                    }

                    thread::sleep(Duration::from_millis(5));
                }

                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: "Encoder stopped.".into(),
                    },
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: format!("Audio output error: {}", e),
                    },
                );
            }
        }
        gate.mark_stopped();
    });

    Ok(())
}

#[tauri::command]
fn stop_encoder(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(stop) = state.encode_stop.lock().unwrap().as_ref() {
        stop.store(true, Ordering::SeqCst);
    }
    state.encode_gate.request_stop_and_wait();
    Ok(())
}

// Back-compat aliases used by older frontend bindings during transition
#[tauri::command]
fn start_listening(
    app: AppHandle,
    state: State<'_, AppState>,
    device_index: Option<usize>,
) -> Result<(), String> {
    start_decoder(app, state, device_index, None, "robust".into())
}

#[tauri::command]
fn stop_listening(state: State<'_, AppState>) -> Result<(), String> {
    stop_decoder(state)
}

#[tauri::command]
fn start_test_sequence(
    app: AppHandle,
    state: State<'_, AppState>,
    device_index: Option<usize>,
) -> Result<(), String> {
    start_encoder(app, state, device_index, None, "robust".into(), true)
}

#[tauri::command]
fn stop_playing(state: State<'_, AppState>) -> Result<(), String> {
    stop_encoder(state)
}

fn open_midi_out(index: usize) -> Result<midir::MidiOutputConnection, String> {
    let midi_out = MidiOutput::new("aml decode out").map_err(|e| e.to_string())?;
    let ports = midi_out.ports();
    let port = ports
        .get(index)
        .ok_or_else(|| format!("MIDI out index {} not found", index))?;
    midi_out
        .connect(port, "aml out")
        .map_err(|e| format!("MIDI connect: {}", e))
}

fn open_midi_in(
    index: usize,
    pending: Arc<Mutex<VecDeque<TransportMessage>>>,
) -> Result<midir::MidiInputConnection<()>, String> {
    let midi_in = MidiInput::new("aml encode in").map_err(|e| e.to_string())?;
    let ports = midi_in.ports();
    let port = ports
        .get(index)
        .ok_or_else(|| format!("MIDI in index {} not found", index))?;
    midi_in
        .connect(
            port,
            "aml in",
            move |_stamp, message, _| {
                if let Some(msg) = midi_to_message(message) {
                    if let Ok(mut q) = pending.lock() {
                        q.push_back(msg);
                    }
                }
            },
            (),
        )
        .map_err(|e| format!("MIDI connect: {}", e))
}

fn push_audio(
    producer: &mut ringbuf::HeapProducer<f32>,
    samples: &[f32],
    stop: &AtomicBool,
    drops: &AtomicU64,
) {
    for &sample in samples {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut attempts = 0;
        while producer.is_full() {
            if stop.load(Ordering::Relaxed) || attempts > 2000 {
                drops.fetch_add(1, Ordering::Relaxed);
                break;
            }
            thread::sleep(Duration::from_millis(1));
            attempts += 1;
        }
        if producer.push(sample).is_err() {
            drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn link_state_str(state: LinkState) -> &'static str {
    match state {
        LinkState::Search => "Search",
        LinkState::Locked => "Locked",
        LinkState::Dead => "Dead",
    }
}

fn format_message(msg: &TransportMessage) -> (String, String) {
    match msg {
        TransportMessage::NoteOn { note, velocity } => (
            "NoteOn".into(),
            format!("note={} vel={}", note, velocity),
        ),
        TransportMessage::NoteOff { note } => ("NoteOff".into(), format!("note={}", note)),
        TransportMessage::ProgramChange { program } => {
            ("ProgramChange".into(), format!("program={}", program))
        }
        TransportMessage::MscLite { cmd, cuelist, cue } => (
            "MscLite".into(),
            format!("{:?} cuelist={} cue={}", cmd, cuelist, cue),
        ),
        TransportMessage::Heartbeat => ("Heartbeat".into(), String::new()),
        TransportMessage::Panic => ("Panic".into(), String::new()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            decode_gate: Arc::new(WorkerGate::new()),
            encode_gate: Arc::new(WorkerGate::new()),
            decode_stop: Mutex::new(None),
            encode_stop: Mutex::new(None),
        })
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
            stop_encoder,
            start_listening,
            stop_listening,
            start_test_sequence,
            stop_playing
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
