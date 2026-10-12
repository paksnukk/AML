# Changelog

## 1.1.0 (unreleased)

- MIDI/AML decoding tolerates a wide input level range (−30 dB to 0 dB relative to transmit level) via level-independent tone detection and a silence floor.
- Link loss and panic use one fail-safe path: Sustain off and All Notes Off are sent once, without duplicate bursts.
- Repeated on-air copies of the same packet are suppressed within a short time window; dedup state resets when the link is declared dead.
- False preamble locks and missed body symbols resync without discarding already-received audio.
- TX prioritizes panic and note-offs over new notes; idle traffic repeats recent note-offs instead of heartbeats when needed, with heartbeats only when fully idle.
- Sustained fast traffic keeps queued note-ons under a bounded latency; stale queued note-ons may be dropped, but note-offs and panic are not.
- If the audio input device stops delivering samples while the link is up, All Notes Off is sent and an audio-input-stalled alarm is raised.
- Selected audio and MIDI devices are remembered by name across restarts and re-plugging.
- The UI unlocks controls when an engine worker stops and allows “none” for optional MIDI in/out.
- `aml_cli loopback` and the GUI self-test report measured MIDI-in to MIDI-out latency (mean, 95th percentile, and jitter) instead of a theoretical airtime.
- While encoding, the UI shows how many messages are queued and how much audio is already buffered. A panic button sends All Notes Off. Profile hints say when to use Fast and when to use Robust.
- Continuous integration runs `cargo test`, `clippy`, and `fmt` on GitHub Actions.
