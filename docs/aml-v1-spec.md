# AML v1 Specification (Bitstream Modem)

## 1. Overview

AML (AudioMIDI Link) is the protocol; Cricket is its reference implementation.

AML v1 transports MIDI/MSC-like control events over an audio path using an
**MFSK-16 bitstream modem** with framed packets, CRC-16 integrity, packet
repetition, and fail-safe link monitoring.

AML v1 is the first official protocol version and the default
physical/link stack. The earlier prototype phy (semantic tone symbols, see
`aml-prototype-protocol.md`) remains available as a legacy debug phy only.

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
                      VERSION = 0x1 (AML v1; any other value is rejected)
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

Velocity and program use full 7-bit resolution (unlike the legacy prototype phy's 4-bit quantization).

### 5.1 MIDI mapping

| Message | MIDI bytes |
|---------|------------|
| NoteOn | `0x90, note, velocity` (velocity 0 treated as NoteOff) |
| NoteOff | `0x80, note, 0x40` |
| ProgramChange | `0xC0, program` |
| Panic / link-dead | Fail-safe burst, two messages in order: Sustain off `0xB0, 0x40, 0x00`, then All Notes Off `0xB0, 0x7B, 0x00` |
| MscLite | Application-defined in POC (logged / evented; not SysEx) |

### 5.2 Fail-safe

- Decoder marks link **dead** if no CRC-valid packet for **`link_dead_ms`** (default **2000 ms**)
  after the first valid packet. This **sample-clock** link-dead detector is timed in received
  samples: it fires at sample `end of last valid packet + link_dead_ms`, independent of how audio
  is chunked, and is reported in the processing call whose input contains that sample. It fires
  once per event; the next valid packet re-arms it. Link state reads **Dead** from that moment
  until the next valid packet.
- **Wall-clock input watchdog** (engine RX, distinct from sample-clock link-dead): if the capture
  device delivers no audio samples for **`link_dead_ms`** while the link has been alive (at least
  one valid packet received and sample-clock link-dead not yet fired), the worker emits the
  fail-safe burst (§5.1) exactly once (when the modem still considers the link alive), raises an
  **audio-input-stalled** alarm, and disarms sample-clock link-dead for that outage. When samples
  resume, the stall alarm clears; link state stays **Dead** until the next valid packet, as usual.
  Sample-clock link-dead is unchanged when samples keep arriving but packets do not.
- On link-dead (either detector), or on Panic: emit the fail-safe burst (§5.1) exactly once. The
  demodulator is already back in search after each packet / during silence; its buffer is not
  flushed. Link-dead also clears the dedup history (§4.4). A Panic is a valid packet, so a link
  that goes silent after it later produces its own link-dead burst.
- Bad-CRC / incomplete frames are discarded; return to search
- Idle **Heartbeat** timing and keepalive airtime are defined by the TX scheduler (§5.3).

### 5.3 TX scheduler

The encoder schedules transport messages before modem encoding. **Heartbeat** is generated by the
scheduler when due; it is not queued with user traffic.

**Priority** (next message to encode):

| Order | Messages |
|-------|----------|
| 1 | Panic |
| 2 | NoteOff (user queue) |
| 3 | NoteOn, ProgramChange, MscLite |
| 4 | NoteOff refresh (idle keepalive; see below) |
| 5 | Heartbeat |

**Heartbeat:** every **500 ms** only when fully idle: no Panic, NoteOff, or normal message queued,
no NoteOff parked behind a queued NoteOn, and the NoteOff refresh list is empty. While user
messages or refreshes are pending, Heartbeat uses no airtime.

**NoteOff refresh:** after a user NoteOff is transmitted, its note is repeated up to **6** times
within **10 s** (max **32** most recent releases). When nothing higher in the table is pending, each
free packet sends a **NoteOff** for one refresh entry instead of waiting for Heartbeat: the note with
the **fewest** refresh transmissions so far, then the **oldest** original release. A **NoteOn** for
that note removes it from the refresh list. Refresh never runs ahead of a **NoteOn** already in the
normal queue. Each release therefore costs a bounded burst of airtime (6 messages: about 0.3 s on
Fast, 1.6 s on Robust) that a NoteOn arriving meanwhile may wait behind for up to one message.

**Held NoteOff:** a NoteOff that arrives while a NoteOn for the same note is still queued is held
with the latest such NoteOn and sent immediately after it, so the note is not released before it
is struck. This also keeps off / on / off on a note that is already sounding (a trill or roll) in
order.

**Panic** removes NoteOns that are still queued (they would otherwise sound after the Panic).

**Queue bounds:** queued **NoteOn** airtime is capped at **250 ms** (one on-wire message period per
queued NoteOn, plus one extra period for a NoteOff held with it). Before accepting another NoteOn,
the oldest queued NoteOn is dropped until the budget fits, but at least **4** queued NoteOns are
always kept, so a chord survives even when one message is longer than the budget (Robust). A
NoteOff held with a dropped NoteOn is dropped too, unless the note is still sounding from an earlier
strike. A NoteOff for a note already on the air, and **Panic**, are never dropped.

**Audio output ring:** sized to about **two** on-wire messages. The next message is chosen and
encoded only when buffered output has drained below **10 ms**, so a NoteOn that arrives while a
message is playing waits for that message only, not for one already encoded behind it.

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

### 6.3 Throughput (airtime budgets)

First-copy duration sets a practical sustained rate on a dedicated chain: Robust (**130–140 ms**
per max-size message) supports about **3–4 messages/s**, and a chord of up to 4 notes is sent whole
(later notes wait about one message each); Fast (**54 ms**) supports higher rates,
and the TX scheduler keeps Fast **queue** latency under **250 ms** at **10 messages/s** by dropping
queued NoteOns beyond that budget. These figures are on-wire airtime budgets for a clean dedicated path, not
guarantees through EQ, dynamics, or other processing on the AML bus.

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
