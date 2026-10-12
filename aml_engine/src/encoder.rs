//! TX worker: MIDI in / injected messages → `AmlModemTx` → audio out.

use crate::audio::{AudioOut, AudioSink};
use crate::events::EngineEvent;
use crate::midi::{self, MidiCallback, MidiIn};
use crate::{closed_receiver, Emitter, Worker};
use aml_core::modem::AmlModemTx;
use aml_core::phy::PhyConfig;
use aml_core::transport::{midi_to_message, TransportMessage};
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Encoder settings.
pub struct EncoderConfig {
    pub audio_out: AudioOut,
    pub midi_in: MidiIn,
    /// PHY profile name ("robust" / "fast"; unknown names mean robust).
    pub profile: String,
    /// Send a C-major scale (one note per second). Also on when `midi_in`
    /// is [`MidiIn::None`].
    pub simulate: bool,
}

type Pending = Arc<Mutex<VecDeque<TransportMessage>>>;

/// A running encoder worker. Dropping it stops the worker.
pub struct EncoderHandle {
    worker: Worker,
    events: Receiver<EngineEvent>,
    pending: Pending,
}

impl EncoderHandle {
    /// Spawn the encoder thread. Device errors (audio or MIDI) do not fail
    /// this call; they are reported as `Log` events, after which the worker
    /// either continues without MIDI in or ends with `Stopped`.
    pub fn start(config: EncoderConfig) -> Result<Self> {
        let pending: Pending = Arc::new(Mutex::new(VecDeque::new()));
        let pending_w = pending.clone();
        let (worker, events) = Worker::spawn("aml-encoder", move |ev, stop| {
            run(config, pending_w, ev, stop)
        })?;
        Ok(Self {
            worker,
            events,
            pending,
        })
    }

    /// Queue a message for transmission (same queue as MIDI input).
    pub fn send(&self, msg: TransportMessage) {
        if let Ok(mut q) = self.pending.lock() {
            q.push_back(msg);
        }
    }

    /// The worker's event channel.
    pub fn events(&self) -> &Receiver<EngineEvent> {
        &self.events
    }

    /// Take ownership of the event channel (e.g. to forward it from another
    /// thread). Afterwards [`events`](Self::events) returns a closed channel.
    pub fn take_events(&mut self) -> Receiver<EngineEvent> {
        std::mem::replace(&mut self.events, closed_receiver())
    }

    /// True until the worker thread has finished.
    pub fn is_running(&self) -> bool {
        self.worker.is_running()
    }

    /// Stop and join the worker (waits at most [`crate::STOP_TIMEOUT`]).
    /// Returns false if the worker did not exit in time.
    pub fn stop(&mut self) -> bool {
        self.worker.stop()
    }
}

impl Drop for EncoderHandle {
    fn drop(&mut self) {
        self.worker.stop();
    }
}

/// Loop period.
const TICK: Duration = Duration::from_millis(5);
/// Simulated note interval.
const SIM_INTERVAL: Duration = Duration::from_millis(1000);
/// Minimum interval between `TxStatus` events.
const STATUS_INTERVAL: Duration = Duration::from_millis(200);

fn run(cfg: EncoderConfig, pending: Pending, ev: &Emitter, stop: &AtomicBool) {
    ev.log(format!(
        "Starting encoder (profile={}, simulate={})...",
        cfg.profile, cfg.simulate
    ));

    let mut config = PhyConfig::from_name(&cfg.profile);
    let est_latency = config.estimated_first_copy_ms();
    let preferred_sample_rate = config.sample_rate_hz;
    let heartbeat_ms = config.heartbeat_interval_ms as u64;
    let use_sim = cfg.simulate || matches!(cfg.midi_in, MidiIn::None);

    let on_midi = {
        let pending = pending.clone();
        move || -> MidiCallback {
            Box::new(move |message: &[u8]| {
                if let Some(msg) = midi_to_message(message) {
                    if let Ok(mut q) = pending.lock() {
                        q.push_back(msg);
                    }
                }
            })
        }
    };
    // Keeps the MIDI input connected for the worker's lifetime.
    let _midi_conn = match cfg.midi_in {
        MidiIn::None => None,
        MidiIn::Device(sel) => match midi::open_input(&sel, on_midi()) {
            Ok(c) => {
                ev.log(format!("MIDI in index {} connected.", sel));
                Some(midi::MidiInputGuard::new(c))
            }
            Err(e) => {
                ev.log(format!("MIDI in error: {}", e));
                None
            }
        },
        MidiIn::Custom(source) => match source.connect(on_midi()) {
            Ok(g) => {
                ev.log("MIDI in (custom source) connected.");
                Some(g)
            }
            Err(e) => {
                ev.log(format!("MIDI in error: {}", e));
                None
            }
        },
    };

    let drops = AtomicU64::new(0);
    let mut sink = match cfg
        .audio_out
        .open(preferred_sample_rate, preferred_sample_rate as usize * 4)
    {
        Ok(s) => s,
        Err(e) => {
            ev.log(format!("Audio output error: {}", e));
            return;
        }
    };
    let stream_sample_rate = sink.sample_rate();

    ev.log(format!(
        "Audio out {} Hz, {} ch. est latency {:.0} ms.",
        stream_sample_rate,
        sink.channels(),
        est_latency
    ));
    if stream_sample_rate != preferred_sample_rate {
        config = config.with_sample_rate(stream_sample_rate);
    }
    if let Err(e) = sink.start() {
        ev.log(format!("Failed to play output: {}", e));
        return;
    }

    let mut tx = AmlModemTx::new(config.clone());
    let mut last_heartbeat = Instant::now();
    let mut note_idx = 0usize;
    let scale = [0u8, 2, 4, 5, 7, 9, 11, 12];
    let mut last_sim = Instant::now() - SIM_INTERVAL;
    let mut last_status = Instant::now();
    let sink = sink.as_mut();

    while !stop.load(Ordering::SeqCst) {
        let msgs: Vec<TransportMessage> = match pending.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        };
        for msg in msgs {
            ev.log(format!("TX {:?}", msg));
            push_audio(sink, &tx.encode_message(&msg), stop, &drops);
        }

        if use_sim && last_sim.elapsed() >= SIM_INTERVAL {
            let note = 60 + scale[note_idx % scale.len()];
            let on = TransportMessage::NoteOn {
                note,
                velocity: 100,
            };
            push_audio(sink, &tx.encode_message(&on), stop, &drops);
            let off = TransportMessage::NoteOff { note };
            push_audio(sink, &tx.encode_message(&off), stop, &drops);
            ev.log(format!("TX sim Note {}", note));
            note_idx += 1;
            last_sim = Instant::now();
        }

        if last_heartbeat.elapsed() >= Duration::from_millis(heartbeat_ms) {
            push_audio(sink, &tx.encode_heartbeat(), stop, &drops);
            last_heartbeat = Instant::now();
        }

        let d = drops.load(Ordering::Relaxed);
        if d > 0 && d.is_multiple_of(1000) {
            ev.log(format!("Audio buffer drops: {}", d));
        }

        if last_status.elapsed() >= STATUS_INTERVAL {
            let queued = pending.lock().map(|q| q.len()).unwrap_or(0);
            let latency_ms = sink.buffered() as f32 * 1000.0 / stream_sample_rate.max(1) as f32;
            ev.send(EngineEvent::TxStatus { queued, latency_ms });
            last_status = Instant::now();
        }

        thread::sleep(TICK);
    }

    ev.log("Encoder stopped.");
}

/// Queue `samples`, waiting up to ~2 s per sample while the sink is full
/// (then counting a drop). Returns early when `stop` is set.
fn push_audio(sink: &mut dyn AudioSink, samples: &[f32], stop: &AtomicBool, drops: &AtomicU64) {
    for &sample in samples {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut attempts = 0;
        while sink.is_full() {
            if stop.load(Ordering::Relaxed) || attempts > 2000 {
                drops.fetch_add(1, Ordering::Relaxed);
                break;
            }
            thread::sleep(Duration::from_millis(1));
            attempts += 1;
        }
        if sink.push(std::slice::from_ref(&sample)) == 0 {
            drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}
