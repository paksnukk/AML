//! TX worker: MIDI in / injected messages → `AmlModemTx` → audio out.

use crate::audio::{AudioOut, AudioSink};
use crate::events::{AlarmKind, EngineEvent};
use crate::midi::{self, MidiCallback, MidiIn};
use crate::{closed_receiver, Emitter, Worker};
use aml_core::modem::{repetition_samples, AmlModemTx};
use aml_core::phy::PhyConfig;
use aml_core::transport::{midi_to_message, TransportMessage, TxScheduler};
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
/// Buffered audio below which the next message is encoded. Above `TICK`, so
/// back-to-back messages play without a gap.
const REFILL_MS: u64 = 10;
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
                ev.log(format!("MIDI in {} connected.", sel));
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

    let ring_floor = (preferred_sample_rate as u64 * 5 / 1000).max(1) as usize;
    let est_msg = repetition_samples(&config) * config.repetition_k.max(1) as u64;
    let ring_samples = (est_msg * 2).max(ring_floor as u64) as usize;

    let drops = AtomicU64::new(0);
    let mut sink = match cfg.audio_out.open(preferred_sample_rate, ring_samples) {
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
    let message_samples = repetition_samples(&config) * config.repetition_k.max(1) as u64;
    let heartbeat_samples =
        config.sample_rate_hz as u64 * config.heartbeat_interval_ms as u64 / 1000;
    let refill_samples = config.sample_rate_hz as u64 * REFILL_MS / 1000;

    if let Err(e) = sink.start() {
        ev.log(format!("Failed to play output: {}", e));
        return;
    }

    let mut tx = AmlModemTx::new(config.clone());
    let mut scheduler = TxScheduler::new(stream_sample_rate, heartbeat_samples, message_samples);
    let stream_start = Instant::now();
    let mut note_idx = 0usize;
    let scale = [0u8, 2, 4, 5, 7, 9, 11, 12];
    let mut last_sim = Instant::now() - SIM_INTERVAL;
    let mut sim_sounding: Option<u8> = None;
    let mut last_status = Instant::now();
    let sink = sink.as_mut();

    while !stop.load(Ordering::SeqCst) {
        let tx_time = (stream_start.elapsed().as_secs_f64() * f64::from(stream_sample_rate)) as u64;

        let incoming: Vec<TransportMessage> = pending
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for msg in incoming {
            scheduler.push(msg, tx_time);
        }

        if scheduler.note_on_dropped {
            ev.log("TX dropped a queued NoteOn (queue airtime budget exceeded)");
            scheduler.note_on_dropped = false;
        }

        if use_sim && last_sim.elapsed() >= SIM_INTERVAL {
            if let Some(prev) = sim_sounding.take() {
                scheduler.push(TransportMessage::NoteOff { note: prev }, tx_time);
            }
            let note = 60 + scale[note_idx % scale.len()];
            scheduler.push(
                TransportMessage::NoteOn {
                    note,
                    velocity: 100,
                },
                tx_time,
            );
            sim_sounding = Some(note);
            ev.log(format!("TX sim Note {}", note));
            note_idx += 1;
            last_sim = Instant::now();
        }

        // Choose the next message only when the current one has nearly
        // played out, so a note that arrives meanwhile does not wait behind an
        // already encoded refresh or heartbeat.
        let buffered = sink.buffered() as u64;
        if buffered < refill_samples {
            if let Some(result) = scheduler.poll_detailed(tx_time + buffered) {
                if result.user {
                    ev.log(format!("TX {:?}", result.msg));
                }
                let audio = if matches!(result.msg, TransportMessage::Heartbeat) {
                    tx.encode_heartbeat()
                } else {
                    tx.encode_message(&result.msg)
                };
                let _ = push_audio(sink, &audio, stop, &drops);
            }
        }

        while let Some(err) = sink.take_error() {
            ev.send(EngineEvent::Alarm {
                kind: AlarmKind::AudioDeviceError,
                message: format!("Audio output device error: {}", err),
            });
            ev.log(format!("Audio output device error: {}", err));
        }

        let d = drops.load(Ordering::Relaxed);
        if d > 0 && d.is_multiple_of(1000) {
            ev.log(format!("Audio buffer drops: {}", d));
        }

        if last_status.elapsed() >= STATUS_INTERVAL {
            // The oldest queued message's wait so far plus the audio already
            // buffered ahead of it.
            let queued = scheduler.queued();
            let late = scheduler.oldest_wait_samples(tx_time) + sink.buffered() as u64;
            let latency_ms = late as f32 * 1000.0 / stream_sample_rate.max(1) as f32;
            ev.send(EngineEvent::TxStatus { queued, latency_ms });
            last_status = Instant::now();
        }

        thread::sleep(TICK);
    }

    ev.log("Encoder stopped.");
}

/// Queue `samples`, waiting up to ~2 s per sample while the sink is full
/// (then counting a drop). Returns the number of samples actually queued.
fn push_audio(
    sink: &mut dyn AudioSink,
    samples: &[f32],
    stop: &AtomicBool,
    drops: &AtomicU64,
) -> usize {
    let mut pushed = 0usize;
    for &sample in samples {
        if stop.load(Ordering::Relaxed) {
            break;
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
        } else {
            pushed += 1;
        }
    }
    pushed
}
