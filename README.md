# AML — AudioMIDI Link

> **MIDI over a cable that was only ever meant to carry sound.**

You have a Dante network, a MADI snake, or a long analog XLR run between front of house and the stage.
You need MIDI or show-control cues over there. The audio path reaches it; the MIDI cable does not.

AML turns MIDI into a short burst of tones, sends it down any spare audio channel, and turns the tones back
into MIDI on the far end. It sounds a bit like a dial-up modem having a very polite conversation.

```
 ┌───────────┐  MIDI   ┌─────────┐  ♪ audio ♪  ┌──────────────────┐  ♪ audio ♪  ┌─────────┐  MIDI   ┌──────────┐
 │ keyboard, │ ──────▶ │ AML     │ ──────────▶ │ Dante / MADI /   │ ──────────▶ │ AML     │ ──────▶ │ synth,   │
 │ show ctrl │         │ encoder │             │ analog XLR / ... │             │ decoder │         │ lights … │
 └───────────┘         └─────────┘             └──────────────────┘             └─────────┘         └──────────┘
```

## How it works

Each MIDI event becomes a small **packet**: a preamble, a header, a sequence number, the payload and a CRC16.
The packet is sent as **16-tone FSK**: one pure tone at a time, and each tone carries 4 bits.

- **Level doesn't matter.** Tones are detected by their purity (Goertzel power relative to the window's
  energy), not by their loudness. Anything from −30 dB to 0 dB relative to the transmit level decodes,
  so someone riding the fader doesn't break the link.
- **Packets find their own start.** A preamble marks every packet. After a false start or a missed
  symbol, the decoder rewinds and locks on again without throwing away audio it has already received.
- **Copies are counted once.** Repeated copies of a packet are dropped by sequence number within a short
  time window.
- **Nothing is left hanging.** A heartbeat keeps the link alive. If no valid packet arrives for 2 s, or the
  audio input stops delivering samples, the decoder sends **Sustain off + All Notes Off** once. Each
  released note's NoteOff is repeated a few times in idle airtime, so a NoteOff lost to noise
  doesn't leave a note droning forever.
- **Important messages first.** The transmitter sends Panic, then NoteOffs, then NoteOns, Program
  Changes and MSC, then NoteOff repeats, then heartbeats. When fast playing would overflow the airtime,
  it drops the oldest queued NoteOns, never a NoteOff or Panic, and never a chord of up to 4 notes.

Supported messages: NoteOn, NoteOff, Program Change, MSC (Go / Stop / Resume / Load), Panic.

## Profiles

| | **Robust** (default) | **Fast** |
|---|---|---|
| Use it for | Cues and program changes over chains you don't fully trust | Playing notes over a clean, dedicated line (no EQ, compression or limiter) |
| Symbol | 6 ms + 0.5 ms guard | 3 ms |
| Tones | 16 × 100 Hz steps from 2.5 kHz | 16 × 200 Hz steps from 2.5 kHz |
| Copies per message | 2 | 1 |
| Comfortable rate | ~3–4 messages/s | ~10 messages/s |
| Measured NoteOn latency\* | mean ~345 ms, p95 ~480 ms | mean ~80 ms, p95 ~115 ms |

\* MIDI in → MIDI out through the engine, in-process (`aml_cli loopback`), while notes are being played *and
released*. Real audio interfaces add their own buffering on top. Run the measurement yourself rather than
trusting this table.

## Quick start

You need a Rust toolchain. On Linux you also need ALSA headers (`libasound2-dev`). The GUI additionally needs
Node.js and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
# What audio and MIDI devices are there?
cargo run -p aml_cli -- list-devices

# Sending side: MIDI in → tones out (by index or by exact name)
cargo run -p aml_cli -- encode --midi-in-name "My Keyboard" --audio-out-name "Dante TX 1-2"

# Receiving side: tones in → MIDI out
cargo run -p aml_cli -- decode --audio-in-name "Dante RX 1-2" --midi-out-name "IAC Bus 1"

# No hardware at hand? Encoder plays a C-major scale on its own
cargo run -p aml_cli -- encode --simulate

# How fast is it, really? (both profiles, no hardware needed)
cargo run --release -p aml_cli -- loopback
```

Devices are remembered **by name**, so unplugging and replugging an interface doesn't silently pick another
one. A name that isn't present is an error.

### The GUI

```sh
cd aml_gui
npm install
npm run tauri dev
```

The GUI has the encoder and decoder side by side. It shows live link stats (state, confidence, CRC pass
rate, sync losses), the TX queue and its latency, alarms for missing or stalled devices, a big red
**Panic** button, and a **Self-Test** that measures real loopback latency for both profiles.

## Project layout

| Crate | What lives there |
|---|---|
| [`aml_core`](aml_core) | The modem: tone synthesis and detection, packets, CRC, interleaving, TX scheduler, fail-safe. Pure Rust, no I/O. |
| [`aml_engine`](aml_engine) | The real-time part: audio/MIDI devices, encoder and decoder workers, events, loopback latency measurement. |
| [`aml_cli`](aml_cli) | Command-line front end. |
| [`aml_gui`](aml_gui) | Tauri 2 desktop app (TypeScript + Rust). |

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The tests don't need hardware. A channel simulator in [`aml_core/tests/support`](aml_core/tests/support)
puts packets through offsets, odd chunk sizes, gain changes, noise, clock drift, low-pass filtering and
random symbol corruption. In [`stuck_notes.rs`](aml_core/tests/stuck_notes.rs), a thousand NoteOn/NoteOff
pairs go through a corrupted channel, and every note must be released. CI runs all of the above on
every push.

## Docs

- [`docs/aml-v2-spec.md`](docs/aml-v2-spec.md): the protocol and physical layer as implemented.
- [`docs/v1.1-plan.md`](docs/v1.1-plan.md): what v1.1 set out to fix, and how.
- [`CHANGELOG.md`](CHANGELOG.md): what changed.

## Status

v1.1 is unreleased. Everything above is tested in software and through the channel simulator, but **not yet
on a real show chain**. Use it as a backup line before you trust it with the main cue. Field reports are
very welcome, especially your chain (Dante? analog? any processing?) and what the decoder's link stats
said.
