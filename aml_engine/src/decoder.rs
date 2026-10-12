//! RX worker: audio in → `AmlModemRx` → MIDI out + events.

use crate::audio::AudioIn;
use crate::events::{AlarmKind, EngineEvent, LinkStats};
use crate::midi::{self, MidiOut, MidiSink};
use crate::{closed_receiver, Emitter, Worker};
use aml_core::modem::{AmlModemRx, ModemOutput};
use aml_core::phy::{LinkState, PhyConfig};
use aml_core::transport::TransportMessage;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, Instant};

/// Decoder settings.
pub struct DecoderConfig {
    pub audio_in: AudioIn,
    pub midi_out: MidiOut,
    /// PHY profile name ("robust" / "fast"; unknown names mean robust).
    pub profile: String,
}

/// A running decoder worker. Dropping it stops the worker.
pub struct DecoderHandle {
    worker: Worker,
    events: Receiver<EngineEvent>,
}

impl DecoderHandle {
    /// Spawn the decoder thread. Device errors (audio or MIDI) do not fail
    /// this call; they are reported as `Log` events, after which the worker
    /// either continues without MIDI or ends with `Stopped`.
    pub fn start(config: DecoderConfig) -> Result<Self> {
        let (worker, events) = Worker::spawn("aml-decoder", move |ev, stop| run(config, ev, stop))?;
        Ok(Self { worker, events })
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

impl Drop for DecoderHandle {
    fn drop(&mut self) {
        self.worker.stop();
    }
}

/// Samples per pull from the source.
const CHUNK: usize = 512;
/// Sleep when no input is buffered.
const IDLE_SLEEP: Duration = Duration::from_millis(5);
/// Minimum interval between `Stats` events.
const STATS_INTERVAL: Duration = Duration::from_millis(200);

fn run(cfg: DecoderConfig, ev: &Emitter, stop: &AtomicBool) {
    ev.log(format!("Starting decoder (profile={})...", cfg.profile));

    let mut config = PhyConfig::from_name(&cfg.profile);
    let est_latency = config.estimated_first_copy_ms();
    let preferred_sample_rate = config.sample_rate_hz;

    let mut midi_out: Option<Box<dyn MidiSink>> = match cfg.midi_out {
        MidiOut::None => {
            ev.log("No MIDI out selected — decode to UI only.");
            None
        }
        MidiOut::Device(sel) => match midi::open_output(&sel) {
            Ok(c) => Some(Box::new(c)),
            Err(e) => {
                ev.log(format!("MIDI out error: {}", e));
                None
            }
        },
        MidiOut::Custom(sink) => Some(sink),
    };

    let mut source = match cfg
        .audio_in
        .open(preferred_sample_rate, preferred_sample_rate as usize * 2)
    {
        Ok(s) => s,
        Err(e) => {
            ev.log(format!("Audio input error: {}", e));
            return;
        }
    };
    let stream_sample_rate = source.sample_rate();

    ev.log(format!(
        "Audio in {} Hz, {} ch (ch0). est latency {:.0} ms.",
        stream_sample_rate,
        source.channels(),
        est_latency
    ));
    if stream_sample_rate != preferred_sample_rate {
        ev.log(format!(
            "Sample rate adapted {} → {} Hz",
            preferred_sample_rate, stream_sample_rate
        ));
        config = config.with_sample_rate(stream_sample_rate);
    }

    if let Err(e) = source.start() {
        ev.log(format!("Failed to play input: {}", e));
        return;
    }

    let mut rx = AmlModemRx::new(config);
    let mut rms_accumulator = 0.0f32;
    let mut sample_count_for_meter = 0usize;
    let meter_report_interval = (stream_sample_rate as usize) / 20;
    let mut last_stats_emit = Instant::now();
    let mut sync_lost: u64 = 0;
    let mut link_alive = false;
    let mut buf = [0.0f32; CHUNK];

    while !stop.load(Ordering::SeqCst) {
        let n = source.pull(&mut buf);
        let chunk = &buf[..n];
        for &sample in chunk {
            rms_accumulator += sample * sample;
            sample_count_for_meter += 1;
            if sample_count_for_meter >= meter_report_interval {
                let rms = (rms_accumulator / sample_count_for_meter as f32).sqrt();
                ev.send(EngineEvent::Level(rms));
                rms_accumulator = 0.0;
                sample_count_for_meter = 0;
            }
        }

        if chunk.is_empty() {
            thread::sleep(IDLE_SLEEP);
            continue;
        }

        for out in rx.push_samples(chunk) {
            match out {
                ModemOutput::Message(msg) => {
                    if matches!(msg, TransportMessage::Heartbeat) {
                        link_alive = true;
                    }
                    ev.send(EngineEvent::Decoded(msg));
                }
                ModemOutput::Midi(bytes) => {
                    if let Some(sink) = midi_out.as_mut() {
                        let _ = sink.send(&bytes);
                    }
                }
                ModemOutput::LinkDead => {
                    link_alive = false;
                    ev.send(EngineEvent::Alarm {
                        kind: AlarmKind::LinkDead,
                        message: "Link dead — All Notes Off sent".into(),
                    });
                    ev.log("Link dead — All Notes Off");
                }
                ModemOutput::SyncLost => {
                    sync_lost = sync_lost.saturating_add(1);
                }
                ModemOutput::Stats(stats) => {
                    if last_stats_emit.elapsed() >= STATS_INTERVAL {
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
                        ev.send(EngineEvent::Stats(LinkStats {
                            state: stats.link_state,
                            crc_pass: stats.crc_pass,
                            crc_fail: stats.crc_fail,
                            confidence_db: stats.last_confidence_db,
                            last_seq: stats.last_seq,
                            sync_lost,
                            crc_pass_rate: rate,
                            est_latency_ms: est_latency,
                            link_alive,
                            buffer_drops: source.dropped(),
                        }));
                        last_stats_emit = Instant::now();
                    }
                }
            }
        }
    }

    ev.log("Decoder stopped.");
}
