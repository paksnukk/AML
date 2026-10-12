use aml_core::modem::{AmlModemRx, AmlModemTx, ModemOutput};
use aml_core::phy::PhyConfig;
use aml_core::transport::TransportMessage;
use aml_core::wav::{read_wav_f32, write_wav_f32};
use aml_engine::{
    audio, midi, AlarmKind, AudioIn, AudioOut, DecoderConfig, DecoderHandle, DeviceSelector,
    EncoderConfig, EncoderHandle, EngineEvent, MidiIn, MidiOut,
};
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::sync::mpsc::Receiver;
use std::time::Instant;

/// Profile the real-time CLI modes run (no flag yet).
const PROFILE: &str = "robust";

#[derive(Parser)]
#[command(author, version, about = "AML V2 audio modem CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the real-time encoder (MIDI -> Audio)
    Encode {
        /// Input MIDI device index
        #[arg(short, long)]
        midi_in: Option<usize>,

        /// Output Audio device index
        #[arg(short, long)]
        audio_out: Option<usize>,

        /// Simulate a C-major scale if no MIDI device is used
        #[arg(long, default_value_t = false)]
        simulate: bool,
    },
    /// Start the real-time decoder (Audio -> MIDI)
    Decode {
        /// Input Audio device index
        #[arg(short, long)]
        audio_in: Option<usize>,

        /// Output MIDI device index
        #[arg(short, long)]
        midi_out: Option<usize>,
    },
    /// List available Audio and MIDI devices
    ListDevices,
    /// Synthetic / WAV loopback test for the V2 modem
    Test {
        /// Optional path to write encoded WAV
        #[arg(long)]
        wav_out: Option<String>,
        /// Optional path to decode from WAV instead of synthetic encode
        #[arg(long)]
        wav_in: Option<String>,
    },
    /// Encode→decode loopback with latency measurement (in-process)
    Loopback,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Encode {
            midi_in,
            audio_out,
            simulate,
        } => {
            run_encoder(*midi_in, *audio_out, *simulate)?;
        }
        Commands::Decode { audio_in, midi_out } => {
            run_decoder(*audio_in, *midi_out)?;
        }
        Commands::ListDevices => {
            list_all_devices()?;
        }
        Commands::Test { wav_out, wav_in } => {
            run_test_mode(wav_out.as_deref(), wav_in.as_deref())?;
        }
        Commands::Loopback => {
            run_loopback_metrics()?;
        }
    }

    Ok(())
}

fn list_all_devices() -> Result<()> {
    println!("Host: {}", audio::host_name());
    println!("--- Input Devices ---");
    for (index, name) in audio::list_input_devices()?.iter().enumerate() {
        println!("{}: {}", index, name);
    }
    println!("--- Output Devices ---");
    for (index, name) in audio::list_output_devices()?.iter().enumerate() {
        println!("{}: {}", index, name);
    }
    println!("--- MIDI Input Devices ---");
    for (i, name) in midi::list_inputs()?.iter().enumerate() {
        println!("{}: {}", i, name);
    }
    println!("--- MIDI Output Devices ---");
    for (i, name) in midi::list_outputs()?.iter().enumerate() {
        println!("{}: {}", i, name);
    }
    Ok(())
}

fn run_test_mode(wav_out: Option<&str>, wav_in: Option<&str>) -> Result<()> {
    println!("Running AML V2 loopback test...");
    let config = PhyConfig::robust();
    let messages = vec![
        TransportMessage::NoteOn {
            note: 60,
            velocity: 100,
        },
        TransportMessage::NoteOff { note: 60 },
        TransportMessage::ProgramChange { program: 10 },
        TransportMessage::Heartbeat,
    ];

    let samples = if let Some(path) = wav_in {
        let (rate, samples) = read_wav_f32(path).map_err(|e| anyhow!(e))?;
        println!("Loaded {} samples @ {} Hz from {}", samples.len(), rate, path);
        samples
    } else {
        let samples = aml_core::encode_to_samples(config.clone(), &messages);
        if let Some(path) = wav_out {
            write_wav_f32(path, config.sample_rate_hz, &samples).map_err(|e| anyhow!(e))?;
            println!("Wrote {} samples to {}", samples.len(), path);
        }
        samples
    };

    let decoded = aml_core::decode_samples(config, &samples);
    println!("Decoded {} messages:", decoded.len());
    for m in &decoded {
        println!("  {:?}", m);
    }

    if wav_in.is_none() {
        // Filter heartbeats optionally — expect at least note on/off/program
        assert!(decoded.iter().any(|m| matches!(m, TransportMessage::NoteOn { .. })));
        assert!(decoded.iter().any(|m| matches!(m, TransportMessage::NoteOff { .. })));
        println!("Test Passed!");
    }
    Ok(())
}

fn run_loopback_metrics() -> Result<()> {
    println!("AML V2 in-process loopback latency measurement...");
    let config = PhyConfig::robust();
    let mut tx = AmlModemTx::new(config.clone());
    let mut rx = AmlModemRx::new(config.clone());

    let mut latencies_ms = Vec::new();
    for note in [60u8, 62, 64, 65, 67] {
        let msg = TransportMessage::NoteOn {
            note,
            velocity: 90,
        };
        let t0 = Instant::now();
        let audio = tx.encode_message(&msg);
        let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = Instant::now();
        let mut got = None;
        for chunk in audio.chunks(256) {
            for out in rx.push_samples(chunk) {
                if let ModemOutput::Message(m) = out {
                    if got.is_none() {
                        got = Some(m);
                    }
                }
            }
        }
        let decode_ms = t1.elapsed().as_secs_f64() * 1000.0;
        let audio_ms = audio.len() as f64 * 1000.0 / config.sample_rate_hz as f64;
        // One repetition duration approximates delivery latency (K=2 sends two copies)
        let one_rep_ms = audio_ms / config.repetition_k.max(1) as f64;

        println!(
            "note {}: encode={:.2}ms decode_cpu={:.2}ms audio_total={:.1}ms ~e2e_first_copy={:.1}ms got={:?}",
            note, encode_ms, decode_ms, audio_ms, one_rep_ms, got
        );
        assert_eq!(got, Some(msg));
        latencies_ms.push(one_rep_ms);
    }

    let avg = latencies_ms.iter().sum::<f64>() / latencies_ms.len() as f64;
    let min = latencies_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = latencies_ms.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "Latency (first repetition audio duration): avg={:.1}ms min={:.1}ms max={:.1}ms jitter={:.1}ms",
        avg,
        min,
        max,
        max - min
    );
    Ok(())
}


/// Print engine events until the worker stops (it only stops on its own
/// after a device error, so that is reported as a failure).
fn print_events(events: &Receiver<EngineEvent>, what: &str) -> Result<()> {
    for event in events {
        match event {
            EngineEvent::Log(line) => println!("{}", line),
            EngineEvent::Decoded(msg) => println!("[MSG] {:?}", msg),
            EngineEvent::Alarm {
                kind: AlarmKind::LinkDead,
                ..
            } => println!("[LINK DEAD] All Notes Off"),
            EngineEvent::Stopped => break,
            EngineEvent::Level(_) | EngineEvent::Stats(_) | EngineEvent::TxStatus { .. } => {}
        }
    }
    Err(anyhow!("{} stopped", what))
}

fn run_decoder(audio_in_index: Option<usize>, midi_out_index: Option<usize>) -> Result<()> {
    println!("Initializing AML V2 Decoder...");
    let midi_out = match midi_out_index {
        Some(idx) => {
            let sel = DeviceSelector::Index(idx);
            // Fail fast on a bad port, as before the engine.
            println!("MIDI Out: {}", midi::output_port_name(&sel)?);
            MidiOut::Device(sel)
        }
        None => MidiOut::None,
    };
    let dec = DecoderHandle::start(DecoderConfig {
        audio_in: AudioIn::Device(DeviceSelector::from_index(audio_in_index)),
        midi_out,
        profile: PROFILE.into(),
    })?;
    print_events(dec.events(), "Decoder")
}

fn run_encoder(
    midi_in_index: Option<usize>,
    audio_out_index: Option<usize>,
    simulate: bool,
) -> Result<()> {
    println!("Initializing AML V2 Encoder...");
    let midi_in = match midi_in_index {
        Some(idx) => {
            let sel = DeviceSelector::Index(idx);
            // Fail fast on a bad port, as before the engine.
            println!("MIDI In: {}", midi::input_port_name(&sel)?);
            MidiIn::Device(sel)
        }
        None => MidiIn::None,
    };
    if simulate || midi_in_index.is_none() {
        println!("Simulation mode: C-major scale + heartbeats");
    }
    let enc = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Device(DeviceSelector::from_index(audio_out_index)),
        midi_in,
        profile: PROFILE.into(),
        simulate,
    })?;
    print_events(enc.events(), "Encoder")
}
