# Cricket

> **Cricket MIDI: MIDI over a cable that was only ever meant to carry sound.**

You have a Dante network, a MADI snake, or a long analog XLR run between front of house and the stage.
You need MIDI or show-control cues over there. The audio path reaches it; the MIDI cable does not.

Cricket turns MIDI into a short burst of tones, sends it down any spare audio channel, and turns the tones back
into MIDI on the far end. It sounds a bit like a dial-up modem having a very polite conversation.
Like its namesake, it keeps chirping steadily even when there's nothing to say; if the far end suddenly hears
nothing but, well, crickets, it sends All Notes Off so no note is left droning.

Cricket speaks **AML v1** (AudioMIDI Link), an open protocol for MIDI over audio, specified in
[`docs/aml-v1-spec.md`](docs/aml-v1-spec.md). Anything else that speaks AML v1 can talk to it.

```
 ┌───────────┐  MIDI   ┌─────────┐  ♪ audio ♪  ┌──────────────────┐  ♪ audio ♪  ┌─────────┐  MIDI   ┌──────────┐
 │ keyboard, │ ──────▶ │ Cricket │ ──────────▶ │ Dante / MADI /   │ ──────────▶ │ Cricket │ ──────▶ │ synth,   │
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

\* MIDI in → MIDI out through the engine, in-process (`cricket loopback`), while notes are being played *and
released*. Real audio interfaces add their own buffering on top. Run the measurement yourself rather than
trusting this table.

### Experimental profiles

More profiles are in the code for evaluation. They are **not part of AML v1**, so both ends must pick the
same one, and they may change or go away. Select one with `--profile` (e.g. `cricket --profile ltc decode …`).

| Profile | What it is | Measured NoteOn latency\* | Needs |
|---|---|---|---|
| `ltc` | LTC-style biphase-mark code at 4800 bit/s (also `ltc2400`, `ltc4800`, `ltc9600`) | mean ~22 ms | A dedicated line: ≥ 4 dB SNR, no EQ, compression or limiter, no dropouts |
| `ltc_fec` | `ltc` with a Reed–Solomon coded body (also `ltc2400_fec`, `ltc9600_fec`); survives 1–3 ms dropouts and click bursts | mean ~26 ms | As `ltc`, but tolerates short dropouts and impulse noise |
| `robust_fec` | Robust tones with a Reed–Solomon coded body sent once instead of twice | mean ~265 ms (Robust: ~345 ms) | Same chains as Robust |

The details and measurements are in [docs/proposals/ltc-bmc.md](docs/proposals/ltc-bmc.md) and
[docs/proposals/fec.md](docs/proposals/fec.md).

## Quick start

You need a Rust toolchain. On Linux you also need ALSA headers (`libasound2-dev`). The GUI additionally needs
Node.js and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
# What audio and MIDI devices are there?
cargo run -p cricket_cli -- list-devices

# Sending side: MIDI in → tones out (by index or by exact name)
cargo run -p cricket_cli -- encode --midi-in-name "My Keyboard" --audio-out-name "Dante TX 1-2"

# Receiving side: tones in → MIDI out
cargo run -p cricket_cli -- decode --audio-in-name "Dante RX 1-2" --midi-out-name "IAC Bus 1"

# No hardware at hand? Encoder plays a C-major scale on its own
cargo run -p cricket_cli -- encode --simulate

# How fast is it, really? (no hardware needed; default: fast,robust)
cargo run --release -p cricket_cli -- loopback
cargo run --release -p cricket_cli -- loopback --profiles robust,robust_fec,fast,ltc,ltc_fec --notes 40
```

The binary is called `cricket` (`cargo install --path cricket_cli`, or `target/release/cricket`).

Devices are remembered **by name**, so unplugging and replugging an interface doesn't silently pick another
one. A name that isn't present is an error.

### The GUI

```sh
cd cricket_gui
npm install
npm run tauri dev
```

The GUI has the encoder and decoder side by side. It shows live link stats (state, confidence, CRC pass
rate, sync losses), the TX queue and its latency, alarms for missing or stalled devices, a big red
**Panic** button, and a **Self-Test** that measures real loopback latency for both profiles.

## Project layout

| Crate | What lives there |
|---|---|
| [`cricket_core`](cricket_core) | The modem: tone synthesis and detection, packets, CRC, interleaving, TX scheduler, fail-safe. Pure Rust, `no_std` + `alloc` for microcontrollers; the default `std` feature adds WAV file I/O and the legacy prototype phy. |
| [`cricket_engine`](cricket_engine) | The real-time part: audio/MIDI devices, encoder and decoder workers, events, loopback latency measurement. |
| [`cricket_cli`](cricket_cli) | Command-line front end. |
| [`cricket_gui`](cricket_gui) | Tauri 2 desktop app (TypeScript + Rust). |

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The tests don't need hardware. A channel simulator in [`cricket_core/tests/support`](cricket_core/tests/support)
puts packets through offsets, odd chunk sizes, gain changes, noise, clock drift, low-pass filtering and
random symbol corruption. In [`stuck_notes.rs`](cricket_core/tests/stuck_notes.rs), a thousand NoteOn/NoteOff
pairs go through a corrupted channel, and every note must be released. CI runs all of the above on
every push.

## Docs

- [`docs/aml-v1-spec.md`](docs/aml-v1-spec.md): the AML v1 protocol and physical layer as implemented.
- [`docs/v1.1-plan.md`](docs/v1.1-plan.md): what v1.1 set out to fix, and how.
- [`CHANGELOG.md`](CHANGELOG.md): what changed.

## Status

v1.1 is unreleased. Everything above is tested in software and through the channel simulator, but **not yet
on a real show chain**. Use it as a backup line before you trust it with the main cue. Field reports are
very welcome, especially your chain (Dante? analog? any processing?) and what the decoder's link stats
said.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by
you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or
conditions.
