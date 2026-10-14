# Proposal: LTC-style biphase-mark phy ("ltc" profile)

Status: **phase 1: prototype, measurements, design proposal.** Not part of the AML v1 spec. The
prototype is in `cricket_core/src/phy/bmc/` and is selected with `PhyConfig::ltc()` / `--profile ltc`.
The robust and fast profiles behave exactly as before.

## 1. Motivation

On a clean, dedicated line (Dante/MADI channel, analog XLR tie line) the MFSK profiles spend most of
their airtime on a robustness that the line does not need. Fast needs 54 ms of tones per message and
Robust needs 2 × 130 ms. Audio engineers already route **LTC** over these lines and trust it. LTC is a
biphase-mark (BMC) square wave: it is self-clocking, does not care about polarity, gain staging or AC
coupling, and fits several kbit/s into an audio channel. A BMC phy that carries the existing AML packets
gives:

* **~3.4× less airtime than Fast and ~17× less than Robust** at 4800 bit/s (16.9 ms per message, see §5).
* **Measured MIDI-in → MIDI-out latency of 22 ms mean** through the engine, against 78 ms for Fast and
  343 ms for Robust.
* A receiver that costs **about 1/10 of the CPU** of the Goertzel receivers and needs **no sample buffer**.

## 2. Design

### 2.1 Line code

Biphase mark, the same code SMPTE LTC and AES3 use. Every bit cell starts with a transition. A `1` has
an extra transition in the middle of the cell and a `0` has none. The data is carried only by *when*
transitions happen, never by the level. The receiver therefore sees just two transition intervals:
**H** (half a cell, half of a `1`) and **F** (a full cell, a `0`).

### 2.2 Frame (one burst)

```
| preamble: 8 x '1' | sync: V V | body: 16 nibbles = 64 bits | postamble: '1' | tail ½ cell + fade |
```

| Part | Content | Purpose |
|---|---|---|
| Preamble | 8 `1` bits = 16 H intervals, a square wave at the bit rate | Clock acquisition (half-cell estimate), envelope settling. The RX needs ≥ 8 consecutive H intervals within ±30 % of each other, with a mean within ±5 % of nominal. |
| Sync | Two **code violations** V, each 1.5 cells (3 half cells), at opposite levels | Valid BMC data contains only H and F intervals, so `V V` can never appear inside a body, whatever the payload. Unlike the LTC sync word, it needs no reserved data pattern and no bit stuffing (AES3 preambles use violations for the same reason). It is DC-balanced and has no polarity. The edge that ends the second V marks the start of the first body bit. |
| Body | The link-layer nibbles, MSB first: exactly the 16 interleaved nibbles that MFSK sends as tones | Reuses the link layer unchanged (§3). |
| Postamble | One `1` bit | Gives the last body bit a terminating transition, so its length can be measured. |
| Tail | ½ cell at the last level, then a shaped fade to 0 | Clean end of burst. |

After each burst comes `gap_bits` = 4 cells of silence (0.8 ms at 4800 bit/s). The burst is 76.5 bit
cells, so it lasts 15.9 ms at 4800 bit/s.

### 2.3 Bit rate

All parameters scale with the bit rate. The three candidates at 48 kHz:

| bit/s | half cell | burst | signal band (fundamentals) | notes |
|---|---|---|---|---|
| 2400 | 10 samples | 31.9 ms | 1.2–2.4 kHz | LTC-like rate (30 fps LTC = 2400 bit/s); most noise margin |
| **4800** | **5 samples** | **15.9 ms** | **2.4–4.8 kHz** | **recommended default**; survives a 3.4 kHz 2-pole low-pass |
| 9600 | 2.5 samples | 8.0 ms | 4.8–9.6 kHz | digital lines only; fails with < ~6 kHz bandwidth or SNR < 12 dB |

The half cell does not have to be a whole number of samples. TX and RX work in fractional samples, and
the tests pass at 44.1 kHz (4.59 samples per half cell at 4800) and at 96 kHz.

### 2.4 Waveform shaping

Each transition is a raised-cosine step `edge_frac` = 0.5 half cells wide (52 µs at 4800 bit/s), placed
at its exact fractional sample position. This is the same idea as LTC's 25 ± 5 µs rise time: almost no
energy above ~2.5× the bit rate, so the signal passes analog stages, transformers and SRC anti-alias
filters without the aliasing or ringing a hard-edged digital square would cause. The level is
`amplitude / √2` (0.57 peak), the **same RMS as an MFSK tone of the same `amplitude`**. All SNR figures
below therefore compare like with like. The peak is 3 dB lower than MFSK's, which leaves headroom.

### 2.5 Receiver and clock recovery

```
x → DC blocker (1-pole HP, 0.05 × bit rate) → 2-pole Butterworth LP (1.5 × bit rate)
  → peak envelope (release 8 cells) → Schmitt trigger (±0.2 × envelope)
  → transition time = interpolated zero crossing that preceded the Schmitt flip
  → interval / half-cell estimate → H | F | V | invalid → bits → nibbles → RxEvent
```

* **Edge-interval classification** with thresholds at 1.5 / 2.5 / 3.5 half cells. Each interval is
  measured on its own between two transitions, so a sample-rate offset never builds up over the frame.
  Only the ratio to the estimate matters. The half-cell estimate is acquired on the preamble and tracked
  through the body (first-order loop, gain 1/16). The measured tolerance is **±1 % rate error with 100 %
  success**. The ±5 % acquisition window could be widened. A PLL is not needed for burst mode.
* **Sub-sample timing without chatter.** The Schmitt trigger decides *that* a transition happened, with
  hysteresis against noise. The linearly interpolated zero crossing before the flip decides *when*.
* **Level independence.** The hysteresis is proportional to the tracked envelope, and the decision
  uses only times. The prototype decodes from 0 dB down to −50 dB with 100 % success. A silence floor
  (`min_rms_dbfs`, −60 dBFS, as in MFSK) gates the receiver: below it, transitions are ignored and any
  body in progress is abandoned.
* **Polarity** is irrelevant by construction (100 % success inverted, also with noise).
* **DC / high-pass.** BMC is DC-free, so AC coupling and transformers are fine (HP 300 Hz and HP 1 kHz
  pass at 4800). The RX DC blocker also removes hum: 50 Hz hum at the same level as the signal passes
  at all three rates.
* The receiver emits the **same `RxEvent`s as the MFSK receiver** (`PreambleLocked`, one `BodyNibble`
  per 4 bits with its end sample, `SyncLost`, `Metrics`). `confidence_db` is a timing-quality figure:
  `−10·log10(mean squared interval error in half cells)`, capped at 40 dB. An ideal line gives 30–40 dB
  and the decision boundary is at 6 dB.

### 2.6 Idle behaviour: burst (prototyped) vs continuous (option)

| | **Burst** (prototype) | **Continuous carrier** (option) |
|---|---|---|
| Idle signal | silence + one 17 ms heartbeat burst every 500 ms (3.4 % duty) | constant BMC idle fill (`1`s), like LTC |
| Link-dead detection | heartbeat timeout (2 s, as now). With 17 ms heartbeats it could drop to ~250 ms / 750 ms at 7 % duty | **instant**: no transitions for ~1 ms = dead. The fail-safe can fire within a few ms |
| Latency | preamble (8 bits) + sync + body | the clock is always locked, so no preamble: sync + body, saving 1.7 ms at 4800 |
| Listener annoyance | a short buzz on each message; quiet when idle | permanent timecode-like buzz. Accepted on LTC lines, unpleasant if the channel is ever monitored |
| Crosstalk into neighbours | only while sending | permanent and broadband (square-wave harmonics) |
| Compressors / gates on the path | a downstream gate can chop the start of a burst; the 8-bit preamble absorbs ~1.5 ms of it | a constant level keeps compressors still and never triggers a gate |
| Fits existing AML semantics | yes: heartbeat, link-dead and fail-safe stay unchanged | needs a new link-dead source (carrier loss) next to the heartbeat |

**Recommendation:** keep burst mode as the default because it matches the existing heartbeat and
fail-safe model. Offer continuous mode later as an opt-in for show-critical lines where sub-10 ms
link-dead detection matters. Its receiver is the same one: the idle fill simply keeps the preamble run
counter satisfied.

## 3. Fit in the code and in AML

### 3.1 Code (additive)

* `PhyConfig` gained one field, `modulation: Modulation`, which is `Mfsk` or `Bmc(BmcParams)`. `robust()`
  and `fast()` set `Mfsk`, so their behaviour is unchanged. New constructors are `ltc()` (4800) and
  `ltc_with_bit_rate(n)`. `from_name` accepts `ltc`, `ltc2400`, `ltc4800` and `ltc9600`.
* `phy::PhyTx` / `phy::PhyRx` are small enums that dispatch to `ModemTx`/`ModemRx` (MFSK) or
  `BmcTx`/`BmcRx`. In `modem.rs`, `CricketModemTx`/`CricketModemRx` hold these instead of the MFSK
  types. `repetition_samples` and the new `repetition_gap_samples` know the BMC burst length. This keeps
  the TX scheduler airtime budget, the dedup window and the engine ring sizes correct without touching
  them.
* The packet format, CRC-16, SEQ/dedup, nibble interleave, repetition K, scheduler, heartbeat, link-dead
  and fail-safe are **reused unchanged**. The engine and GUI select the phy by profile name. The CLI has
  a global `--profile` option (`cricket --profile ltc encode|decode|test`) and
  `cricket loopback --profiles ltc,fast,robust`.
* Everything is `no_std` + `alloc` and builds for `thumbv8m.main-none-eabihf`.

### 3.2 Wire format and spec implications

* **No header change is needed.** The packet bytes, including version nibble `0x1`, are the same on both
  phys. The phy is a property of the *line*, the way the MFSK profile already is.
* **Telling MFSK from BMC: auto-detect is feasible and cheap.** The two signals cannot be confused. A BMC
  sync needs ≥ 8 regular H intervals followed by two 3-half-cell violations and 64 valid bits; MFSK
  needs Goertzel-pure tones on its preamble frequencies; and both are CRC-checked. A test feeds Robust
  and Fast audio to all three BMC receivers and BMC audio to both MFSK receivers, and gets **zero
  cross-decodes** (`mfsk_and_bmc_do_not_cross_decode`). The same test shows that two receivers running
  in parallel on one stream that mixes Fast and LTC bursts each get exactly their own packets. Because
  the BMC receiver adds only ~10 % to the CPU cost of an MFSK receiver, a v1.x receiver can **always run
  BMC alongside its MFSK profile** and report which phy the link is on. The operator then picks the
  profile only on the TX side. Auto-detecting the BMC *bit rate* also works: run one BMC receiver per
  rate (each about 20 ns per sample on the host), or widen the acquisition window and use the measured
  half cell.
* A profile identifier in the header would cost header bits for information the receiver already has.
  If the TX should advertise its profile (for example for GUI display), a later heartbeat payload is
  the right place.
* For the spec: a new §6.x "LTC (BMC) profile". Frame (§2.2), shaping, and RX requirements: interval
  thresholds, ±1 % rate tolerance, the silence floor, and the rule that the RX must accept either
  polarity.

### 3.3 Combining with link-layer FEC (`claude/fec`)

The BMC phy carries an **opaque nibble body** of any length. The TX sends whatever nibbles it is given,
and the RX collects `set_expected_body_nibbles(n)` nibbles. An RS(GF(16)) codeword produced by the link
layer therefore goes on the wire unchanged, at 4 bits per nibble (each extra parity nibble costs 0.83 ms
at 4800 bit/s). Points to note:

* BMC errors are of two kinds. A missed or spurious mid-cell transition gives a **single-bit substitution**
  (H+H read as F, or the reverse), and RS corrects these. A shifted transition usually gives an
  **invalid interval sequence**, and the prototype then aborts the packet. With FEC it would be better
  to mark the affected nibble as an **erasure** and resynchronise on the next unambiguous F, so that RS
  can fill it in. RS corrects twice as many erasures as unknown errors. This is the main phy change FEC
  would want.
* The nibble interleaver is still useful against impulse noise, which corrupts a short run of
  consecutive bits.
* With FEC, the default K stays 1 on BMC. FEC helps more than a second copy, which would double the
  airtime.

## 4. Measurements

Channel simulator: `cricket_core/tests/support`. New impairments are a 2-pole low-pass, 1-pole high-pass,
peaking EQ, soft clip, polarity inversion and mains hum. The existing ones are AWGN, gain, drift
(resampling) and 1-pole low-pass. SNR is relative to the RMS of the signal (equal for MFSK and BMC, see
§2.4), with white noise over the full 0–24 kHz band. Each cell is the success rate of 200 single NoteOns
with random start offsets and random input chunk sizes (64–4096); success means the exact message and
nothing else.

Reproduce with:

```sh
cargo test --release -p cricket_core --test phy_ab_measurements -- --ignored --nocapture
cargo run --release -p cricket_cli -- loopback --profiles ltc,ltc2400,ltc9600,fast,robust
```

### 4.1 Packet success (%)

| condition | robust | fast | ltc2400 | ltc4800 | ltc9600 |
|---|---|---|---|---|---|
| clean | 100 | 100 | 100 | 100 | 100 |
| SNR 20 dB | 100 | 100 | 100 | 100 | 100 |
| SNR 15 dB | 100 | 100 | 100 | 100 | 100 |
| SNR 12 dB | 100 | 100 | 100 | 100 | 100 |
| SNR 10 dB | 100 | 100 | 100 | 100 | 94.0 |
| SNR 8 dB | 100 | 100 | 100 | 99.5 | 70.0 |
| SNR 6 dB | 100 | 100 | 99.5 | 86.5 | 22.5 |
| SNR 4 dB | 100 | 100 | 96.0 | 43.5 | 0 |
| SNR 2 dB | 100 | 99.5 | 72.5 | 5.0 | 0 |
| SNR 0 dB | 100 | 94.0 | 23.0 | 0 | 0 |
| level −10 / −30 / −50 dB | 100 | 100 | 100 | 100 | 100 |
| level −30 dB + SNR 20 | 100 | 100 | 100 | 100 | 100 |
| polarity inverted | 100 | 100 | 100 | 100 | 100 |
| inverted + SNR 12 | 100 | 100 | 100 | 100 | 100 |
| drift ±1000 ppm (±0.1 %) | 100 | 100 | 100 | 100 | 100 |
| drift +1000 ppm + SNR 12 | 100 | 100 | 100 | 100 | 99.0 |
| drift +1 % | 0 | 65.5 | 100 | 100 | 100 |
| drift −1 % | 0 | 73.5 | 100 | 100 | 100 |
| 1-pole LP 15 / 8 / 4 kHz | 100 | 100 | 100 | 100 | 100 |
| 2-pole LP 12 kHz | 100 | 100 | 100 | 100 | 100 |
| 2-pole LP 6 kHz | 100 | 100 | 100 | 100 | 100 |
| 2-pole LP 3.4 kHz (telephone) | 100 | 100 | 100 | 100 | **0** |
| HP 20 Hz / 300 Hz (transformer) | 100 | 100 | 100 | 100 | 100 |
| HP 1 kHz | 100 | 100 | **0** | 100 | 100 |
| HP 300 Hz + LP 8 kHz + SNR 15 | 100 | 100 | 100 | 100 | 100 |
| EQ +12 dB @ 3 kHz, Q 1 | 100 | 100 | 100 | **0** | **1.5** |
| EQ −12 dB @ 3 kHz, Q 1 | 100 | 100 | 100 | 100 | 100 |
| EQ −12 dB @ 6 kHz, Q 1 | 100 | 100 | 100 | 100 | 100 |
| soft clip (tanh, drive 4) | 100 | 100 | 100 | 100 | 100 |
| hum 50 Hz @ −6 dB | 100 | 100 | 100 | 100 | 100 |
| hum 50 Hz @ 0 dB | 100 | 65.0 | 100 | 100 | 100 |
| false messages from 60 s of white noise | 0 | 0 | 0 | 0 | 0 |

### 4.2 Airtime, latency, throughput (NoteOn, 48 kHz)

| profile | burst (one copy) | K | airtime / msg | decode delay¹ | max msgs/s | engine loopback² mean / p95 / jitter |
|---|---|---|---|---|---|---|
| robust | 130.0 ms | 2 | 273.0 ms | 130.0 ms | 4 | 343 / 478 / 98 ms |
| fast | 54.0 ms | 1 | 57.0 ms | 54.0 ms | 18 | 78 / 113 / 18 ms |
| ltc2400 | 31.9 ms | 1 | 33.7 ms | 31.7 ms | 30 | 44 / 71 / 11 ms |
| **ltc4800** | **15.9 ms** | **1** | **16.9 ms** | **16.0 ms** | **59** | **22 / 26 / 3 ms** |
| ltc9600 | 8.0 ms | 1 | 8.4 ms | 8.0 ms | 119 | 11 / 15 / 3 ms |

¹ From the first sample of the message's audio to `ModemOutput::Message`, on a clean channel (16-sample
chunks). For BMC, the packet is out as soon as the postamble's first transition arrives.
² `cricket loopback`: paced MIDI in → engine encoder → in-memory audio → engine decoder → MIDI out, 20
notes, while notes are also released (so NoteOffs compete for airtime). This includes the engine's
buffering and polling ticks, about 4–6 ms. Real interfaces add their own buffers.

Burst airtime per message is 3.4× lower than Fast and 16× lower than Robust. A 4-note chord (4 NoteOns)
takes 68 ms on air at 4800 bit/s, against 228 ms on Fast.

### 4.3 Robustness weak spots

1. **Noise.** Zero-crossing timing is not an optimal detector. At 4800 bit/s, BMC needs about **10 dB
   full-band SNR** (≈ 5 dB more inside the RX band) for ≥ 99 % success. Fast works down to ~2 dB and
   Robust below 0 dB. On the lines this profile targets (digital: > 90 dB, analog tie lines: > 60 dB)
   this does not matter, but BMC is **not** a replacement for Robust on unknown chains. An integrate-
   and-dump or correlating detector clocked from the recovered half cell should gain 3–4 dB at roughly
   the same cost (open question).
2. **In-band EQ and group delay.** A +12 dB peaking EQ at 3 kHz (between the 2.4 kHz and 4.8 kHz
   fundamentals) skews interval lengths. The long sync intervals read 2.1 instead of 3, and runs of
   `0`s followed by `1`s produce 1.5-half-cell intervals, so 4800 fails outright. A cut instead of a
   boost, and EQ outside the band, are harmless. LTC has the same weakness, and the same rule applies:
   **no EQ on the line**.
3. **Bandwidth.** 9600 bit/s needs more than ~6 kHz of bandwidth (it fails a 3.4 kHz 2-pole low-pass)
   and ≥ 12 dB SNR, so it is for digital lines only. 2400 bit/s fails a steep 1 kHz high-pass because
   its sync violations have their fundamental at 800 Hz.
4. **Hard decisions only.** The receiver aborts on the first invalid interval. With FEC it should
   produce erasures instead (§3.3).
5. **Gates / fast compressors** ahead of the receiver can eat the start of the burst. The 8-bit
   preamble (1.7 ms) needs only 8 of its 16 intervals, which absorbs ~0.8 ms of gate attack. More needs
   a longer preamble (0.2 ms per bit at 4800).
6. **Spectrum and annoyance.** BMC is broadband (square-wave harmonics, softened by the shaping), so it
   sounds like timecode and produces more crosstalk into neighbouring channels than MFSK tones of the
   same RMS. Bursts are short (17 ms), which helps.

## 5. MCU cost

Per input sample, the BMC receiver does a DC blocker (1 multiply, 2 adds), a biquad (5 multiply-adds),
an envelope (1 multiply, 1 compare), a zero-crossing test, and the Schmitt compares (1 multiply, 2
compares). That is **≈ 8 multiply-adds + ~6 compares, about 20–30 cycles on a Cortex-M33 with FPU**. One
division per transition (≈ every 5 samples at 4800) and the per-interval decoder logic add a few more.
At 48 kHz this is **≈ 1–1.5 MHz, about 1 % of a 150 MHz M33**. The RX keeps **no sample buffer**: its
state is about 100 bytes.

The MFSK receivers run a 16-tone Goertzel bank over a 144–288-sample window every ¼ symbol, which is
≈ 64 Goertzel iterations (≈ 200 FLOPs) per sample. They also run fine alignment and purity/energy
computations, and buffer up to a whole packet: Robust holds ~6 k samples, about 25 KB of f32. That is
**≈ 250–400 cycles per sample, ≈ 12–20 MHz**.

Measured on the host (release build, 10 s of traffic at 30 dB SNR, in ns per input sample):

| robust | fast | ltc2400 | ltc4800 | ltc9600 |
|---|---|---|---|---|
| 161 | 250 | 15.5 | 17.8 | 21.5 |

That is **9–14× cheaper** than MFSK, in line with the estimate, and nearly all the RAM is gone. The TX is
also cheap: per sample it is a table lookup or a `sin` only inside the 0.5-half-cell transition windows.
On an MCU the raised-cosine edge would be a 16-entry LUT.

## 6. Recommendation

* **Default bit rate: 4800 bit/s.** It is the best trade-off. It is 3.4× faster than Fast on air (22 ms
  measured end-to-end), it survives everything in the matrix except an in-band EQ boost, including
  3.4 kHz band limiting, a 1 kHz high-pass, ±1 % rate error, −50 dB level, inversion, clipping and hum,
  and it keeps 6 dB more noise margin than 9600.
* Offer **9600** as "LTC-digital" for Dante/MADI/AES lines (11 ms end-to-end), and possibly **2400** for
  long analog runs with a lot of noise. Rate auto-detection makes the extra rates nearly free on the RX.
* Keep **burst mode with the existing heartbeat**, and consider a shorter heartbeat and link-dead for
  this profile. Make continuous carrier an opt-in later.
* Do **not** change the packet header. Do add **parallel MFSK + BMC reception** (auto-detect) to the
  receiver.
* Before going into the spec: the erasure-producing RX for FEC, a 3–4 dB better detector if it stays
  cheap, a gate/compressor simulation, and **hardware tests** over a real analog transformer path and
  Dante with SRC.

## 7. Open questions

1. Should the RX also lock to bursts at 2× / ½× the configured rate (rate auto-detect), or should the
   rate be part of the profile name only?
2. What heartbeat and link-dead timing should the ltc profile use? 17 ms heartbeats make 250 ms / 750 ms
   affordable at 7 % duty.
3. Continuous carrier: worth implementing as a "LTC-continuous" mode for instant link-dead?
4. Body length: BMC could send variable-length bodies (12 nibbles for NoteOn instead of the padded 16,
   saving 3.3 ms at 4800). That only works if the RX trusts the header type nibble before the CRC. Is it
   worth diverging from the fixed-length MFSK body, and how does it interact with FEC codeword sizes?
5. Output level: 0.57 peak (equal RMS to MFSK). Should it match house LTC level instead (often
   −18…−10 dBFS)? The decoder does not care.
6. Should we prefer a correlating detector (better SNR, needs a clock-recovery loop) or keep pure
   zero-crossing (cheapest, proven by LTC readers)?

## 8. Files

* `cricket_core/src/phy/bmc/{mod,tx,rx}.rs`: BMC parameters, frame, TX synthesis, streaming RX.
* `cricket_core/src/phy/select.rs`: `PhyTx` / `PhyRx` dispatch.
* `cricket_core/src/phy/config.rs`: `modulation` field, `ltc()`, `ltc_with_bit_rate()`, `from_name`.
* `cricket_core/src/modem.rs`: uses the dispatchers; adds BMC-aware `repetition_samples` and
  `repetition_gap_samples`.
* `cricket_core/tests/support/mod.rs`: new impairments.
* `cricket_core/tests/bmc_phy.rs`: regression tests. `cricket_core/tests/phy_ab_measurements.rs`: the
  A/B report and the RX parameter sweep (ignored tests).
* `cricket_cli/src/main.rs`: global `--profile`, `loopback --profiles`.
