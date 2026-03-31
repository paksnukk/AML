pub mod audio;

use aml_core::decoder::Decoder;
use aml_core::encoder::Encoder;
use aml_core::profile::ProfileDefinition;
use aml_core::protocol::{FrameType, SymbolValue};
use cpal::traits::StreamTrait;
use ringbuf::HeapRb;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

struct AppState {
    // Flags to control the worker threads
    is_listening: Arc<AtomicBool>,
    is_playing: Arc<AtomicBool>,
}

#[derive(Clone, serde::Serialize)]
struct DecodedFrameEvent {
    frame_type: String,
    payload: String,
}

#[derive(Clone, serde::Serialize)]
struct LogEvent {
    message: String,
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
fn start_listening(
    app: AppHandle,
    state: State<'_, AppState>,
    device_index: Option<usize>,
) -> Result<(), String> {
    // 1. Signal any existing thread to stop
    state.is_listening.store(false, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(100));

    // 2. Set flag to true for new thread
    state.is_listening.store(true, Ordering::SeqCst);
    let keep_running = state.is_listening.clone();

    // 3. Spawn the worker thread
    let app_handle = app.clone();

    thread::spawn(move || {
        let _ = app_handle.emit(
            "log",
            LogEvent {
                message: "Initializing audio stream...".into(),
            },
        );

        let mut profile = ProfileDefinition::default_pro_clean();
        // Relax thresholds for over-the-air testing
        profile.detection.min_confidence_db_over_runnerup = 1.0;
        profile.detection.absolute_threshold.margin_db = 1.0;
        profile.audio.ramp_ms.attack = 0.0;
        let preferred_sample_rate = profile.audio.sample_rate_hz;

        // RingBuffer: Audio Thread -> Decoder Thread
        let ring = HeapRb::<f32>::new(preferred_sample_rate as usize * 2);
        let (mut producer, mut consumer) = ring.split();

        // We create the stream HERE, inside the thread.
        // This avoids Send issues because the Stream never leaves this thread.
        let stream_result = audio::setup_input_stream(
            device_index,
            Some(preferred_sample_rate),
            move |data: &[f32]| {
                for &sample in data {
                    let _ = producer.push(sample);
                }
            },
        );

        match stream_result {
            Ok((stream, stream_sample_rate)) => {
                if stream_sample_rate != preferred_sample_rate {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!(
                                "Input stream sample rate: {} Hz (preferred {} Hz). Using {} Hz.",
                                stream_sample_rate, preferred_sample_rate, stream_sample_rate
                            ),
                        },
                    );
                } else {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!(
                                "Input stream sample rate: {} Hz.",
                                stream_sample_rate
                            ),
                        },
                    );
                }
                profile.audio.sample_rate_hz = stream_sample_rate;

                if let Err(e) = stream.play() {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("Failed to play stream: {}", e),
                        },
                    );
                    return;
                }

                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: "Listening started.".into(),
                    },
                );

                // Decoder Setup
                let mut decoder = Decoder::new(profile.clone());
                let symbol_duration_ms = profile.audio.symbol_duration_ms;
                let samples_per_symbol =
                    (stream_sample_rate as f32 * symbol_duration_ms / 1000.0) as usize;
                let samples_guard_gap =
                    (stream_sample_rate as f32 * profile.audio.guard_gap_ms / 1000.0) as usize;
                let hop_size = samples_per_symbol / 4;
                let symbol_total_ms = symbol_duration_ms + profile.audio.guard_gap_ms;
                let max_misses = ((profile.detection.timeouts_ms.frame_payload as f32)
                    / symbol_total_ms)
                    .ceil() as usize;
                let max_misses = max_misses.max(2);
                let mut analysis_buffer: VecDeque<f32> =
                    VecDeque::with_capacity(samples_per_symbol * 2);

                // Metering variables
                let mut rms_accumulator = 0.0;
                let mut sample_count_for_meter = 0;
                let meter_report_interval = 2400; // Emit level every ~50ms (at 48k)
                let mut missed_symbol_count = 0usize;
                let preamble_min_power = 200.0;

                // Loop
                while keep_running.load(Ordering::Relaxed) {
                    // 1. Consume from RingBuffer
                    while let Some(sample) = consumer.pop() {
                        analysis_buffer.push_back(sample);

                        // Calculate Level
                        rms_accumulator += sample * sample;
                        sample_count_for_meter += 1;

                        if sample_count_for_meter >= meter_report_interval {
                            let rms = (rms_accumulator / sample_count_for_meter as f32).sqrt();
                            // Send level to frontend (event name: "input_level")
                            let _ = app_handle.emit("input_level", rms);
                            rms_accumulator = 0.0;
                            sample_count_for_meter = 0;
                        }
                    }

                    // 2. Decoder Logic
                    // We wait for symbol + guard gap to ensure we can advance correctly
                    let samples_required = samples_per_symbol + samples_guard_gap;
                    if analysis_buffer.len() >= samples_required {
                        let window: Vec<f32> = analysis_buffer
                            .iter()
                            .take(samples_per_symbol)
                            .cloned()
                            .collect();
                        let candidates = decoder.get_scan_candidates();

                        if let Some((symbol, power)) = decoder.detect_symbol(&window, &candidates) {
                            let was_wait_preamble =
                                matches!(decoder.state, aml_core::decoder::DecoderState::WaitPreamble);
                            let mut preamble_align_offset = 0usize;
                            // Filter weak Preambles (noise/echo)
                            if symbol == SymbolValue::Preamble {
                                if power < preamble_min_power {
                                    // Too weak, probably noise. Skip small amount and retry.
                                    if analysis_buffer.len() >= hop_size {
                                        analysis_buffer.drain(0..hop_size);
                                    }
                                    continue;
                                }

                                if was_wait_preamble {
                                    // Try to align to the strongest preamble offset within the hop window.
                                    let max_offset = hop_size.min(
                                        analysis_buffer
                                            .len()
                                            .saturating_sub(samples_per_symbol),
                                    );
                                    let step = (hop_size / 2).max(1);
                                    let mut offset = step;
                                    let mut best_power = power;
                                    while offset <= max_offset {
                                        let offset_window: Vec<f32> = analysis_buffer
                                            .iter()
                                            .skip(offset)
                                            .take(samples_per_symbol)
                                            .cloned()
                                            .collect();
                                        let offset_power = decoder.goertzel_power(
                                            &offset_window,
                                            decoder.get_preamble_freq(),
                                        );
                                        if offset_power > best_power {
                                            best_power = offset_power;
                                            preamble_align_offset = offset;
                                        }
                                        offset += step;
                                    }
                                }

                                let _ = app_handle.emit(
                                    "log",
                                    LogEvent {
                                        message: format!("Detected Preamble (P: {:.0})", power),
                                    },
                                );
                            }

                            if let Some(frame) = decoder.handle_symbol(symbol) {
                                let _ = app_handle.emit(
                                    "decoded_frame",
                                    DecodedFrameEvent {
                                        frame_type: format!("{:?}", frame.frame_type),
                                        payload: format!("{:?}", frame.payload),
                                    },
                                );
                            }
                            missed_symbol_count = 0;
                            // Advance (consume symbol + guard gap), with optional alignment after preamble
                            let mut advance = samples_required;
                            if symbol == SymbolValue::Preamble
                                && was_wait_preamble
                                && preamble_align_offset > 0
                                && preamble_align_offset < samples_required
                            {
                                advance = samples_required - preamble_align_offset;
                            }
                            analysis_buffer.drain(0..advance);
                        } else {
                            // Advance / Hop
                            match decoder.state {
                                aml_core::decoder::DecoderState::WaitPreamble => {
                                    missed_symbol_count = 0;
                                    if analysis_buffer.len() >= hop_size {
                                        analysis_buffer.drain(0..hop_size);
                                    }
                                }
                                _ => {
                                    missed_symbol_count += 1;
                                    if missed_symbol_count >= max_misses {
                                        let _ = app_handle.emit(
                                            "log",
                                            LogEvent {
                                                message: "Sync Lost (Missing Symbol)".into(),
                                            },
                                        );
                                        decoder.reset_state();
                                        missed_symbol_count = 0;
                                    }
                                    if analysis_buffer.len() >= hop_size {
                                        analysis_buffer.drain(0..hop_size);
                                    }
                                }
                            }
                        }
                    } else {
                        thread::sleep(Duration::from_millis(5));
                    }
                }

                // When loop exits, `stream` is dropped here.
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: "Listener thread stopped.".into(),
                    },
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: format!("Error setting up audio: {}", e),
                    },
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
fn stop_listening(state: State<'_, AppState>) -> Result<(), String> {
    state.is_listening.store(false, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn start_test_sequence(
    app: AppHandle,
    state: State<'_, AppState>,
    device_index: Option<usize>,
) -> Result<(), String> {
    state.is_playing.store(false, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(100));

    state.is_playing.store(true, Ordering::SeqCst);
    let keep_running = state.is_playing.clone();
    let app_handle = app.clone();

    thread::spawn(move || {
        let _ = app_handle.emit(
            "log",
            LogEvent {
                message: "Initializing test sequence...".into(),
            },
        );

        let mut profile = ProfileDefinition::default_pro_clean();
        profile.audio.ramp_ms.attack = 0.0;
        let preferred_sample_rate = profile.audio.sample_rate_hz;

        let ring = HeapRb::<f32>::new(preferred_sample_rate as usize * 2);
        let (mut producer, mut consumer) = ring.split();

        let stream_result = audio::setup_output_stream(
            device_index,
            Some(preferred_sample_rate),
            move |data: &mut [f32]| {
                for sample in data.iter_mut() {
                    *sample = consumer.pop().unwrap_or(0.0);
                }
            },
        );

        match stream_result {
            Ok((stream, stream_sample_rate)) => {
                if stream_sample_rate != preferred_sample_rate {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!(
                                "Output stream sample rate: {} Hz (preferred {} Hz). Using {} Hz.",
                                stream_sample_rate, preferred_sample_rate, stream_sample_rate
                            ),
                        },
                    );
                } else {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!(
                                "Output stream sample rate: {} Hz.",
                                stream_sample_rate
                            ),
                        },
                    );
                }
                profile.audio.sample_rate_hz = stream_sample_rate;
                if let Err(e) = stream.play() {
                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("Failed to play output: {}", e),
                        },
                    );
                    return;
                }

                let encoder = Encoder::new(profile);
                let mut note_idx = 0;
                let scale = [0, 2, 4, 5, 7, 9, 11, 12];

                while keep_running.load(Ordering::Relaxed) {
                    let note_offset = scale[note_idx % scale.len()];
                    let base_note = 60;
                    let note = base_note + note_offset;

                    let frame_symbols = vec![
                        SymbolValue::Preamble,
                        SymbolValue::Type(FrameType::NoteOn),
                        SymbolValue::Note((note % 12) as u8),
                        SymbolValue::Octave((note / 12) as u8),
                        SymbolValue::Value(12),
                    ];

                    let samples = encoder.encode_frame(&frame_symbols);

                    for sample in samples {
                        let mut attempts = 0;
                        while producer.is_full() {
                            if !keep_running.load(Ordering::Relaxed) || attempts > 1000 {
                                break;
                            }
                            thread::sleep(Duration::from_millis(1));
                            attempts += 1;
                        }
                        if !keep_running.load(Ordering::Relaxed) {
                            break;
                        }
                        let _ = producer.push(sample);
                    }

                    let _ = app_handle.emit(
                        "log",
                        LogEvent {
                            message: format!("Sent Note: {}", note),
                        },
                    );

                    // Sleep between notes
                    for _ in 0..10 {
                        if !keep_running.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                    note_idx += 1;
                }
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: "Encoder thread stopped.".into(),
                    },
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "log",
                    LogEvent {
                        message: format!("Error setting up output: {}", e),
                    },
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
fn stop_playing(state: State<'_, AppState>) -> Result<(), String> {
    state.is_playing.store(false, Ordering::SeqCst);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            is_listening: Arc::new(AtomicBool::new(false)),
            is_playing: Arc::new(AtomicBool::new(false)),
        })
        .invoke_handler(tauri::generate_handler![
            get_input_devices,
            get_output_devices,
            start_listening,
            stop_listening,
            start_test_sequence,
            stop_playing
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
