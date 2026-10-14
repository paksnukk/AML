# Changelog

## 1.1.0 (unreleased)

- **Breaking:** the project is renamed from AML (AudioMIDI Link) to Cricket (Cricket MIDI). Crates are now `cricket_core`, `cricket_engine`, `cricket_cli`, and `cricket_gui`; the CLI binary is `cricket`; the app identifier is `se.dpmstudios.cricket`. The protocol keeps the name AML (AudioMIDI Link), with Cricket as its reference implementation; its first official version is AML v1 (spec: `docs/aml-v1-spec.md`), with wire version nibble `1` in the packet header, so it does not interoperate with earlier AML builds. The legacy prototype phy module is now `phy_legacy`. GUI settings live in a new `cricket` config directory; settings from AML builds are not migrated.
- MIDI/AML decoding tolerates a wide input level range (−30 dB to 0 dB relative to transmit level) via level-independent tone detection and a silence floor.
- Link loss and panic use one fail-safe path: Sustain off and All Notes Off are sent once, without duplicate bursts.
- Repeated on-air copies of the same packet are suppressed within a short time window; dedup state resets when the link is declared dead.
- False preamble locks and missed body symbols resync without discarding already-received audio.
- TX prioritizes panic and note-offs over new notes. Each released note's note-off is repeated up to 6 times in idle slots, so a lost note-off does not leave the note hanging; heartbeats are sent only when fully idle.
- A note released and struck again quickly (a trill or roll) stays in order, so it ends released. Panic discards note-ons that are still queued.
- Sustained fast traffic keeps queued note-ons under a bounded latency; queued note-ons beyond that budget may be dropped, but note-offs and panic are not, and a chord of up to 4 notes is never dropped.
- If the audio input device stops delivering samples while the link is up, All Notes Off is sent and an audio-input-stalled alarm is raised.
- Selected audio and MIDI devices are remembered by name across restarts and re-plugging.
- The UI unlocks controls when an engine worker stops and allows “none” for optional MIDI in/out.
- `aml_cli loopback` and the GUI self-test report measured MIDI-in to MIDI-out note-on latency (mean, 95th percentile, and jitter) while playing and releasing notes, instead of a theoretical airtime. The self-test no longer blocks the UI.
- While encoding, the UI shows how many messages are queued and how late the oldest one already is (its wait plus the audio buffered ahead of it). A panic button sends All Notes Off. Profile hints say when to use Fast and when to use Robust.
- Continuous integration runs `cargo test`, `clippy`, and `fmt` on GitHub Actions.
- `aml_core` builds as `no_std` + `alloc` (with `--no-default-features`) so the modem can run on microcontrollers such as ESP32-S3, RP2350, and STM32. WAV file I/O and the legacy V1 phy stay behind the default `std` feature. CI checks the build for `thumbv8m.main-none-eabihf`.
