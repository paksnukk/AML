# AML v1 Specification (Bitstream Modem)

## 1. Overview

AML (AudioMIDI Link) is the protocol; Cricket is its reference implementation.

AML v1 transports MIDI 1.0 short messages (all channels) and MIDI Show Control over an audio path using an
**MFSK-16 bitstream modem** with framed packets, CRC-16 integrity, packet
repetition, and fail-safe link monitoring.

AML v1 is the first official protocol version and the default
physical/link stack. The earlier prototype phy (semantic tone symbols, see
`aml-prototype-protocol.md`) remains available as a legacy debug phy only.

## 2. Layering

| Layer | Responsibility |
|-------|----------------|
| Transport | Message types, MIDI and MSC mapping, TX scheduling, heartbeat / fail-safe policy |
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
[1]     SEQ         = 0..3 (2-bit counter, wrapping 0, 1, 2, 3, 0, …);
                      bits 7..2 are reserved and MUST be 0
[2..]   PAYLOAD     (length implied by MSG_TYPE, §5)
[…]     CRC16_HI
[…]     CRC16_LO
```

Payload lengths by type:

| MSG_TYPE | Payload bytes |
|----------|---------------|
| Midi (0x0) | 3 (`status`, `data1`, `data2`) |
| Msc (0x3) | 3 (compact MSC, §5.4) |
| Heartbeat (0x4) | 0 |
| Panic (0x5) | 0 |

Type values 0x1, 0x2 and 0x6–0xF are reserved; a packet with a reserved type, a non-zero reserved
SEQ bit or a non-canonical payload (§5.1) MUST be rejected like a CRC failure, even when its CRC is
valid. The largest packet (Midi, Msc) is 7 bytes; shorter packets are zero-padded to the body length
(§4.3, FEC: `docs/proposals/fec.md`), and every body has the same length whatever the type.

SEQ only has to tell consecutive packets apart for duplicate suppression (§4.4): message `n` and
`n + 4` are always more than one dedup window apart, because messages are sent one after another and
the window is shorter than two messages. The six reserved bits are part of the structure a random
or corrupted block must match (`docs/proposals/fec.md` §9.7).

CRC-16/CCITT (poly `0x1021`, init `0xFFFF`, refin=false, refout=false, xorout=`0x0000`)
covers all bytes from `HDR` through end of `PAYLOAD` (excludes CRC itself).

### 4.2 Nibble mapping

Each byte becomes two MFSK symbols (high nibble first):

```
byte b → symbols [b >> 4, b & 0x0F]
```

### 4.3 Interleaving

Block interleave with depth **4** over the packet nibble stream
(column-major write, row-major read). The packet nibbles are first padded with `0x0` to the fixed
body length of **16** nibbles.

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
PREAMBLE(4) || INTERLEAVE(PAD16(NIBBLES(packet_bytes)))
```

Every body is 16 nibbles: one repetition is 4 + 16 = 20 symbols ≈ 130 ms @ 6.5 ms/symbol.
Latency to first good copy is one repetition; K=2 improves delivery under burst loss.

## 5. Transport messages

| MSG_TYPE | Name | Payload |
|----------|------|---------|
| 0x0 | Midi | One MIDI 1.0 short message: `status`, `data1`, `data2` (§5.1) |
| 0x3 | Msc | MIDI Show Control command, compact form (§5.4) |
| 0x4 | Heartbeat | empty |
| 0x5 | Panic | empty |

### 5.1 Midi payload and validation

The payload is the MIDI message itself, without running status, with unused bytes zero:

| Status | Message | Data bytes |
|--------|---------|------------|
| `0x8n` | Note Off (with release velocity) | note, velocity |
| `0x9n` | Note On, velocity 1–127 | note, velocity |
| `0xAn` | Polyphonic key pressure | note, pressure |
| `0xBn` | Control Change, incl. channel mode messages (controllers 120–127) | controller, value |
| `0xCn` | Program Change | program, `0` |
| `0xDn` | Channel pressure | pressure, `0` |
| `0xEn` | Pitch Bend | LSB, MSB |
| `0xF2` | Song Position Pointer | LSB, MSB |
| `0xF3` | Song Select | song, `0` |
| `0xF6` | Tune Request | `0`, `0` |

`n` is the MIDI channel (0–15 = channels 1–16); all 16 channels are carried. A payload is
**canonical** if its status byte is in the table, its data bytes are `< 0x80`, its unused bytes are
`0`, and it is not a Note On with velocity 0. A transmitter MUST send canonical payloads only: a Note
On with velocity 0 is sent as Note Off with velocity 64 (`0x40`). A receiver MUST reject a packet
whose payload is not canonical (§4.1).

Not carried, by design: System Exclusive other than MSC (§5.4), MIDI Time Code quarter frames,
and all System Real-Time messages (Timing Clock, Start, Continue, Stop, Active Sensing, Reset).
Their value depends on timing: a clock or timecode stream needs a steady rate with sub-millisecond
jitter, and a packet takes 22–270 ms of airtime depending on the profile, so these streams would
both arrive useless and delay every other message. A transmitter drops them at its MIDI input.

### 5.2 MIDI mapping

The receiver outputs each Midi payload as the MIDI message it holds (2 or 3 bytes per the table in
§5.1; `0xF6` alone), on its channel. Msc is output as an MSC SysEx (§5.4); Heartbeat has no MIDI
output; Panic outputs the fail-safe burst (§5.3).

### 5.3 Fail-safe

- Decoder marks link **dead** if no CRC-valid packet for **`link_dead_ms`** (default **2000 ms**)
  after the first valid packet. This **sample-clock** link-dead detector is timed in received
  samples: it fires at sample `end of last valid packet + link_dead_ms`, independent of how audio
  is chunked, and is reported in the processing call whose input contains that sample. It fires
  once per event; the next valid packet re-arms it. Link state reads **Dead** from that moment
  until the next valid packet.
- **Wall-clock input watchdog** (engine RX, distinct from sample-clock link-dead): if the capture
  device delivers no audio samples for **`link_dead_ms`** while the link has been alive (at least
  one valid packet received and sample-clock link-dead not yet fired), the worker emits the
  fail-safe burst exactly once (when the modem still considers the link alive), raises an
  **audio-input-stalled** alarm, and disarms sample-clock link-dead for that outage. When samples
  resume, the stall alarm clears; link state stays **Dead** until the next valid packet, as usual.
  Sample-clock link-dead is unchanged when samples keep arriving but packets do not.
- On link-dead (either detector), or on Panic: emit the **fail-safe burst** exactly once. The
  demodulator is already back in search after each packet / during silence; its buffer is not
  flushed. Link-dead also clears the dedup history (§4.4). A Panic is a valid packet, so a link
  that goes silent after it later produces its own link-dead burst.
- Bad-CRC / incomplete frames are discarded; return to search
- Idle **Heartbeat** timing and keepalive airtime are defined by the TX scheduler (§5.5).

**Fail-safe burst.** For each MIDI channel 1–16 in order: Sustain off (`0xBn 0x40 0x00`), then
All Notes Off (`0xBn 0x7B 0x00`), then — only on channels whose last Pitch Bend output by this
receiver was not center and not followed by Reset All Controllers — Pitch Bend center
(`0xEn 0x00 0x40`). That is 32–48 short messages (~31–46 ms on a 31.25 kbit/s MIDI cable).

- *All 16 channels*, not only channels seen since start: the receiver may have restarted while
  notes were sounding, and a stateless burst cannot miss a channel. It is the usual "MIDI panic"
  convention and harmless on channels without notes.
- *Sustain off first*, because All Notes Off does not release notes held by the pedal.
- *Pitch Bend center* only where this receiver left a bend: a pitch wheel springs back to center, so
  a lost "back to center" would detune every later note; other controllers have no rest position
  and are left alone.
- *Not sent:* Reset All Controllers (CC121; it also resets expression, modulation and pressure
  that a show may rely on, and its exact effect varies between devices), All Sound Off (CC120;
  it cuts release tails), and Note Offs per note.

### 5.4 MIDI Show Control

MSC 1.0 messages are SysEx: `F0 7F <device_id> 02 <command_format> <command> <data> F7`, where cue
numbers, lists and paths are ASCII digits with an optional `.` (`"14.5"` = `31 34 2E 35`), separated
by `00` (`<cue> 00 <list> 00 <path>`).

AML carries the **command and its data only**. The device ID and command format are **receiver
settings** (default device ID **`0x7F`** all-call, so every MSC device responds; default command format
**`0x01`** Lighting (General), the format lighting consoles listen for). A transmitter accepts MSC from
any device ID and in any command format.

**Compact form** (payload bytes `b0 b1 b2`, big-endian 24 bits):

```
bits 23..19  command   MSC command byte (supported commands only, table below)
bits 18..15  list      0 = no cue list; 1–15 = cue list 1–15
bits 14..0   value     cue commands: 0 = no cue; v = 1–10000 → cue (v − 1) / 10  (0.0–999.9)
                       FIRE: macro number 0–127
                       other commands: 0
```

| Command (byte) | Data | Compact |
|---|---|---|
| GO (01), STOP (02), RESUME (03), GO_OFF (0B), GO/JAM_CLOCK (10) | `[cue [00 list]]` | cue optional; list only with a cue |
| LOAD (05) | `cue [00 list]` | cue required; list optional |
| FIRE (07) | macro number | value = macro, list 0 |
| ALL_OFF (08), RESTORE (09), RESET (0A) | none | list 0, value 0 |
| STANDBY_+ (11), STANDBY_- (12), SEQUENCE_+ (13), SEQUENCE_- (14), START_CLOCK (15), STOP_CLOCK (16), ZERO_CLOCK (17), MTC_CHASE_ON (19), MTC_CHASE_OFF (1A) | `[list]` | list optional, value 0 |
| OPEN_CUE_LIST (1B), CLOSE_CUE_LIST (1C) | `list` | list required, value 0 |

A compact payload is **canonical** only as in this table (a receiver rejects anything else, §4.1).

**Numbers.** Cue and list numbers are carried as numbers: `"05"`, `"5.0"`, `"5."` and `"5"` are the
same cue and are output as `"5"`; `"12.50"` is output as `"12.5"`. Cues 0–999.9 with at most one
decimal (after dropping trailing zeros) and lists 1–15 are carried.

**Not carried** (the transmitter drops the message and reports it, e.g. "MSC cue 12.345 not
representable"; it never sends an approximation): cues with more than one decimal or above 999.9,
lists 0 or above 15, lists with decimals, cue paths, TIMED_GO, SET, SET_CLOCK, OPEN_CUE_PATH,
CLOSE_CUE_PATH, and malformed MSC. Other SysEx is ignored silently (§5.1).

**Output.** The receiver outputs `F0 7F <device_id> 02 <command_format> <command> <data> F7` with its
configured device ID and command format; `<data>` is the cue in ASCII without trailing zeros (`14`,
`14.5`, `0.5`), then `00` and the list in ASCII if there is one; the list alone for list commands;
the macro byte for FIRE; nothing otherwise.

### 5.5 TX scheduler

The encoder schedules transport messages before modem encoding. **Heartbeat** is generated by the
scheduler when due; it is not queued with user traffic.

**Priority** (next message to encode):

| Order | Messages |
|-------|----------|
| 1 | Panic; channel-mode messages that end notes: CC 120 (All Sound Off), 123 (All Notes Off), 124–127 (Omni Off/On, Mono, Poly) |
| 2 | Note Off (any channel) and MSC, in arrival order |
| 3 | Everything else: Note On, Program Change, Control Change, Pitch Bend, pressure, system common |
| 4 | NoteOff refresh (idle keepalive; see below) |
| 5 | Heartbeat |

Messages of order 1 and order 2 are never coalesced or dropped.

**Order within tier 3.** *Continuous* messages — Control Change except Bank Select (CC 0, 32), Data
Entry (6, 38), Data Increment/Decrement (96, 97), NRPN/RPN select (98–101) and the channel mode
messages (120–127); Pitch Bend; channel pressure; polyphonic key pressure — are latest-value-wins.
All other tier-3 messages are *discrete* and keep their arrival order. The next tier-3 message is:
the oldest discrete message, unless a continuous message **of its channel** was queued before it,
in which case that continuous message goes first; with no discrete message queued, the oldest
continuous message. A note therefore waits for at most the controller values of its own channel that
were set before it (it is struck with them), never for those of other channels. System common
messages have no channel and keep their place relative to everything.

**Coalescing (latest value wins).** A continuous message replaces, in place, the newest queued
continuous message with the same channel, kind and controller (or note, for polyphonic pressure),
unless a discrete message of the same channel or a system common message is queued after it, or
Note Offs are held behind it (below); then it is queued as a new message. So a controller value set
before a Note On stays before it and the newer value follows it; Bank Select, RPN/NRPN and Data Entry
are never merged or reordered, and stay in order with the Program Change after them. Program Change
is never coalesced: each one can be a deliberate action (a scene or cue on the receiving device), and
a skipped program can be audible. A fader streaming faster than the link carries thus costs one queued
message, and its final value goes out within the message in flight plus one.

**Held NoteOff.** A Note Off that arrives while a Note On for the same channel and note is still
queued is held with the latest such Note On and sent immediately after it, so the note is not
released before it is struck; this also keeps off / on / off on a note that is already sounding (a
trill or roll) in order. Likewise a Note Off that arrives while a pedal controller of its channel —
Sustain (CC 64), Sostenuto (66) or Hold 2 (69) — is queued is held behind the latest such pedal
message: pedal down then key up must not become key up then pedal down (the note would stop instead
of ringing). A pedal message with held Note Offs is not coalesced.

**Panic and channel mode.** Panic removes queued Note Ons on every channel; a Panic-class channel
mode message (order 1) removes queued Note Ons of its channel (they would otherwise sound after it).
Reset All Controllers (CC 121) and Local Control (CC 122) are discrete tier-3 messages: moving them
ahead of queued controller values would change the resulting state.

**Heartbeat:** every **500 ms** only when fully idle: no message queued, no NoteOff held, and the
NoteOff refresh list is empty. While user messages or refreshes are pending, Heartbeat uses no
airtime.

**NoteOff refresh:** after a user Note Off is transmitted, it is repeated (same channel, note and
velocity) up to **6** times within **10 s** (max **32** most recent releases). When nothing higher in
the table is pending, each free packet sends a **Note Off** for one refresh entry instead of waiting
for Heartbeat: the note with the **fewest** refresh transmissions so far, then the **oldest**
original release. A **Note On** for that channel and note removes it from the refresh list. Refresh
never runs ahead of a queued message. Each release therefore costs a bounded burst of airtime (6
messages: about 0.3 s on Fast, 1.6 s on Robust) that a Note On arriving meanwhile may wait behind for
up to one message.

**Queue bounds:** queued **Note On** airtime is capped at **250 ms** (one on-wire message period per
queued Note On, plus one per Note Off held with it). Before accepting another Note On, the oldest
queued Note On is dropped until the budget fits, but at least **4** queued Note Ons are always kept,
so a chord survives even when one message is longer than the budget (Robust). A Note Off held with a
dropped Note On is dropped too, unless the note is still sounding from an earlier strike. A Note Off
for a note already on the air, MSC, and Panic-class messages are never dropped. Continuous messages
are bounded by coalescing.

**Audio output ring:** sized to about **two** on-wire messages. The next message is chosen and
encoded only when buffered output has drained below **10 ms**, so a Note On that arrives while a
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
