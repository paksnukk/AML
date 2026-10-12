//! MIDI-in → audio → MIDI-out latency measurement (hardware-free, paced).
//!
//! Built on [`crate::testing`] endpoints; enabled with the `testing` feature.

use crate::testing::{MemoryAudio, MemoryMidiInput, RecordingMidiSink};
use crate::{
    AudioIn, AudioOut, DecoderConfig, DecoderHandle, EncoderConfig, EncoderHandle, MidiIn, MidiOut,
};
use aml_core::modem::repetition_samples;
use aml_core::phy::PhyConfig;
use anyhow::{anyhow, Result};
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Summary of one loopback run (milliseconds).
#[derive(Debug, Clone, PartialEq)]
pub struct LatencySummary {
    pub n: usize,
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub jitter_ms: f64,
}

/// Arithmetic mean; empty slice returns 0.
pub fn mean_ms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().sum::<f64>() / samples.len() as f64
}

/// Population standard deviation (jitter); empty or single sample → 0.
pub fn jitter_ms(samples: &[f64]) -> f64 {
    let n = samples.len();
    if n < 2 {
        return 0.0;
    }
    let m = mean_ms(samples);
    let var = samples.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n as f64;
    var.sqrt()
}

/// Nearest-rank percentile (e.g. p = 95 → 95th).
pub fn percentile_nearest_rank_ms(mut samples: Vec<f64>, p: f64) -> f64 {
    assert!((0.0..=100.0).contains(&p));
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = samples.len();
    let rank = (p / 100.0 * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    samples[idx]
}

fn summary_from_latencies(latencies: &[f64]) -> LatencySummary {
    LatencySummary {
        n: latencies.len(),
        mean_ms: mean_ms(latencies),
        p95_ms: percentile_nearest_rank_ms(latencies.to_vec(), 95.0),
        jitter_ms: jitter_ms(latencies),
    }
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

/// Measure wall-clock MIDI-in → MIDI-out latency for `profile` (`"fast"` /
/// `"robust"`), injecting `note_count` NoteOn messages (no NoteOff).
pub fn measure_midi_loopback(profile: &str, note_count: usize) -> Result<LatencySummary> {
    if note_count == 0 {
        return Err(anyhow!("note_count must be at least 1"));
    }

    let config = PhyConfig::from_name(profile);
    let packet_samples = repetition_samples(&config) * config.repetition_k.max(1) as u64;
    let packet_duration =
        Duration::from_secs_f64(packet_samples as f64 / config.sample_rate_hz as f64);
    let sink_capacity = (packet_samples * 2).max(512) as usize;

    let (sink, source) = MemoryAudio::pair(RATE, sink_capacity);
    let paced = source.into_paced();
    let rec = RecordingMidiSink::new();
    let mut dec = DecoderHandle::start(DecoderConfig {
        audio_in: AudioIn::Custom(Box::new(paced)),
        midi_out: MidiOut::Custom(Box::new(rec.clone())),
        profile: profile.into(),
        link_dead_ms: None,
    })?;

    let (midi_in, injector) = MemoryMidiInput::new();
    let mut enc = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Custom(Box::new(sink)),
        midi_in: MidiIn::Custom(Box::new(midi_in)),
        profile: profile.into(),
        simulate: false,
    })?;

    if !wait_until(Duration::from_secs(5), || injector.inject(&[0xF6])) {
        let _ = enc.stop();
        let _ = dec.stop();
        return Err(anyhow!("MIDI input did not connect"));
    }

    // Let the link settle (encoder may send heartbeats while idle).
    std::thread::sleep(Duration::from_millis(300));

    let mut latencies = Vec::with_capacity(note_count);
    for i in 0..note_count {
        let note = 48 + (i % 40) as u8;
        let velocity = 90u8;
        let expected = vec![0x90, note, velocity];
        let before = rec.messages().len();

        let t0 = Instant::now();
        if !injector.inject(&expected) {
            let _ = enc.stop();
            let _ = dec.stop();
            return Err(anyhow!("MIDI inject failed on note {}", note));
        }

        let delivered = wait_until(DELIVERY_TIMEOUT, || {
            rec.messages()[before..]
                .iter()
                .any(|m| m.as_slice() == expected.as_slice())
        });
        if !delivered {
            let _ = enc.stop();
            let _ = dec.stop();
            return Err(anyhow!(
                "NoteOn note={} not delivered within {:.0} s",
                note,
                DELIVERY_TIMEOUT.as_secs_f64()
            ));
        }
        latencies.push(t0.elapsed().as_secs_f64() * 1000.0);

        if i + 1 < note_count {
            std::thread::sleep(packet_duration);
        }
    }

    assert!(enc.stop());
    assert!(dec.stop());

    if latencies.len() < note_count {
        return Err(anyhow!(
            "only {} of {} NoteOns delivered",
            latencies.len(),
            note_count
        ));
    }

    Ok(summary_from_latencies(&latencies))
}
