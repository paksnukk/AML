use aml_core::decoder::Decoder;
use aml_core::encoder::Encoder;
use aml_core::profile::{AmlConfig, ProfileDefinition};
use aml_core::protocol::{FrameType, SymbolValue};
use anyhow::Result;
use clap::{Parser, Subcommand};
use cpal::traits::StreamTrait;
use ringbuf::HeapRb;
use std::collections::VecDeque;
use std::thread;
use std::time::Duration;

mod audio;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
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
    /// Run a internal loopback test to verify core logic
    Test,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Encode { midi_in, audio_out } => {
            run_encoder(*midi_in, *audio_out)?;
        }
        Commands::Decode { audio_in, midi_out } => {
            run_decoder(*audio_in, *midi_out)?;
        }
        Commands::ListDevices => {
            audio::list_devices()?;
        }
        Commands::Test => {
            run_test_mode()?;
        }
    }

    Ok(())
}

fn run_decoder(audio_in_index: Option<usize>, _midi_out_index: Option<usize>) -> Result<()> {
    println!("Initializing Decoder...");

    // 1. Setup Profile
    // TODO: Load from file eventually. Using default Pro Clean for now.
    let mut profile = ProfileDefinition::default_pro_clean();
    let sample_rate = profile.audio.sample_rate_hz;

    // 2. Setup RingBuffer for Audio Thread -> Main Thread
    // 48000Hz * 1s buffer = 48000 samples.
    let ring = HeapRb::<f32>::new(sample_rate as usize);
    let (mut producer, mut consumer) = ring.split();

    // 3. Start Audio Stream
    let (stream, stream_sample_rate) = audio::setup_input_stream(audio_in_index, move |data| {
        for &sample in data {
            let _ = producer.push(sample);
        }
    })?;

    // Warn if sample rates mismatch
    if stream_sample_rate != sample_rate {
        eprintln!(
            "WARNING: Audio Device running at {}Hz, Profile expects {}Hz. Adapting profile...",
            stream_sample_rate, sample_rate
        );
        profile.audio.sample_rate_hz = stream_sample_rate;
    }

    stream.play()?;
    println!("Listening...");

    // 4. Decoder Loop
    let mut decoder = Decoder::new(profile.clone());

    // We need a local buffer to accumulate samples for sliding window analysis
    let symbol_duration_ms = profile.audio.symbol_duration_ms;
    let samples_per_symbol = (stream_sample_rate as f32 * symbol_duration_ms / 1000.0) as usize;
    let hop_size = samples_per_symbol / 4; // Scan step when searching for preamble

    let mut analysis_buffer: VecDeque<f32> = VecDeque::with_capacity(samples_per_symbol * 2);

    println!(
        "Symbol Size: {} samples. Hop Size: {}",
        samples_per_symbol, hop_size
    );

    loop {
        // Move data from ringbuffer to analysis buffer
        while let Some(sample) = consumer.pop() {
            analysis_buffer.push_back(sample);
        }

        // If we have enough data for a symbol window
        if analysis_buffer.len() >= samples_per_symbol {
            // Get the window
            let window: Vec<f32> = analysis_buffer
                .iter()
                .take(samples_per_symbol)
                .cloned()
                .collect();

            // Determine candidates based on state
            let candidates = decoder.get_scan_candidates();

            // Check signal
            if let Some((symbol, _power)) = decoder.detect_symbol(&window, &candidates) {
                // Symbol Detected!

                // Visualization (Print dot for Preamble, Letter for others)
                match symbol {
                    SymbolValue::Preamble => print!("."),
                    _ => print!("!"),
                }
                use std::io::Write;
                std::io::stdout().flush().unwrap();

                if let Some(frame) = decoder.handle_symbol(symbol) {
                    println!(
                        "\n[DECODED FRAME]: {:?} Payload: {:?}",
                        frame.frame_type, frame.payload
                    );
                    // TODO: Send to MIDI Out
                }

                // If we detected a symbol, we should advance by the symbol duration
                // (minus some small overlap? or strict grid?)
                // For V1 simple logic: Consume full symbol duration.
                // NOTE: This assumes perfect alignment. A robust version syncs phase.

                let advance = samples_per_symbol;
                if analysis_buffer.len() >= advance {
                    analysis_buffer.drain(0..advance);
                }
            } else {
                // No valid symbol detected.

                match decoder.state {
                    aml_core::decoder::DecoderState::WaitPreamble => {
                        // Scan mode: Advance by hop_size
                        if analysis_buffer.len() >= hop_size {
                            analysis_buffer.drain(0..hop_size);
                        }
                    }
                    _ => {
                        // We were expecting a symbol but didn't find it.
                        // This usually means we lost sync or signal dropped.
                        // Reset state.
                        decoder.reset_state();
                        // Advance slightly to try re-syncing?
                        if analysis_buffer.len() >= hop_size {
                            analysis_buffer.drain(0..hop_size);
                        }
                    }
                }
            }
        } else {
            // Sleep briefly to avoid busy loop if buffer is empty
            thread::sleep(Duration::from_millis(1));
        }
    }
}

fn run_test_mode() -> Result<()> {
    println!("Running AML Core Loopback Test...");

    // 1. Load Profile (using a hardcoded subset of the JSON for simplicity in this test)
    let json = r#"
    {
      "protocol": "AML",
      "version": "1.0-draft",
      "active_profile": "test_profile",
      "profiles": {
        "test_profile": {
          "description": "Test Profile",
          "audio": {
            "sample_rate_hz": 48000,
            "symbol_duration_ms": 10.0,
            "guard_gap_ms": 0.0,
            "ramp_ms": { "attack": 0.1, "release": 0.1 }
          },
          "detection": {
            "method": "goertzel",
            "min_confidence_db_over_runnerup": 3.0,
            "absolute_threshold": { "type": "relative_to_noise_floor", "margin_db": 10.0 },
            "timeouts_ms": { "frame_payload": 25, "link_dead": 2000 }
          },
          "bands": {
            "preamble_hz": 2400.0,
            "type_band": { "base_hz": 3000.0, "step_hz": 50.0 },
            "note_band": { "base_hz": 200.0, "step_hz": 50.0 },
            "octave_band": { "base_hz": 1300.0, "step_hz": 80.0 },
            "value_band": { "base_hz": 4500.0, "step_hz": 50.0 },
            "msc_cmd_band": { "base_hz": 3500.0, "step_hz": 50.0 },
            "cuelist_band": { "base_hz": 4200.0, "step_hz": 50.0 }
          }
        }
      }
    }
    "#;

    let config = AmlConfig::from_json(json)?;
    let profile = config
        .get_active_profile()
        .ok_or_else(|| anyhow::anyhow!("Profile not found"))?;
    println!("Loaded profile: {}", profile.description);

    // 2. Setup Encoder
    let encoder = Encoder::new(profile.clone());

    // Create a NoteOn Frame: PREAMBLE -> NOTE_ON -> NOTE(C) -> OCTAVE(4) -> VALUE(10)
    let frame_symbols = vec![
        SymbolValue::Preamble,
        SymbolValue::Type(FrameType::NoteOn),
        SymbolValue::Note(0), // C
        SymbolValue::Octave(4),
        SymbolValue::Value(10),
    ];

    println!("Encoding symbols: {:?}", frame_symbols);
    let audio_data = encoder.encode_frame(&frame_symbols);
    println!("Generated {} audio samples.", audio_data.len());

    // 3. Setup Decoder
    let mut decoder = Decoder::new(profile.clone());
    let symbol_samples =
        (profile.audio.sample_rate_hz as f32 * profile.audio.symbol_duration_ms / 1000.0) as usize;

    // 4. Decode loop
    // In this synthetic test, we assume we are perfectly synced to the symbol boundaries.
    // In real usage, the decoder would need to hunt for the Preamble.
    let mut offset = 0;
    let mut final_frame = None;

    println!("Decoding stream...");

    while offset + symbol_samples <= audio_data.len() {
        let slice = &audio_data[offset..offset + symbol_samples];

        // 1. Get candidates based on current state
        let candidates = decoder.get_scan_candidates();

        // 2. Run detection
        if let Some((symbol, power)) = decoder.detect_symbol(slice, &candidates) {
            println!(
                "  [Offset {:04}] Detected {:?} (Power: {:.2})",
                offset, symbol, power
            );

            // 3. Update State Machine
            if let Some(decoded_frame) = decoder.handle_symbol(symbol) {
                println!("  *** FRAME COMPLETED ***");
                final_frame = Some(decoded_frame);
            }
        } else {
            println!("  [Offset {:04}] No clear symbol detected.", offset);
            // If we miss a symbol in a strictly timed sequence, the state machine might get stuck or reset
        }

        offset += symbol_samples;
    }

    // 5. Verify results
    if let Some(frame) = final_frame {
        println!("Test Passed!");
        println!("Decoded Frame Type: {:?}", frame.frame_type);
        println!("Payload: {:?}", frame.payload);

        // Assertions
        assert_eq!(frame.frame_type, FrameType::NoteOn);
        assert_eq!(frame.payload[0], SymbolValue::Note(0));
        assert_eq!(frame.payload[1], SymbolValue::Octave(4));
        assert_eq!(frame.payload[2], SymbolValue::Value(10));
    } else {
        println!("Test Failed: No complete frame decoded.");
        anyhow::bail!("Test failed");
    }

    Ok(())
}

fn run_encoder(_midi_in_index: Option<usize>, audio_out_index: Option<usize>) -> Result<()> {
    println!("Initializing Encoder...");

    // 1. Setup Profile
    let mut profile = ProfileDefinition::default_pro_clean();
    let sample_rate = profile.audio.sample_rate_hz;

    // 2. Setup RingBuffer: Main Thread -> Audio Thread
    let ring = HeapRb::<f32>::new(sample_rate as usize * 2); // 2 seconds buffer
    let (mut producer, mut consumer) = ring.split();

    // 3. Start Audio Stream
    let (stream, stream_sample_rate) =
        audio::setup_output_stream(audio_out_index, move |data: &mut [f32]| {
            for sample in data.iter_mut() {
                *sample = consumer.pop().unwrap_or(0.0);
            }
        })?;

    if stream_sample_rate != sample_rate {
        eprintln!(
            "WARNING: Audio Device running at {}Hz, Profile expects {}Hz. Adapting profile...",
            stream_sample_rate, sample_rate
        );
        profile.audio.sample_rate_hz = stream_sample_rate;
    }

    stream.play()?;
    println!("Encoder running. Press Ctrl+C to stop.");
    println!("Simulating MIDI events (random notes)...");

    let encoder = Encoder::new(profile);
    let mut note_idx = 0;

    loop {
        // Generate a NoteOn frame
        // Simple C major scale walk
        let scale = [0, 2, 4, 5, 7, 9, 11, 12];
        let note_offset = scale[note_idx % scale.len()];
        let base_note = 60; // C4
        let note = base_note + note_offset;

        let frame_symbols = vec![
            SymbolValue::Preamble,
            SymbolValue::Type(FrameType::NoteOn),
            SymbolValue::Note((note % 12) as u8),
            SymbolValue::Octave((note / 12) as u8),
            SymbolValue::Value(12), // Velocity
        ];

        println!("Sending Note: {} (Index: {})", note, note_idx);
        let samples = encoder.encode_frame(&frame_symbols);

        // Push samples to ring buffer
        for sample in samples {
            // Spin-wait if buffer is full to avoid dropping audio
            while producer.is_full() {
                thread::sleep(Duration::from_millis(1));
            }
            let _ = producer.push(sample);
        }

        // Wait before next note
        thread::sleep(Duration::from_millis(1000));
        note_idx += 1;
    }
}
