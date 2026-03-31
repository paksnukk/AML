# Project Plan: AML (AudioMIDI Link) Implementation in Rust

## Overview
Build a real-time application to convert MIDI to Audio (Encoder) and Audio to MIDI (Decoder) using the AML Protocol V1.
The system is designed for low-latency, deterministic control over standard audio channels.

## Goals
- **Performance:** Low latency real-time processing.
- **Portability:** Cross-platform support (macOS, Linux), preparing for Tauri GUI.
- **Architecture:** Separation of concerns (Core Library vs. Application Layer).

## Phase 1: Architecture & Setup
- [ ] Initialize Rust Workspace (`aml_workspace`)
- [ ] Create crates:
    - `aml_core`: Logic, State Machine, DSP (Goertzel), Protocol definitions.
    - `aml_cli`: Command-line interface for real-time I/O.
- [ ] Dependency selection:
    - `cpal` (Audio I/O)
    - `midir` (MIDI I/O)
    - `hound` (WAV file handling for tests)
    - `clap` (CLI arguments)
    - `serde`/`serde_json` (Profile parsing)

## Phase 2: AML Core Implementation
### 2.1 Data Structures
- [ ] Implement `Profile` struct and JSON parsing.
- [ ] Implement `Symbol` definitions and frequency table generation.

### 2.2 Encoder (MIDI -> Audio)
- [ ] `Encoder` struct: Accepts MIDI events, buffers symbols.
- [ ] Synthesis engine: Generate sine waves with envelopes (Attack/Release) based on profile settings.
- [ ] Frame generation: Construct `PREAMBLE -> TYPE -> PAYLOAD` sequences.

### 2.3 Decoder (Audio -> MIDI)
- [ ] Implement Goertzel algorithm for efficient frequency detection.
- [ ] `Decoder` struct: Processes audio buffers, detects symbols.
- [ ] State Machine: `WAIT_PREAMBLE` -> `READ_TYPE` -> `READ_PAYLOAD` -> `EMIT`.
- [ ] Fail-safe logic: Timeouts and "Link Dead" detection.

## Phase 3: CLI & Real-time I/O
- [ ] Implement Audio Input/Output streams using `cpal`.
- [ ] Implement MIDI Input/Output connections using `midir`.
- [ ] Create a "Loopback" test mode (Encoder fed directly to Decoder).
- [ ] Real-time latency measurement and tuning.

## Phase 4: Testing & Optimization
- [ ] Unit tests for Encoder/Decoder logic (using static buffers).
- [ ] Integration tests using WAV files.
- [ ] Optimize Goertzel window size and thresholds for the "Pro Clean" profile.

## Phase 5: Future / GUI
- [ ] Set up Tauri wrapper.
- [ ] Visualize signal presence and decoder state.
- [ ] Profile editor.