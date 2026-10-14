# Cricket v1.0 Release / Field Checklist (pro-clean)

Operational model: **dedicated channel like LTC** — no EQ/comp/limiter on the Cricket bus.

## Pre-release (automated)

- [ ] `cargo test -p cricket_core` green (includes Fast latency budget ≤ 60 ms)
- [ ] `cargo run -p cricket_cli -- test` passes
- [ ] `cargo run -p cricket_cli -- loopback` reports stable latency
- [ ] GUI **Self-Test** passes for Robust and Fast
- [ ] `npm run tauri build` produces macOS app/bundle

## Pro-clean field gate

| Test | Pass criteria |
|------|----------------|
| Dante/XLR/virtual 1:1 | ≥ 30 min continuous without false Note Ons |
| Level offset ±12 dB | Notes still decode; no stuck notes |
| Sample-rate change | App adapts or warns clearly in log |
| Audio dropout > 2 s | Link Dead + All Notes Off on MIDI out |
| Resync after dropout | Valid packets within < 1 s after signal returns |
| Fast on clean chain | Subjectively usable latency; no CRC storm |
| Robust on same chain | Works as fallback if Fast flaps |

## Operator checks in UI

- [ ] LTC-style banner visible
- [ ] MIDI in → audio out (encoder) and audio in → MIDI out (decoder)
- [ ] Heartbeat updates “Link alive” without flooding last-event display
- [ ] Link Dead shows alarm banner
- [ ] Buffer drops counter reacts if audio thread is starved (optional stress)

## Version

- App version **1.0.0** (`cricket_gui` package + `tauri.conf.json`)
- Git tag: `v1.0.0` when field gate is signed off
