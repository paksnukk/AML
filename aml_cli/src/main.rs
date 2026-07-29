use aml_core::modem::{AmlModemRx, AmlModemTx, ModemOutput};
use aml_core::phy::PhyConfig;
use aml_core::transport::{midi_to_message, TransportMessage};
use aml_core::wav::{read_wav_f32, write_wav_f32};
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use cpal::traits::StreamTrait;
use midir::{MidiInput, MidiOutput};
use ringbuf::HeapRb;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod audio;

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
    audio::list_devices()?;
    println!("--- MIDI Input Devices ---");
    let midi_in = MidiInput::new("aml_cli list")?;
    for (i, port) in midi_in.ports().iter().enumerate() {
        println!(
            "{}: {}",
            i,
            midi_in.port_name(port).unwrap_or_else(|_| "Unknown".into())
        );
    }
    println!("--- MIDI Output Devices ---");
    let midi_out = MidiOutput::new("aml_cli list")?;
    for (i, port) in midi_out.ports().iter().enumerate() {
        println!(
            "{}: {}",
            i,
            midi_out
                .port_name(port)
                .unwrap_or_else(|_| "Unknown".into())
        );
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

fn run_decoder(audio_in_index: Option<usize>, midi_out_index: Option<usize>) -> Result<()> {
    println!("Initializing AML V2 Decoder...");
    let mut config = PhyConfig::robust();

    let midi_conn = if let Some(idx) = midi_out_index {
        let midi_out = MidiOutput::new("aml_cli decode")?;
        let ports = midi_out.ports();
        let port = ports
            .get(idx)
            .ok_or_else(|| anyhow!("MIDI out index {} not found", idx))?;
        println!("MIDI Out: {}", midi_out.port_name(port)?);
        Some(Arc::new(Mutex::new(
            midi_out
                .connect(port, "aml_cli out")
                .map_err(|e| anyhow!("MIDI connect: {}", e))?,
        )))
    } else {
        println!("No MIDI out selected; decoded messages will be logged only.");
        None
    };

    let ring = HeapRb::<f32>::new(config.sample_rate_hz as usize * 2);
    let (mut producer, mut consumer) = ring.split();

    let (stream, stream_sample_rate, channels) =
        audio::setup_input_stream(audio_in_index, move |data, chans| {
            // Channel 0 only
            for frame in data.chunks(chans) {
                if let Some(&s) = frame.first() {
                    let _ = producer.push(s);
                }
            }
        })?;

    if stream_sample_rate != config.sample_rate_hz {
        eprintln!(
            "Adapting sample rate {} -> {}",
            config.sample_rate_hz, stream_sample_rate
        );
        config = config.with_sample_rate(stream_sample_rate);
    }
    println!("Input channels reported: {} (using ch0)", channels);

    stream.play()?;
    let mut rx = AmlModemRx::new(config);
    println!("Listening (V2)...");

    loop {
        let mut chunk = Vec::new();
        while let Some(s) = consumer.pop() {
            chunk.push(s);
            if chunk.len() >= 512 {
                break;
            }
        }
        if chunk.is_empty() {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        for out in rx.push_samples(&chunk) {
            match out {
                ModemOutput::Message(msg) => {
                    println!("[MSG] {:?}", msg);
                }
                ModemOutput::Midi(bytes) => {
                    if let Some(conn) = &midi_conn {
                        if let Ok(mut c) = conn.lock() {
                            let _ = c.send(&bytes);
                        }
                    }
                }
                ModemOutput::LinkDead => {
                    println!("[LINK DEAD] All Notes Off");
                }
                ModemOutput::SyncLost => {}
                ModemOutput::Stats(stats) => {
                    // Periodic light log could go here; keep quiet unless useful
                    let _ = stats;
                }
            }
        }
    }
}

fn run_encoder(
    midi_in_index: Option<usize>,
    audio_out_index: Option<usize>,
    simulate: bool,
) -> Result<()> {
    println!("Initializing AML V2 Encoder...");
    let mut config = PhyConfig::robust();

    let pending: Arc<Mutex<VecDeque<TransportMessage>>> = Arc::new(Mutex::new(VecDeque::new()));

    // Optional MIDI input
    let _midi_conn = if let Some(idx) = midi_in_index {
        let midi_in = MidiInput::new("aml_cli encode")?;
        let ports = midi_in.ports();
        let port = ports
            .get(idx)
            .ok_or_else(|| anyhow!("MIDI in index {} not found", idx))?;
        println!("MIDI In: {}", midi_in.port_name(port)?);
        let pending_cb = pending.clone();
        let conn = midi_in
            .connect(
                port,
                "aml_cli in",
                move |_stamp, message, _| {
                    if let Some(msg) = midi_to_message(message) {
                        if let Ok(mut q) = pending_cb.lock() {
                            q.push_back(msg);
                        }
                    }
                },
                (),
            )
            .map_err(|e| anyhow!("MIDI connect: {}", e))?;
        Some(conn)
    } else {
        None
    };

    let ring = HeapRb::<f32>::new(config.sample_rate_hz as usize * 4);
    let (mut producer, mut consumer) = ring.split();

    let (stream, stream_sample_rate, channels) =
        audio::setup_output_stream(audio_out_index, move |data, chans| {
            // Fill channel 0 from ring; duplicate mono to other channels
            for frame in data.chunks_mut(chans) {
                let sample = consumer.pop().unwrap_or(0.0);
                for (i, s) in frame.iter_mut().enumerate() {
                    *s = if i == 0 { sample } else { sample };
                }
            }
        })?;

    if stream_sample_rate != config.sample_rate_hz {
        eprintln!(
            "Adapting sample rate {} -> {}",
            config.sample_rate_hz, stream_sample_rate
        );
        config = config.with_sample_rate(stream_sample_rate);
    }
    println!("Output channels reported: {} (mono on all)", channels);

    stream.play()?;
    let mut tx = AmlModemTx::new(config.clone());
    println!("Encoder running (V2).");

    let use_simulate = simulate || midi_in_index.is_none();
    if use_simulate {
        println!("Simulation mode: C-major scale + heartbeats");
    }

    let mut note_idx = 0usize;
    let scale = [0u8, 2, 4, 5, 7, 9, 11, 12];
    let mut last_heartbeat = Instant::now();
    let mut last_sim_note = Instant::now();

    loop {
        // Drain pending MIDI messages
        let msgs: Vec<TransportMessage> = {
            let mut q = pending.lock().unwrap();
            q.drain(..).collect()
        };

        for msg in msgs {
            println!("TX {:?}", msg);
            push_audio(&mut producer, &tx.encode_message(&msg));
        }

        if use_simulate && last_sim_note.elapsed() >= Duration::from_millis(1000) {
            let note = 60 + scale[note_idx % scale.len()];
            let msg = TransportMessage::NoteOn {
                note,
                velocity: 100,
            };
            println!("TX sim {:?}", msg);
            push_audio(&mut producer, &tx.encode_message(&msg));
            // Matching note off shortly after via queue timing — send immediately after
            let off = TransportMessage::NoteOff { note };
            push_audio(&mut producer, &tx.encode_message(&off));
            note_idx += 1;
            last_sim_note = Instant::now();
        }

        if last_heartbeat.elapsed()
            >= Duration::from_millis(config.heartbeat_interval_ms as u64)
        {
            push_audio(&mut producer, &tx.encode_heartbeat());
            last_heartbeat = Instant::now();
        }

        thread::sleep(Duration::from_millis(5));
    }
}

fn push_audio(producer: &mut ringbuf::HeapProducer<f32>, samples: &[f32]) {
    for &sample in samples {
        while producer.is_full() {
            thread::sleep(Duration::from_millis(1));
        }
        let _ = producer.push(sample);
    }
}
