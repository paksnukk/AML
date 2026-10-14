use cricket_core::modem::{
    decode_samples, encode_to_samples, CricketModemRx, CricketModemTx, ModemOutput,
};
use cricket_core::phy::PhyConfig;
use cricket_core::transport::{MscCommand, TransportMessage};
#[cfg(feature = "std")]
use cricket_core::wav::{read_wav_f32, write_wav_f32};

#[test]
fn loopback_note_on_off() {
    let config = PhyConfig::robust();
    let messages = vec![
        TransportMessage::NoteOn {
            note: 60,
            velocity: 100,
        },
        TransportMessage::NoteOff { note: 60 },
    ];
    let samples = encode_to_samples(config.clone(), &messages);
    let decoded = decode_samples(config, &samples);
    assert_eq!(decoded, messages);
}

#[test]
fn loopback_program_and_msc() {
    let config = PhyConfig::robust();
    let messages = vec![
        TransportMessage::ProgramChange { program: 42 },
        TransportMessage::MscLite {
            cmd: MscCommand::Go,
            cuelist: 3,
            cue: 12,
        },
        TransportMessage::Heartbeat,
    ];
    let samples = encode_to_samples(config.clone(), &messages);
    let decoded = decode_samples(config, &samples);
    assert_eq!(decoded, messages);
}

#[cfg(feature = "std")]
#[test]
fn loopback_via_wav_file() {
    let config = PhyConfig::robust();
    let messages = vec![TransportMessage::NoteOn {
        note: 64,
        velocity: 80,
    }];
    let samples = encode_to_samples(config.clone(), &messages);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("golden.wav");
    write_wav_f32(&path, config.sample_rate_hz, &samples).unwrap();

    let (rate, loaded) = read_wav_f32(&path).unwrap();
    assert_eq!(rate, config.sample_rate_hz);
    let decoded = decode_samples(config, &loaded);
    assert_eq!(decoded, messages);
}

#[test]
fn duplicate_repetition_suppressed() {
    let config = PhyConfig::robust();
    let messages = vec![TransportMessage::NoteOn {
        note: 72,
        velocity: 64,
    }];
    let samples = encode_to_samples(config.clone(), &messages);
    let decoded = decode_samples(config, &samples);
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0], messages[0]);
}

#[test]
fn streaming_rx_emits_stats() {
    let config = PhyConfig::robust();
    let mut tx = CricketModemTx::new(config.clone());
    let audio = tx.encode_message(&TransportMessage::Panic);
    let mut rx = CricketModemRx::new(config);
    let mut saw_stats = false;
    let mut saw_panic = false;
    for chunk in audio.chunks(512) {
        for out in rx.push_samples(chunk) {
            match out {
                ModemOutput::Stats(_) => saw_stats = true,
                ModemOutput::Message(TransportMessage::Panic) => saw_panic = true,
                _ => {}
            }
        }
    }
    assert!(saw_stats);
    assert!(saw_panic);
}

#[test]
fn level_offset_still_decodes() {
    let config = PhyConfig::robust();
    let messages = vec![TransportMessage::NoteOn {
        note: 55,
        velocity: 90,
    }];
    let mut samples = encode_to_samples(config.clone(), &messages);
    for s in samples.iter_mut() {
        *s *= 0.25;
    }
    let decoded = decode_samples(config, &samples);
    assert_eq!(decoded, messages);
}

#[test]
fn fast_profile_latency_budget() {
    let fast = PhyConfig::fast();
    assert!(
        fast.estimated_first_copy_ms() <= 60.0,
        "fast first-copy {} ms exceeds 60 ms",
        fast.estimated_first_copy_ms()
    );
    let samples = encode_to_samples(
        fast.clone(),
        &[TransportMessage::NoteOn {
            note: 60,
            velocity: 90,
        }],
    );
    let audio_ms = samples.len() as f32 * 1000.0 / fast.sample_rate_hz as f32;
    assert!(
        audio_ms <= 70.0,
        "fast encode audio {} ms too long",
        audio_ms
    );
    let decoded = decode_samples(fast, &samples);
    assert_eq!(
        decoded,
        vec![TransportMessage::NoteOn {
            note: 60,
            velocity: 90
        }]
    );
}
