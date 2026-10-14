//! A/B measurements: MFSK (robust, fast) vs experimental BMC (`ltc*`).
//!
//! Prints Markdown tables; not an assertion test. Run with
//!
//! ```sh
//! cargo test --release -p cricket_core --test phy_ab_measurements -- --ignored --nocapture
//! ```
//!
//! Results are recorded in `docs/proposals/ltc-bmc.md`.

mod support;

use cricket_core::modem::{
    encode_to_samples, repetition_samples, CricketModemRx, CricketModemTx, ModemOutput,
};
use cricket_core::phy::PhyConfig;
use cricket_core::transport::TransportMessage;
use std::time::Instant;
use support::{decode_chunked, ChannelSim, XorShift64};

const TRIALS: usize = 200;
const CHUNK_SIZES: [usize; 5] = [64, 128, 240, 512, 4096];

fn profiles() -> Vec<(&'static str, PhyConfig)> {
    vec![
        ("robust", PhyConfig::robust()),
        ("fast", PhyConfig::fast()),
        ("ltc2400", PhyConfig::ltc_with_bit_rate(2400)),
        ("ltc4800", PhyConfig::ltc_with_bit_rate(4800)),
        ("ltc9600", PhyConfig::ltc_with_bit_rate(9600)),
    ]
}

type Impair = fn(ChannelSim) -> ChannelSim;

fn conditions() -> Vec<(&'static str, Impair)> {
    vec![
        ("clean", |s| s),
        ("SNR 30 dB", |s| s.snr_db(30.0)),
        ("SNR 20 dB", |s| s.snr_db(20.0)),
        ("SNR 15 dB", |s| s.snr_db(15.0)),
        ("SNR 12 dB", |s| s.snr_db(12.0)),
        ("SNR 10 dB", |s| s.snr_db(10.0)),
        ("SNR 8 dB", |s| s.snr_db(8.0)),
        ("SNR 6 dB", |s| s.snr_db(6.0)),
        ("SNR 4 dB", |s| s.snr_db(4.0)),
        ("SNR 2 dB", |s| s.snr_db(2.0)),
        ("SNR 0 dB", |s| s.snr_db(0.0)),
        ("level -10 dB", |s| s.gain(0.316)),
        ("level -30 dB", |s| s.gain(0.0316)),
        ("level -50 dB", |s| s.gain(0.00316)),
        ("level -30 dB + SNR 20", |s| s.gain(0.0316).snr_db(20.0)),
        ("polarity inverted", |s| s.invert()),
        ("inverted + SNR 12", |s| s.invert().snr_db(12.0)),
        ("drift +1000 ppm", |s| s.drift_ppm(1000.0)),
        ("drift -1000 ppm", |s| s.drift_ppm(-1000.0)),
        ("drift +1000 ppm + SNR 12", |s| {
            s.drift_ppm(1000.0).snr_db(12.0)
        }),
        ("drift +1 % (SRC misconfig)", |s| s.drift_ppm(10_000.0)),
        ("drift -1 %", |s| s.drift_ppm(-10_000.0)),
        ("1-pole LP 15 kHz", |s| s.lowpass_hz(15_000.0)),
        ("1-pole LP 8 kHz", |s| s.lowpass_hz(8_000.0)),
        ("1-pole LP 4 kHz", |s| s.lowpass_hz(4_000.0)),
        ("2-pole LP 12 kHz", |s| s.lowpass2_hz(12_000.0)),
        ("2-pole LP 6 kHz", |s| s.lowpass2_hz(6_000.0)),
        ("2-pole LP 3.4 kHz (telephone)", |s| s.lowpass2_hz(3_400.0)),
        ("HP 20 Hz", |s| s.highpass_hz(20.0)),
        ("HP 300 Hz (transformer)", |s| s.highpass_hz(300.0)),
        ("HP 1 kHz", |s| s.highpass_hz(1_000.0)),
        ("HP 300 Hz + LP 8 kHz + SNR 15", |s| {
            s.highpass_hz(300.0).lowpass_hz(8_000.0).snr_db(15.0)
        }),
        ("EQ +12 dB @ 3 kHz Q1", |s| s.peaking_eq(3_000.0, 12.0, 1.0)),
        ("EQ -12 dB @ 3 kHz Q1", |s| {
            s.peaking_eq(3_000.0, -12.0, 1.0)
        }),
        ("EQ -12 dB @ 6 kHz Q1", |s| {
            s.peaking_eq(6_000.0, -12.0, 1.0)
        }),
        ("soft clip (drive 4)", |s| s.soft_clip(4.0)),
        ("hum 50 Hz @ -6 dB", |s| s.hum(50.0, -6.0)),
        ("hum 50 Hz @ 0 dB", |s| s.hum(50.0, 0.0)),
    ]
}

fn note_on_for(i: usize) -> TransportMessage {
    TransportMessage::NoteOn {
        note: (i % 128) as u8,
        velocity: (1 + (i * 7) % 127) as u8,
    }
}

fn success_rate(config: &PhyConfig, impair: Impair, seed: u64) -> usize {
    let mut rng = XorShift64::new(seed);
    let mut ok = 0;
    for trial in 0..TRIALS {
        let offset = rng.below(4 * config.samples_per_symbol_period().max(64));
        let chunk = CHUNK_SIZES[rng.below(CHUNK_SIZES.len())];
        let messages = [note_on_for(trial)];
        let clean = encode_to_samples(config.clone(), &messages);
        let sim = impair(ChannelSim::new(config.clone(), rng.next_u64()).offset(offset));
        if decode_chunked(config.clone(), &sim.apply(&clean), chunk) == messages {
            ok += 1;
        }
    }
    ok
}

/// Samples from the first sample of a message's audio until the receiver
/// outputs it (chunk of 16 samples), clean channel.
fn decode_delay_ms(config: &PhyConfig) -> f64 {
    let mut tx = CricketModemTx::new(config.clone());
    let mut rx = CricketModemRx::new(config.clone());
    let mut audio = tx.encode_message(&note_on_for(60));
    audio.extend(vec![0.0; config.sample_rate_hz as usize / 2]);
    let mut pushed = 0usize;
    for chunk in audio.chunks(16) {
        pushed += chunk.len();
        if rx
            .push_samples(chunk)
            .iter()
            .any(|o| matches!(o, ModemOutput::Message(_)))
        {
            return pushed as f64 * 1000.0 / config.sample_rate_hz as f64;
        }
    }
    f64::NAN
}

/// Receiver CPU time per input sample (ns) over 10 s of noisy traffic.
fn rx_ns_per_sample(config: &PhyConfig) -> f64 {
    let msgs: Vec<_> = (0..20).map(note_on_for).collect();
    let mut audio = encode_to_samples(config.clone(), &msgs);
    audio.resize(config.sample_rate_hz as usize * 10, 0.0);
    let audio = ChannelSim::new(config.clone(), 3)
        .snr_db(30.0)
        .apply(&audio);
    let mut rx = CricketModemRx::new(config.clone());
    let t = Instant::now();
    let mut n = 0usize;
    for chunk in audio.chunks(256) {
        n += rx.push_samples(chunk).len();
    }
    std::hint::black_box(n);
    t.elapsed().as_nanos() as f64 / audio.len() as f64
}

/// Messages decoded from `seconds` of white noise (should be 0).
fn false_decodes(config: &PhyConfig, seconds: usize) -> usize {
    let mut rng = XorShift64::new(0xFA15E);
    let noise: Vec<f32> = (0..config.sample_rate_hz as usize * seconds)
        .map(|_| (rng.gaussian() * 0.3) as f32)
        .collect();
    decode_chunked(config.clone(), &noise, 512).len()
}

#[test]
#[ignore = "measurement report; run in release with --ignored --nocapture"]
fn ab_report() {
    let profiles = profiles();
    let names: Vec<&str> = profiles.iter().map(|p| p.0).collect();

    // Timing.
    println!("\n### Airtime and decode delay (NoteOn, 48 kHz)\n");
    println!("| profile | burst ms (one copy) | copies K | airtime/msg ms | decode delay ms | max msgs/s |");
    println!("|---|---|---|---|---|---|");
    for (name, config) in &profiles {
        let rep = repetition_samples(config) as f64 * 1000.0 / config.sample_rate_hz as f64;
        let k = config.repetition_k.max(1);
        let air = rep * k as f64;
        println!(
            "| {name} | {:.1} | {k} | {air:.1} | {:.1} | {:.0} |",
            config.estimated_first_copy_ms(),
            decode_delay_ms(config),
            1000.0 / air
        );
    }

    // CPU.
    println!("\n### Receiver CPU (host, release, ns per input sample)\n");
    println!("| profile | ns/sample | x realtime @48k | false msgs in 60 s noise |");
    println!("|---|---|---|---|");
    for (name, config) in &profiles {
        let ns = rx_ns_per_sample(config);
        println!(
            "| {name} | {ns:.1} | {:.0} | {} |",
            1e9 / (ns * config.sample_rate_hz as f64),
            false_decodes(config, 60)
        );
    }

    // Robustness, one thread per profile.
    let conds = conditions();
    let handles: Vec<_> = profiles
        .iter()
        .cloned()
        .map(|(_, config)| {
            let conds = conds.clone();
            std::thread::spawn(move || {
                conds
                    .iter()
                    .enumerate()
                    .map(|(i, (_, f))| success_rate(&config, *f, 0xAB00 + i as u64))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let results: Vec<Vec<usize>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    println!("\n### Packet success (% of {TRIALS} single NoteOns, random offset/chunk)\n");
    println!("| condition | {} |", names.join(" | "));
    println!("|---|{}", "---|".repeat(names.len()));
    for (ci, (cname, _)) in conds.iter().enumerate() {
        let cells: Vec<String> = results
            .iter()
            .map(|r| format!("{:.1}", r[ci] as f64 * 100.0 / TRIALS as f64))
            .collect();
        println!("| {cname} | {} |", cells.join(" | "));
    }
}

/// RX parameter sweep for the BMC profile at 4800 bit/s.
#[test]
#[ignore = "measurement report; run in release with --ignored --nocapture"]
fn bmc_param_sweep() {
    use cricket_core::phy::Modulation;
    let conds: Vec<(&str, Impair)> = vec![
        ("SNR 8", |s| s.snr_db(8.0)),
        ("SNR 6", |s| s.snr_db(6.0)),
        ("EQ+12@3k", |s| s.peaking_eq(3_000.0, 12.0, 1.0)),
        ("EQ-12@3k", |s| s.peaking_eq(3_000.0, -12.0, 1.0)),
        ("hum 0dB", |s| s.hum(50.0, 0.0)),
        ("HP 1k", |s| s.highpass_hz(1_000.0)),
        ("LP2 4k", |s| s.lowpass2_hz(4_000.0)),
    ];
    let mut jobs = Vec::new();
    for lp in [0.75f32, 1.0, 1.5, 2.0] {
        for hyst in [0.1f32, 0.2, 0.3] {
            for hp in [0.02f32, 0.05, 0.1] {
                jobs.push((lp, hyst, hp));
            }
        }
    }
    let names: Vec<&str> = conds.iter().map(|c| c.0).collect();
    println!(
        "\n| lp x bitrate | hysteresis | hp x bitrate | {} |",
        names.join(" | ")
    );
    println!("|---|---|---|{}", "---|".repeat(conds.len()));
    let handles: Vec<_> = jobs
        .chunks(jobs.len().div_ceil(8))
        .map(|chunk| {
            let chunk = chunk.to_vec();
            let conds = conds.clone();
            std::thread::spawn(move || {
                chunk
                    .into_iter()
                    .map(|(lp, hyst, hp)| {
                        let mut config = PhyConfig::ltc();
                        if let Modulation::Bmc(p) = &mut config.modulation {
                            p.rx_lowpass_ratio = lp;
                            p.hysteresis = hyst;
                            p.rx_highpass_ratio = hp;
                        }
                        let r: Vec<usize> = conds
                            .iter()
                            .enumerate()
                            .map(|(i, (_, f))| success_rate(&config, *f, 0x5EE0 + i as u64))
                            .collect();
                        (lp, hyst, hp, r)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for h in handles {
        for (lp, hyst, hp, r) in h.join().unwrap() {
            let cells: Vec<String> = r
                .iter()
                .map(|&ok| format!("{:.0}", ok as f64 * 100.0 / TRIALS as f64))
                .collect();
            println!("| {lp} | {hyst} | {hp} | {} |", cells.join(" | "));
        }
    }
}
