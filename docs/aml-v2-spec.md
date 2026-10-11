# AML V2 Specification (Bitstream Modem)

## 1. Overview

AML V2 transports MIDI/MSC-like control events over an audio path using an
**MFSK-16 bitstream modem** with framed packets, CRC-16 integrity, packet
repetition, and fail-safe link monitoring.

V2 is the default physical/link stack. AML V1 (semantic tone symbols) remains
available as a legacy debug phy only.

## 2. Layering

| Layer | Responsibility |
|-------|----------------|
| Transport | Message types, MIDI mapping, heartbeat / All Notes Off policy |
| Link | Packet header, sequence numbers, CRC-16, repetition, interleave |
| Physical | MFSK-16 tones, preamble sync, Goertzel demodulation |

Audio I/O always uses **channel 0 only** (mono). Extra interleaved channels are ignored on input and filled with the mono signal on output.

## 3. Physical layer (Robust profile)

| Parameter | Value |
|-----------|-------|
| Sample rate | 48 000 Hz (device rate adapted at runtime) |
| Modulation | MFSK-16 (4 bits / symbol) |
| Tone base | 2500 Hz |
| Tone spacing | 100 Hz |
| Tone indices | 0..15 → 2500, 2600, …, 4000 Hz |
| Symbol duration | 6.0 ms (288 samples @ 48 kHz) |
| Guard gap | 0.5 ms silence after each symbol |
| Attack / release | 0.3 ms linear ramps |
| Amplitude | 0.8 peak |

### 3.1 Preamble

Fixed 4-symbol sequence (not CRC-covered):

```
[0xA, 0x5, 0xC, 0x3]
```

Used for packet acquisition and symbol-boundary alignment. The preamble replaces a separate link-layer sync word.

### 3.2 Detection

- Goertzel bank over the 16 tone frequencies on each symbol window
  (`N` = `symbol_samples`)
- Detection is level-independent. A symbol is accepted only if all hold:
  1. **Silence floor:** window RMS `>= min_rms_dbfs` (−60 dBFS; full-scale
     sine = −3 dBFS). Quieter windows are never detected.
  2. **Tone purity:** `purity = P_best / (E * N / 2) >= min_tone_purity`,
     where `P_best` is the best tone's Goertzel power and `E = sum(x^2)` the
     window energy. A steady sinusoid filling the window on the target
     frequency gives `P = (A N / 2)^2` and `E = A^2 N / 2`, so purity ≈ 1.0 at
     any amplitude; ramps, misalignment, other tones and noise lower it
     (white noise ≈ `2 / N` per bin). Thresholds: 0.25 robust, 0.30 fast.
  3. **Confidence:** `10 * log10(best / runner_up) >= min_confidence_db`
     (default 3.0 dB) for the first preamble symbol; `0.5 * min_confidence_db`
     for the remaining preamble symbols and body symbols.
- Measured purity of synthesized symbols (both profiles): ≈0.97 aligned
  (attack/release ramps), ≈0.91 aligned at SNR 12 dB, ≥0.57 (fast) / ≥0.80
  (robust) at ±hop/2 misalignment, ≥0.43 at ±hop; best-bin purity of white
  noise peaks at ≈0.09 (robust, N=288) / ≈0.18 (fast, N=144).
- Decodes from 0 dB down to at least −30 dB relative to the 0.8-peak
  transmit level.
- Search mode hops by `hop = symbol_samples / 4`
- **Fine alignment:** on a preamble[0] hit at window position `t`, the lock
  position is the window start in `[t − hop, t + symbol_samples]` that maximizes
  the preamble[0] tone's Goertzel power (backward range clamped to the end of the
  last decoded packet). Coarse grid of `hop / 2`, then halving refinement to one
  sample. Residual error on a clean signal: ≤ 1 sample at every start offset (both
  profiles). The receiver waits until `t + symbol_samples + period` has been
  received before deciding, so the result does not depend on input chunking.
- After preamble lock, advance by `symbol_samples + gap_samples` per symbol
- **Resync:** a failed lock attempt (preamble symbol mismatch or a body symbol
  that fails the gate) rewinds: search resumes at `t + hop`, as if the window
  that triggered the attempt had been rejected. Samples read during the attempt
  are kept until it resolves (at most one packet), so a real preamble that starts
  inside a false lock is still found.

## 4. Link layer

### 4.1 Packet bytes (compact)

```
[0]     HDR         = (VERSION << 4) | MSG_TYPE
                      VERSION = 0x2
[1]     SEQ         (0..255, wrapping)
[2..]   PAYLOAD     (length implied by MSG_TYPE)
[…]     CRC16_HI
[…]     CRC16_LO
```

Payload lengths by type:

| MSG_TYPE | Payload bytes |
|----------|---------------|
| NoteOn (0) | 2 (`note`, `velocity`) |
| NoteOff (1) | 1 (`note`) |
| ProgramChange (2) | 1 (`program`) |
| MscLite (3) | 3 (`cmd`, `cuelist`, `cue`) |
| Heartbeat (4) | 0 |
| Panic (5) | 0 |

CRC-16/CCITT (poly `0x1021`, init `0xFFFF`, refin=false, refout=false, xorout=`0x0000`)
covers all bytes from `HDR` through end of `PAYLOAD` (excludes CRC itself).

### 4.2 Nibble mapping

Each byte becomes two MFSK symbols (high nibble first):

```
byte b → symbols [b >> 4, b & 0x0F]
```

### 4.3 Interleaving

Block interleave with depth **4** over the packet nibble stream
(column-major write, row-major read). Pad with `0x0` nibbles to a multiple of depth.

### 4.4 Repetition

Each packet is transmitted **K = 2** times (full preamble + interleaved body each time).
The receiver accepts the first CRC-valid copy and suppresses later copies: a CRC-valid packet is
dropped only if its packet bytes (same `SEQ`, type and payload) equal a packet accepted within the
dedup window, measured in received samples:

```
repetition = (len(preamble) + 16 + 1) symbol periods      # preamble + body + inter-repetition gap
window     = K × repetition + repetition / 2              # Robust ≈ 341 ms, Fast ≈ 86 ms
```

A packet with the same `SEQ` but a different payload, or one arriving after the window (TX restart,
`SEQ` wrap), is delivered. The dedup history is cleared on link-dead.

### 4.5 On-wire order (one repetition)

```
PREAMBLE(4) || INTERLEAVE(NIBBLES(packet_bytes))
```

Example NoteOn body size: HDR+SEQ+2+CRC = 6 bytes → 12 nibbles → pad to 12
(already multiple of 4). One repetition ≈ 4 + 12 = 16 symbols ≈ 104 ms @ 6.5 ms/symbol.
Latency to first good copy is one repetition; K=2 improves delivery under burst loss.

## 5. Transport messages

| MSG_TYPE | Name | Payload |
|----------|------|---------|
| 0x0 | NoteOn | `note` (0–127), `velocity` (0–127) |
| 0x1 | NoteOff | `note` (0–127) |
| 0x2 | ProgramChange | `program` (0–127) |
| 0x3 | MscLite | `cmd` (0=GO,1=STOP,2=RESUME,3=LOAD), `cuelist` (0–15), `cue` (0–127) |
| 0x4 | Heartbeat | empty |
| 0x5 | Panic | empty |

Velocity and program use full 7-bit resolution (unlike V1 4-bit quantization).

### 5.1 MIDI mapping

| Message | MIDI bytes |
|---------|------------|
| NoteOn | `0x90, note, velocity` (velocity 0 treated as NoteOff) |
| NoteOff | `0x80, note, 0x40` |
| ProgramChange | `0xC0, program` |
| Panic / link-dead | Fail-safe burst, two messages in order: Sustain off `0xB0, 0x40, 0x00`, then All Notes Off `0xB0, 0x7B, 0x00` |
| MscLite | Application-defined in POC (logged / evented; not SysEx) |

### 5.2 Fail-safe

- Encoder emits Heartbeat every **500 ms** while running
- Decoder marks link **dead** if no CRC-valid packet for **2000 ms** after the first valid packet.
  This is the only link-dead detector. It is timed in received samples: it fires at sample
  `end of last valid packet + 2000 ms`, independent of how audio is chunked, and is reported in the
  processing call whose input contains that sample. It fires once per event; the next valid packet
  re-arms it. Link state reads **Dead** from that moment until the next valid packet.
- On link-dead or Panic: emit the fail-safe burst (§5.1) exactly once. The demodulator is already
  back in search after each packet / during silence; its buffer is not flushed. Link-dead also clears
  the dedup history (§4.4). A Panic is a valid packet, so a link that goes silent after it later
  produces its own link-dead burst.
- Bad-CRC / incomplete frames are discarded; return to search

## 6. Profiles

### 6.1 Robust (default)

Parameters in §3. Packet repetition K=2. First-copy on-wire ≈ **130–140 ms** for max body (20 symbols × 6.5 ms).

Use when the dedicated chain is unknown or may include converters/patchbays with mild distortion.

### 6.2 Fast

For clean dedicated chains only (LTC-like mental model: no EQ/comp/limiter on the AML bus).

| Parameter | Value |
|-----------|-------|
| Symbol duration | 3.0 ms |
| Guard gap | 0 |
| Attack / release | 0.15 ms |
| Tone spacing | 200 Hz (wider for short Goertzel windows) |
| Preamble | `[0xA, 0x5]` (2 symbols) |
| Repetition K | 1 |
| Confidence | 2.0 dB |
| Min tone purity | 0.30 |

First-copy on-wire for max body ≈ **54 ms** (2 + 16 symbols × 3 ms).

## 7. Release assumptions (v1.0)

- Dedicated mono path (analog XLR, Dante, MADI, virtual loop) treated like LTC
- Operators do not EQ/compress/limit the AML channel
- Gain staging is set once and left alone during the show
- GUI is the primary product (MIDI in/out + audio modem); CLI remains a lab tool

## 8. Observability metrics

Implementations should expose:

- Link state: `Search` | `Locked` | `Dead`
- CRC pass count / fail count / pass rate
- Sync-lost count
- Last symbol confidence (dB over runner-up)
- Last sequence number
- Estimated first-copy latency
- Audio ring-buffer drop count
