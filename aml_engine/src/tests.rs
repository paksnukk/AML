//! Engine-level tests on in-memory endpoints (no audio/MIDI hardware).

use crate::testing::{FailingAudio, MemoryAudio, MemoryMidiInput, RecordingMidiSink};
use crate::*;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;

fn wait_for<F: FnMut() -> bool>(timeout: Duration, mut cond: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// Drain events until `Stopped`; then the channel must disconnect (the
/// worker closure has returned and dropped its sender).
fn expect_stopped_then_closed(events: &Receiver<EngineEvent>) -> Vec<EngineEvent> {
    let mut seen = Vec::new();
    loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(EngineEvent::Stopped) => break,
            Ok(ev) => seen.push(ev),
            Err(e) => panic!("no Stopped event ({:?}); saw {:?}", e, seen),
        }
    }
    assert_eq!(
        events.recv_timeout(Duration::from_secs(5)),
        Err(RecvTimeoutError::Disconnected),
        "events after Stopped or worker still alive"
    );
    seen
}

fn log_lines(events: &[EngineEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Log(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn memory_decoder(source: testing::MemorySource, rec: &RecordingMidiSink) -> DecoderHandle {
    DecoderHandle::start(DecoderConfig {
        audio_in: AudioIn::Custom(Box::new(source)),
        midi_out: MidiOut::Custom(Box::new(rec.clone())),
        profile: "fast".into(),
    })
    .unwrap()
}

/// MIDI in → encoder → samples → decoder → MIDI out, all through the engine.
#[test]
fn loopback_midi_in_to_midi_out() {
    let (sink, source) = MemoryAudio::pair(RATE, RATE as usize * 4);
    let rec = RecordingMidiSink::new();
    let mut dec = memory_decoder(source, &rec);

    let (midi_in, injector) = MemoryMidiInput::new();
    let mut enc = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Custom(Box::new(sink)),
        midi_in: MidiIn::Custom(Box::new(midi_in)),
        profile: "fast".into(),
        simulate: false,
    })
    .unwrap();

    // The encoder connects its MIDI input on its own thread.
    assert!(wait_for(Duration::from_secs(5), || injector.inject(&[0x90, 64, 100])));
    injector.inject(&[0x80, 64, 0]);
    enc.send(TransportMessage::ProgramChange { program: 5 });

    let note_on = vec![0x90, 64, 100];
    let note_off = vec![0x80, 64, 0x40];
    let program = vec![0xC0, 5];
    let got_all = wait_for(Duration::from_secs(15), || {
        let m = rec.messages();
        m.contains(&note_on) && m.contains(&note_off) && m.contains(&program)
    });
    let midi = rec.messages();
    assert!(got_all, "MIDI out: {:?}", midi);
    let pos = |m: &Vec<u8>| midi.iter().position(|x| x == m).unwrap();
    assert!(pos(&note_on) < pos(&note_off), "order: {:?}", midi);
    assert!(pos(&note_off) < pos(&program), "order: {:?}", midi);

    assert!(enc.stop());
    assert!(dec.stop());

    let dec_events: Vec<EngineEvent> = dec.events().try_iter().collect();
    assert!(
        dec_events.contains(&EngineEvent::Decoded(TransportMessage::NoteOn {
            note: 64,
            velocity: 100
        }))
    );
    assert!(dec_events
        .iter()
        .any(|e| matches!(e, EngineEvent::Level(_))));
    assert_eq!(dec_events.last(), Some(&EngineEvent::Stopped));

    let enc_logs = log_lines(&enc.events().try_iter().collect::<Vec<_>>());
    assert!(
        enc_logs
            .iter()
            .any(|l| l == "TX NoteOn { note: 64, velocity: 100 }"),
        "{:?}",
        enc_logs
    );
    assert!(
        enc_logs.iter().any(|l| l == "Encoder stopped."),
        "{:?}",
        enc_logs
    );
}

/// Without MIDI in the encoder plays its simulated scale (first note: 60).
#[test]
fn simulated_scale_reaches_decoder() {
    let (sink, source) = MemoryAudio::pair(RATE, RATE as usize * 4);
    let rec = RecordingMidiSink::new();
    let _dec = memory_decoder(source, &rec);
    let _enc = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Custom(Box::new(sink)),
        midi_in: MidiIn::None,
        profile: "fast".into(),
        simulate: false,
    })
    .unwrap();
    let ok = wait_for(Duration::from_secs(15), || {
        let m = rec.messages();
        m.contains(&vec![0x90, 60, 100]) && m.contains(&vec![0x80, 60, 0x40])
    });
    assert!(ok, "MIDI out: {:?}", rec.messages());
    // Handles dropped here: Drop must stop and join both workers.
}

#[test]
fn stop_joins_worker_and_ends_with_stopped() {
    let (sink, source) = MemoryAudio::pair(RATE, RATE as usize);
    let rec = RecordingMidiSink::new();
    let mut dec = memory_decoder(source, &rec);
    let mut enc = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Custom(Box::new(sink)),
        midi_in: MidiIn::None,
        profile: "fast".into(),
        simulate: true,
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(dec.is_running() && enc.is_running());

    assert!(enc.stop(), "encoder did not stop in time");
    assert!(dec.stop(), "decoder did not stop in time");
    assert!(!dec.is_running() && !enc.is_running());
    // Stopping again is a no-op.
    assert!(dec.stop() && enc.stop());

    let enc_logs = log_lines(&expect_stopped_then_closed(enc.events()));
    assert_eq!(
        enc_logs.last().map(String::as_str),
        Some("Encoder stopped.")
    );
    let dec_logs = log_lines(&expect_stopped_then_closed(dec.events()));
    assert_eq!(
        dec_logs.first().map(String::as_str),
        Some("Starting decoder (profile=fast)...")
    );
    assert_eq!(
        dec_logs.last().map(String::as_str),
        Some("Decoder stopped.")
    );
}

#[test]
fn drop_stops_worker() {
    let (_sink, source) = MemoryAudio::pair(RATE, RATE as usize);
    let rec = RecordingMidiSink::new();
    let mut dec = memory_decoder(source, &rec);
    let events = dec.take_events();
    // The handle's own channel is now closed.
    assert!(matches!(
        dec.events().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected)
    ));
    drop(dec);
    let logs = log_lines(&expect_stopped_then_closed(&events));
    assert_eq!(logs.last().map(String::as_str), Some("Decoder stopped."));
}

#[test]
fn audio_open_error_is_logged_and_worker_ends() {
    let dec = DecoderHandle::start(DecoderConfig {
        audio_in: AudioIn::Custom(Box::new(FailingAudio("boom".into()))),
        midi_out: MidiOut::None,
        profile: "robust".into(),
    })
    .unwrap();
    let logs = log_lines(&expect_stopped_then_closed(dec.events()));
    assert_eq!(
        logs,
        vec![
            "Starting decoder (profile=robust)...".to_string(),
            "No MIDI out selected — decode to UI only.".to_string(),
            "Audio input error: boom".to_string(),
        ]
    );
    assert!(wait_for(Duration::from_secs(1), || !dec.is_running()));

    let enc = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Custom(Box::new(FailingAudio("bang".into()))),
        midi_in: MidiIn::None,
        profile: "robust".into(),
        simulate: false,
    })
    .unwrap();
    let logs = log_lines(&expect_stopped_then_closed(enc.events()));
    assert_eq!(
        logs.last().map(String::as_str),
        Some("Audio output error: bang")
    );
}
