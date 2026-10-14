//! Experimental FEC profile (`PhyConfig::robust_fec`, docs/proposals/fec.md):
//! loopback, sync sweep, and an A/B comparison against K = 2 repetition on
//! the channel simulator.
//!
//! The full A/B table is an ignored test (it takes a while in debug builds):
//!
//! ```sh
//! cargo test --release -p cricket_core --test fec -- --ignored --nocapture
//! ```

mod support;

use cricket_core::link::FecMode;
use cricket_core::modem::{
    encode_to_samples, repetition_samples, CricketModemRx, CricketModemTx, ModemOutput,
};
use cricket_core::phy::PhyConfig;
use cricket_core::transport::{MscCommand, TransportMessage};
use support::{decode_chunked, ChannelSim, XorShift64};

fn all_types() -> Vec<TransportMessage> {
    vec![
        TransportMessage::NoteOn {
            note: 60,
            velocity: 100,
        },
        TransportMessage::NoteOff { note: 60 },
        TransportMessage::ProgramChange { program: 42 },
        TransportMessage::MscLite {
            cmd: MscCommand::Go,
            cuelist: 15,
            cue: 127,
        },
        TransportMessage::Heartbeat,
        TransportMessage::Panic,
    ]
}

fn fec_parity(parity: u8, k: usize) -> PhyConfig {
    PhyConfig {
        repetition_k: k,
        fec: FecMode::Rs16 { parity },
        ..PhyConfig::robust()
    }
}

#[test]
fn defaults_are_unchanged() {
    assert_eq!(PhyConfig::robust().fec, FecMode::None);
    assert_eq!(PhyConfig::fast().fec, FecMode::None);
    assert_eq!(PhyConfig::default().fec, FecMode::None);
    assert_eq!(repetition_samples(&PhyConfig::robust()), 21 * 312);
    let fec = PhyConfig::robust_fec();
    assert_eq!(fec.repetition_k, 1);
    assert_eq!(fec.fec.body_nibbles(), 22);
    assert_eq!(repetition_samples(&fec), 27 * 312);
    assert!((fec.estimated_first_copy_ms() - 26.0 * 6.5).abs() < 1e-3);
    assert_eq!(PhyConfig::from_name("robust_fec").fec, fec.fec);
}

#[test]
fn loopback_all_message_types() {
    for config in [PhyConfig::robust_fec(), fec_parity(2, 1), fec_parity(4, 2)] {
        let messages = all_types();
        let samples = encode_to_samples(config.clone(), &messages);
        for chunk in [64, 240, 4096] {
            assert_eq!(decode_chunked(config.clone(), &samples, chunk), messages);
        }
    }
}

#[test]
fn sync_offset_sweep() {
    let config = PhyConfig::robust_fec();
    let period = config.samples_per_symbol_period();
    let mut failures = Vec::new();
    for offset in (0..2 * period).step_by(5) {
        let messages = [TransportMessage::NoteOn {
            note: (offset % 128) as u8,
            velocity: 1 + (offset % 127) as u8,
        }];
        let clean = encode_to_samples(config.clone(), &messages);
        let rx = ChannelSim::new(config.clone(), 0)
            .offset(offset)
            .apply(&clean);
        let chunk = [64, 128, 240, 512, 4096][offset % 5];
        if decode_chunked(config.clone(), &rx, chunk) != messages {
            failures.push(offset);
        }
    }
    assert!(failures.is_empty(), "failed offsets: {failures:?}");
}

/// Wrong-tone symbols are fixed by the code and counted in the stats.
#[test]
fn corrects_wrong_symbols_and_reports_them() {
    let config = PhyConfig::robust_fec();
    let msg = TransportMessage::MscLite {
        cmd: MscCommand::Load,
        cuelist: 3,
        cue: 99,
    };
    let tx = CricketModemTx::new(config.clone());
    let audio = tx.encode_message_with_seq(&msg, 9);
    // Overwrite body symbols 0..4 (2 per codeword) with a different tone.
    let period = config.samples_per_symbol_period();
    let pre = config.preamble.len();
    let phy = cricket_core::phy::ModemTx::new(config.clone());
    let mut audio = audio;
    for i in 0..4 {
        let at = (pre + i) * period;
        let wrong = phy.synthesize_nibbles(&[((i * 3 + 1) % 16) as u8]);
        audio[at..at + period].copy_from_slice(&wrong[..period]);
    }
    audio.extend(std::iter::repeat_n(0.0, 8 * period));
    let mut rx = CricketModemRx::new(config);
    let mut got = Vec::new();
    let mut corrected = 0;
    for out in rx.push_samples(&audio) {
        match out {
            ModemOutput::Message(m) => got.push(m),
            ModemOutput::Stats(s) => corrected = s.fec_corrected,
            _ => {}
        }
    }
    assert_eq!(got, vec![msg]);
    assert!(corrected >= 1, "fec_corrected = {corrected}");
}

/// Erasure-tolerant body + tolerant preamble must not turn noise or random
/// tone streams into messages (CRC-16 + version/type/pad checks).
#[test]
fn noise_and_random_tones_produce_no_messages() {
    let config = PhyConfig::robust_fec();
    let rate = config.sample_rate_hz as usize;
    let mut rng = XorShift64::new(0x0BAD);
    // 10 s of white noise.
    let noise: Vec<f32> = (0..10 * rate)
        .map(|_| (rng.gaussian() * 0.3) as f32)
        .collect();
    // 10 s of random MFSK symbols with preambles sprinkled in.
    let phy = cricket_core::phy::ModemTx::new(config.clone());
    let mut nibbles = Vec::new();
    while nibbles.len() * config.samples_per_symbol_period() < 10 * rate {
        if rng.below(8) == 0 {
            nibbles.extend_from_slice(&config.preamble);
        }
        nibbles.push(rng.below(16) as u8);
    }
    let tones = phy.synthesize_nibbles(&nibbles);
    for audio in [noise, tones] {
        let got = decode_chunked(config.clone(), &audio, 512);
        assert!(got.is_empty(), "false messages: {got:?}");
    }
}

/// Same trials through both profiles: single-copy FEC must deliver at least
/// as often as two uncoded copies under random wrong-tone symbols.
#[test]
fn fec_k1_at_least_as_robust_as_repetition_k2() {
    let trials = 120;
    let robust = run_cell(&PhyConfig::robust(), Impairment::SymErr(0.04), trials, 77);
    let fec = run_cell(
        &PhyConfig::robust_fec(),
        Impairment::SymErr(0.04),
        trials,
        77,
    );
    assert!(
        fec.ok >= robust.ok,
        "robust K2 {}/{trials}, robust_fec {}/{trials}",
        robust.ok,
        fec.ok
    );
}

// ---------------------------------------------------------------------------
// A/B measurement

#[derive(Debug, Clone, Copy)]
enum Impairment {
    Clean,
    Snr(f32),
    SymErr(f64),
    /// Dropout length in ms, at SNR 20 dB.
    Dropout(f32),
    /// Gain in dB, at an SNR (dB).
    GainSnr(f32, f32),
    /// One-pole low-pass cutoff (Hz), at an SNR (dB).
    LowpassSnr(f32, f32),
    /// Clock drift (ppm), at an SNR (dB).
    DriftSnr(f64, f32),
}

impl Impairment {
    fn label(&self) -> String {
        match *self {
            Impairment::Clean => "clean".into(),
            Impairment::Snr(s) => format!("AWGN SNR {s} dB"),
            Impairment::SymErr(p) => format!("wrong tone p={:.0}%", p * 100.0),
            Impairment::Dropout(ms) => format!("dropout {ms} ms"),
            Impairment::GainSnr(g, s) => format!("gain {g} dB, SNR {s}"),
            Impairment::LowpassSnr(f, s) => format!("LP {f} Hz, SNR {s}"),
            Impairment::DriftSnr(p, s) => format!("drift {p} ppm, SNR {s}"),
        }
    }

    fn sim(&self, config: &PhyConfig, seed: u64, offset: usize) -> ChannelSim {
        let base = ChannelSim::new(config.clone(), seed).offset(offset);
        let ms = |ms: f32| (ms * config.sample_rate_hz as f32 / 1000.0) as usize;
        match *self {
            Impairment::Clean => base,
            Impairment::Snr(s) => base.snr_db(s),
            Impairment::SymErr(p) => base.symbol_error_prob(p).snr_db(20.0),
            Impairment::Dropout(d) => base.dropout_samples(ms(d)).snr_db(20.0),
            Impairment::GainSnr(g, s) => base.gain(10f32.powf(g / 20.0)).snr_db(s),
            Impairment::LowpassSnr(f, s) => base.lowpass_hz(f).snr_db(s),
            Impairment::DriftSnr(p, s) => base.drift_ppm(p).snr_db(s),
        }
    }
}

struct Cell {
    ok: usize,
    trials: usize,
    /// Mean message start → delivery time over delivered trials (ms).
    mean_latency_ms: f64,
    false_msgs: usize,
}

/// `trials` single-message transmissions (random NoteOn / MscLite, random
/// start offset, 64-sample chunks so delivery time has ~1.3 ms resolution).
fn run_cell(config: &PhyConfig, imp: Impairment, trials: usize, seed: u64) -> Cell {
    let mut rng = XorShift64::new(seed);
    let rate = config.sample_rate_hz as f64;
    let mut ok = 0;
    let mut false_msgs = 0;
    let mut lat = Vec::new();
    for t in 0..trials {
        let msg = if t % 4 == 3 {
            TransportMessage::MscLite {
                cmd: MscCommand::Go,
                cuelist: rng.below(16) as u8,
                cue: rng.below(128) as u8,
            }
        } else {
            TransportMessage::NoteOn {
                note: rng.below(128) as u8,
                velocity: 1 + rng.below(127) as u8,
            }
        };
        let offset = 200 + rng.below(4 * config.samples_per_symbol_period());
        let tx = CricketModemTx::new(config.clone());
        let mut clean = tx.encode_message_with_seq(&msg, rng.below(256) as u8);
        clean.extend(std::iter::repeat_n(0.0, config.sample_rate_hz as usize / 4));
        let audio = imp.sim(config, rng.next_u64(), offset).apply(&clean);

        let mut rx = CricketModemRx::new(config.clone());
        let mut delivered_at = None;
        for chunk in audio.chunks(64) {
            for out in rx.push_samples(chunk) {
                if let ModemOutput::Message(m) = out {
                    if m == msg && delivered_at.is_none() {
                        delivered_at = Some(rx.samples_processed());
                    } else if m != msg {
                        false_msgs += 1;
                    }
                }
            }
        }
        if let Some(at) = delivered_at {
            ok += 1;
            lat.push((at as f64 - offset as f64) / rate * 1000.0);
        }
    }
    Cell {
        ok,
        trials,
        mean_latency_ms: if lat.is_empty() {
            f64::NAN
        } else {
            lat.iter().sum::<f64>() / lat.len() as f64
        },
        false_msgs,
    }
}

#[test]
#[ignore = "long-running A/B table; run with --release -- --ignored --nocapture"]
fn fec_ab_table() {
    let trials: usize = std::env::var("FEC_AB_TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    let profiles: Vec<(&str, PhyConfig)> = vec![
        ("robust K2", PhyConfig::robust()),
        (
            "robust K1",
            PhyConfig {
                repetition_k: 1,
                ..PhyConfig::robust()
            },
        ),
        ("fec p4 K1", PhyConfig::robust_fec()),
        ("fec p2 K1", fec_parity(2, 1)),
        ("fec p4 K2", fec_parity(4, 2)),
    ];
    let imps = [
        Impairment::Clean,
        Impairment::Snr(6.0),
        Impairment::Snr(0.0),
        Impairment::Snr(-2.0),
        Impairment::Snr(-3.0),
        Impairment::Snr(-4.0),
        Impairment::SymErr(0.01),
        Impairment::SymErr(0.02),
        Impairment::SymErr(0.05),
        Impairment::SymErr(0.10),
        Impairment::Dropout(10.0),
        Impairment::Dropout(30.0),
        Impairment::Dropout(60.0),
        Impairment::GainSnr(-40.0, 0.0),
        Impairment::LowpassSnr(2500.0, 6.0),
        Impairment::DriftSnr(500.0, 0.0),
    ];

    println!("\nAirtime per message (all copies) and first-copy body end:");
    for (name, c) in &profiles {
        let rep = repetition_samples(c) as f64 / c.sample_rate_hz as f64 * 1000.0;
        println!(
            "  {name:<10} airtime {:6.1} ms  first copy {:6.1} ms",
            rep * c.repetition_k as f64,
            c.estimated_first_copy_ms()
        );
    }

    let results: Vec<Vec<Cell>> = std::thread::scope(|s| {
        let handles: Vec<_> = profiles
            .iter()
            .map(|(_, c)| {
                s.spawn(move || {
                    imps.iter()
                        .enumerate()
                        .map(|(i, imp)| run_cell(c, *imp, trials, 0xFEC0 + i as u64))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    println!("\nDelivery rate % (mean start→delivery ms), {trials} trials per cell:");
    print!("| {:<22} |", "channel");
    for (name, _) in &profiles {
        print!(" {name:>15} |");
    }
    println!();
    print!("|{:-<24}|", "");
    for _ in &profiles {
        print!("{:->17}|", "");
    }
    println!();
    let mut false_total = 0;
    for (i, imp) in imps.iter().enumerate() {
        print!("| {:<22} |", imp.label());
        for r in &results {
            let c = &r[i];
            false_total += c.false_msgs;
            let pct = 100.0 * c.ok as f64 / c.trials as f64;
            print!(" {:>5.1} ({:>5.0} ms) |", pct, c.mean_latency_ms);
        }
        println!();
    }
    println!("\nfalse messages (all cells): {false_total}");
    assert_eq!(false_total, 0);
}
