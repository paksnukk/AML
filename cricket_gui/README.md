# Cricket — desktop app

Cricket MIDI desktop app: carries MIDI / MSC-lite control over a **dedicated audio channel**
(XLR, Dante, MADI, virtual cable), similar in operational mindset to LTC.

## Requirements

- Rust toolchain + Cargo
- Node.js 18+
- macOS (primary release target)

## Run (dev)

```bash
cd cricket_gui
npm install
npm run tauri dev
```

## Build (release)

```bash
cd cricket_gui
npm install
npm run tauri build
```

Artifacts land under `cricket_gui/src-tauri/target/release/bundle/`.

## How to route (like LTC)

1. Patch a **dedicated** mono bus from the TX machine/console to the RX machine.
2. Do **not** put EQ, compression, limiting, or noise reduction on that bus.
3. Set gain once; leave it alone during the show.
4. In Cricket: pick Audio Out + MIDI In on the encoder side; Audio In + MIDI Out on the decoder side.
5. Prefer **Robust** profile unless the chain is known-clean — then **Fast** (~60 ms).

## Profiles

| Profile | When | Est. first-copy |
|---------|------|-----------------|
| Robust | Default / unknown chain | ~136 ms |
| Fast | Clean dedicated chain only | ~60 ms |

## Self-test

Use **Self-Test** in the UI (offline encode→decode in process) before going on air.

## CLI (lab)

```bash
cargo run -p cricket_cli -- test
cargo run -p cricket_cli -- loopback
cargo run -p cricket_cli -- list-devices
```

## Docs

- [AML v1 spec](../docs/aml-v1-spec.md)
- [Release / field checklist](../docs/release-checklist.md)
