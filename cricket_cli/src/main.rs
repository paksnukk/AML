use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use cricket_core::phy::PhyConfig;
use cricket_core::transport::TransportMessage;
use cricket_core::wav::{read_wav_f32, write_wav_f32};
use cricket_engine::measure_midi_loopback;
use cricket_engine::{
    audio, midi, AlarmKind, AudioIn, AudioOut, DecoderConfig, DecoderHandle, DeviceSelector,
    EncoderConfig, EncoderHandle, EngineEvent, MidiIn, MidiOut,
};
use std::sync::mpsc::Receiver;

/// Profile the real-time CLI modes run (no flag yet).
const PROFILE: &str = "robust";

#[derive(Parser)]
#[command(author, version, about = "Cricket MIDI-over-audio modem CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the real-time encoder (MIDI -> Audio)
    ///
    /// Devices are chosen by index or by exact name (see `list-devices`);
    /// a name that is not present is an error, never another device.
    Encode {
        /// Input MIDI device index
        #[arg(short, long)]
        midi_in: Option<usize>,

        /// Input MIDI port by exact name
        #[arg(long, value_name = "NAME", conflicts_with = "midi_in")]
        midi_in_name: Option<String>,

        /// Output Audio device index (default device if neither is given)
        #[arg(short, long)]
        audio_out: Option<usize>,

        /// Output Audio device by exact name
        #[arg(long, value_name = "NAME", conflicts_with = "audio_out")]
        audio_out_name: Option<String>,

        /// Simulate a C-major scale if no MIDI device is used
        #[arg(long, default_value_t = false)]
        simulate: bool,
    },
    /// Start the real-time decoder (Audio -> MIDI)
    ///
    /// Devices are chosen by index or by exact name (see `list-devices`);
    /// a name that is not present is an error, never another device.
    Decode {
        /// Input Audio device index (default device if neither is given)
        #[arg(short, long)]
        audio_in: Option<usize>,

        /// Input Audio device by exact name
        #[arg(long, value_name = "NAME", conflicts_with = "audio_in")]
        audio_in_name: Option<String>,

        /// Output MIDI device index
        #[arg(short, long)]
        midi_out: Option<usize>,

        /// Output MIDI port by exact name
        #[arg(long, value_name = "NAME", conflicts_with = "midi_out")]
        midi_out_name: Option<String>,
    },
    /// List available Audio and MIDI devices
    ListDevices,
    /// Synthetic / WAV loopback test for the Cricket modem
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
            midi_in_name,
            audio_out,
            audio_out_name,
            simulate,
        } => {
            run_encoder(
                selector(*midi_in, midi_in_name),
                selector(*audio_out, audio_out_name),
                *simulate,
            )?;
        }
        Commands::Decode {
            audio_in,
            audio_in_name,
            midi_out,
            midi_out_name,
        } => {
            run_decoder(
                selector(*audio_in, audio_in_name),
                selector(*midi_out, midi_out_name),
            )?;
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

/// Device from an index flag or a name flag (clap rejects both at once);
/// `Default` when neither is given.
fn selector(index: Option<usize>, name: &Option<String>) -> DeviceSelector {
    match (index, name) {
        (_, Some(n)) => DeviceSelector::Name(n.clone()),
        (i, None) => DeviceSelector::from_index(i),
    }
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
    println!("Running Cricket loopback test...");
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
        println!(
            "Loaded {} samples @ {} Hz from {}",
            samples.len(),
            rate,
            path
        );
        samples
    } else {
        let samples = cricket_core::encode_to_samples(config.clone(), &messages);
        if let Some(path) = wav_out {
            write_wav_f32(path, config.sample_rate_hz, &samples).map_err(|e| anyhow!(e))?;
            println!("Wrote {} samples to {}", samples.len(), path);
        }
        samples
    };

    let decoded = cricket_core::decode_samples(config, &samples);
    println!("Decoded {} messages:", decoded.len());
    for m in &decoded {
        println!("  {:?}", m);
    }

    if wav_in.is_none() {
        // Filter heartbeats optionally — expect at least note on/off/program
        assert!(decoded
            .iter()
            .any(|m| matches!(m, TransportMessage::NoteOn { .. })));
        assert!(decoded
            .iter()
            .any(|m| matches!(m, TransportMessage::NoteOff { .. })));
        println!("Test Passed!");
    }
    Ok(())
}

const LOOPBACK_NOTES: usize = 20;

fn run_loopback_metrics() -> Result<()> {
    println!("Cricket engine loopback latency (paced MIDI in → MIDI out)...");
    for profile in ["fast", "robust"] {
        let s = measure_midi_loopback(profile, LOOPBACK_NOTES)?;
        if s.n < LOOPBACK_NOTES {
            return Err(anyhow!(
                "{}: only {} of {} NoteOns delivered",
                profile,
                s.n,
                LOOPBACK_NOTES
            ));
        }
        println!(
            "{}: n={} mean={:.1} ms p95={:.1} ms jitter={:.1} ms",
            profile, s.n, s.mean_ms, s.p95_ms, s.jitter_ms
        );
    }
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
            EngineEvent::Alarm { message, .. } => println!("[ALARM] {}", message),
            EngineEvent::AlarmCleared { message, .. } => println!("[ALARM CLEARED] {}", message),
            EngineEvent::Stopped => break,
            EngineEvent::Level(_) | EngineEvent::Stats(_) | EngineEvent::TxStatus { .. } => {}
        }
    }
    Err(anyhow!("{} stopped", what))
}

fn run_decoder(audio_in: DeviceSelector, midi_out: DeviceSelector) -> Result<()> {
    println!("Initializing Cricket decoder...");
    let midi_out = match midi_out {
        DeviceSelector::Default => MidiOut::None,
        sel => {
            // Fail fast on a bad port, as before the engine.
            println!("MIDI Out: {}", midi::output_port_name(&sel)?);
            MidiOut::Device(sel)
        }
    };
    let dec = DecoderHandle::start(DecoderConfig {
        audio_in: AudioIn::Device(audio_in),
        midi_out,
        profile: PROFILE.into(),
        link_dead_ms: None,
    })?;
    print_events(dec.events(), "Decoder")
}

fn run_encoder(midi_in: DeviceSelector, audio_out: DeviceSelector, simulate: bool) -> Result<()> {
    println!("Initializing Cricket encoder...");
    let midi_in = match midi_in {
        DeviceSelector::Default => MidiIn::None,
        sel => {
            // Fail fast on a bad port, as before the engine.
            println!("MIDI In: {}", midi::input_port_name(&sel)?);
            MidiIn::Device(sel)
        }
    };
    if simulate || matches!(midi_in, MidiIn::None) {
        println!("Simulation mode: C-major scale + heartbeats");
    }
    let enc = EncoderHandle::start(EncoderConfig {
        audio_out: AudioOut::Device(audio_out),
        midi_in,
        profile: PROFILE.into(),
        simulate,
    })?;
    print_events(enc.events(), "Encoder")
}
