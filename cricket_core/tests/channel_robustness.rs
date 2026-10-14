//! Channel-robustness tests: sync across start offsets / feed chunk sizes and
//! decoding under modest noise. Uses the simulator in `tests/support`.

mod support;

use cricket_core::modem::encode_to_samples;
use cricket_core::phy::PhyConfig;
use cricket_core::transport::TransportMessage;
use support::{decode_chunked, ChannelSim, XorShift64};

const CHUNK_SIZES: [usize; 5] = [64, 128, 240, 512, 4096];

fn profiles() -> Vec<(&'static str, PhyConfig)> {
    vec![("robust", PhyConfig::robust()), ("fast", PhyConfig::fast())]
}

fn note_on_for(i: usize) -> TransportMessage {
    TransportMessage::NoteOn {
        note: (i % 128) as u8,
        velocity: (1 + (i * 7) % 127) as u8,
    }
}

/// Offset step for the single-message sweep. Every offset is covered for the
/// fast profile at chunk sizes 64 and 240 (the historically failing cases);
/// other combinations use every second offset to keep debug runtime low.
fn single_sweep_step(profile: &str, chunk: usize) -> usize {
    match (profile, chunk) {
        ("fast", 64) | ("fast", 240) => 1,
        _ => 2,
    }
}

/// Runs one sweep case; returns a failure description if the decode differs.
fn run_case(
    config: &PhyConfig,
    messages: &[TransportMessage],
    offset: usize,
    chunk: usize,
) -> Option<String> {
    let clean = encode_to_samples(config.clone(), messages);
    let rx_audio = ChannelSim::new(config.clone(), 0)
        .offset(offset)
        .apply(&clean);
    let decoded = decode_chunked(config.clone(), &rx_audio, chunk);
    if decoded == messages {
        None
    } else {
        Some(format!(
            "offset {offset} chunk {chunk}: sent {messages:?} got {decoded:?}"
        ))
    }
}

/// Per-(profile, chunk) tally: (label, failed, total).
type Tally = Vec<(String, usize, usize)>;

fn report(name: &str, tally: &Tally, failures: &[String]) {
    let total: usize = tally.iter().map(|t| t.2).sum();
    let breakdown = tally
        .iter()
        .map(|(label, failed, n)| format!("{label}: {failed}/{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        failures.is_empty(),
        "{name}: {}/{} cases failed [{breakdown}]; first failures:\n{}",
        failures.len(),
        total,
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn sync_offset_chunk_sweep() {
    let mut tally = Tally::new();
    let mut failures = Vec::new();
    for (name, config) in profiles() {
        let period = config.samples_per_symbol_period();
        for &chunk in &CHUNK_SIZES {
            let step = single_sweep_step(name, chunk);
            let before = failures.len();
            let mut n = 0usize;
            for offset in (0..2 * period).step_by(step) {
                n += 1;
                let messages = [note_on_for(offset)];
                if let Some(f) = run_case(&config, &messages, offset, chunk) {
                    failures.push(format!("[{name}] {f}"));
                }
            }
            tally.push((format!("{name}/{chunk}"), failures.len() - before, n));
        }
    }
    report("sync_offset_chunk_sweep", &tally, &failures);
}

#[test]
fn sync_offset_two_messages() {
    let mut tally = Tally::new();
    let mut failures = Vec::new();
    for (name, config) in profiles() {
        let period = config.samples_per_symbol_period();
        for &chunk in &CHUNK_SIZES {
            let before = failures.len();
            let mut n = 0usize;
            for offset in (0..2 * period).step_by(7) {
                n += 1;
                let note = (offset % 128) as u8;
                let messages = [
                    TransportMessage::NoteOn {
                        note,
                        velocity: (1 + offset % 127) as u8,
                    },
                    TransportMessage::NoteOff { note },
                ];
                if let Some(f) = run_case(&config, &messages, offset, chunk) {
                    failures.push(format!("[{name}] {f}"));
                }
            }
            tally.push((format!("{name}/{chunk}"), failures.len() - before, n));
        }
    }
    report("sync_offset_two_messages", &tally, &failures);
}

#[test]
fn modest_noise_snr20_decodes() {
    const TRIALS: usize = 200;
    for (name, config) in profiles() {
        let period = config.samples_per_symbol_period();
        let mut rng = XorShift64::new(0xA41_2026);
        let mut ok = 0usize;
        let mut failures = Vec::new();
        for trial in 0..TRIALS {
            let offset = rng.below(4 * period);
            let chunk = CHUNK_SIZES[rng.below(CHUNK_SIZES.len())];
            let messages = [note_on_for(trial)];
            let clean = encode_to_samples(config.clone(), &messages);
            let rx_audio = ChannelSim::new(config.clone(), rng.next_u64())
                .offset(offset)
                .snr_db(20.0)
                .apply(&clean);
            let decoded = decode_chunked(config.clone(), &rx_audio, chunk);
            if decoded == messages {
                ok += 1;
            } else {
                failures.push(format!(
                    "trial {trial} offset {offset} chunk {chunk}: got {decoded:?}"
                ));
            }
        }
        assert!(
            ok * 100 >= TRIALS * 99,
            "[{name}] SNR 20 dB: {ok}/{TRIALS} exact decodes (< 99%); failures:\n{}",
            failures.join("\n")
        );
    }
}

#[test]
fn channel_sim_knobs_behave() {
    let config = PhyConfig::robust();
    let messages = [note_on_for(42)];
    let clean = encode_to_samples(config.clone(), &messages);

    // Deterministic for a fixed seed, different for another seed.
    let noisy = |seed| {
        ChannelSim::new(config.clone(), seed)
            .snr_db(10.0)
            .apply(&clean)
    };
    assert_eq!(noisy(1), noisy(1));
    assert_ne!(noisy(1), noisy(2));

    // Offset + gain.
    let shifted = ChannelSim::new(config.clone(), 0)
        .offset(100)
        .gain(0.5)
        .apply(&clean);
    assert_eq!(shifted.len(), clean.len() + 100);
    assert!(shifted[..100].iter().all(|&s| s == 0.0));
    assert!((shifted[150] - 0.5 * clean[50]).abs() < 1e-6);

    // +1000 ppm drift shortens the signal by ~0.1 %.
    let drifted = ChannelSim::new(config.clone(), 0)
        .drift_ppm(1000.0)
        .apply(&clean);
    let expected = clean.len() as f64 / 1.001;
    assert!((drifted.len() as f64 - expected).abs() <= 2.0);

    // Low-pass well below the tone band attenuates the signal strongly.
    let rms = |x: &[f32]| (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
    let filtered = ChannelSim::new(config.clone(), 0)
        .lowpass_hz(200.0)
        .apply(&clean);
    assert!(rms(&filtered) < 0.2 * rms(&clean));

    // Corrupting every symbol must break the decode; p = 0 must not.
    let corrupted = ChannelSim::new(config.clone(), 7)
        .symbol_error_prob(1.0)
        .apply(&clean);
    assert_ne!(corrupted, clean);
    assert_ne!(decode_chunked(config.clone(), &corrupted, 512), messages);
    let untouched = ChannelSim::new(config.clone(), 7)
        .symbol_error_prob(0.0)
        .apply(&clean);
    assert_eq!(decode_chunked(config, &untouched, 512), messages);
}
