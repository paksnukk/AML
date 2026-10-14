# Proposal: Reed–Solomon FEC for the Robust profile (experimental)

Status: **Phase 1, prototype + measurements.** Not part of AML v1. The `robust` and `fast` profiles are
unchanged on the wire; the prototype is the opt-in profile `robust_fec` (`PhyConfig::robust_fec()`).

## 1. Problem

Robust sends every packet twice (K = 2). That doubles airtime: 273 ms per message instead of 136.5 ms.
On a single message the first good copy arrives after ~131 ms either way. Under real traffic, though, a
NoteOn waits behind whatever is on the air (often a NoteOff refresh), and that wait is a full K = 2
transmission. This is most of the measured NoteOn latency (mean ~360 ms, p95 ~500 ms in `cricket loopback`).

Repetition is also a weak code. A copy is lost when any one of its 20 symbols is bad, and the receiver
abandons a lock on the first body symbol that fails the detection gate.

## 2. Chosen code: RS over GF(16), two interleaved RS(11,7) codewords

- **Symbol-matched.** One MFSK-16 tone is one nibble is one GF(16) code symbol. A wrong tone is exactly
  one symbol error, whatever its bit pattern. Bit-oriented codes are a poor fit: one wrong tone flips
  1–4 bits, so Hamming(7,4) fails and a convolutional/Viterbi code needs bit interleaving and soft bits
  that the phy does not have. Byte-wise RS over GF(256) works, but a codeword needs at least ~11 bytes
  (22 symbols) for the same correction, and a single tone error costs one whole byte.
- **Erasures are cheap and accurate.** The demodulator already has a reliable gate (purity, confidence).
  In the simulator, under AWGN, **every** body symbol that passed the gate was correct, and every bad
  symbol failed the gate. RS corrects `2e + f <= parity`, so an erasure costs half as much as an error.
- **Layout.** RS over GF(16) has `n <= 15`, and the largest packet (MscLite) is 7 bytes = 14 nibbles.
  The 14 data nibbles (packet bytes zero-padded, **CRC-16 unchanged**) are split into two halves, and
  each half is encoded as a shortened RS(11,7) (4 parity nibbles). The codewords go out alternately
  (`a0 b0 a1 b1 …`; this replaces the depth-4 block interleaver). Body: 22 symbols instead of 16.
  Per codeword the code corrects 2 errors, or 4 erasures, or a mix. On the wire that covers any burst
  of 4 wrong tones or 8 erased symbols.
- **Decoder.** Errors-and-erasures (Forney syndromes, Berlekamp–Massey, Chien, Forney). A small
  GMD search adds 0..4 extra erasures on the lowest-`confidence_db` symbols and finally tries
  errors-only. Each codeword yields at most 6 candidates. A candidate pair is accepted only if the
  CRC-16, version, type and zero padding all check out.
- **RX changes (only with FEC):** a body symbol that fails the gate is kept as an erasure, up to 8 per
  body (above that the attempt is abandoned and rewinds as today). One preamble symbol after the
  first may also fail, because the coded body + CRC now verify the lock. Without this change, the
  4-symbol preamble becomes the main loss mechanism at K = 1 (see §5).

Code: `cricket_core/src/link/rs16.rs` (codec, ~300 lines, `no_std` + `alloc`, no dependencies),
`cricket_core/src/link/fec.rs` (`FecMode`, `FecCodec`), `PhyConfig::fec`, and erasure and
preamble-miss handling in `phy/demod.rs`. `ModemStats::fec_corrected` counts the repaired symbols.

## 3. Wire-format impact

| | AML v1 Robust | `robust_fec` |
|---|---|---|
| Preamble | `A 5 C 3` | unchanged (RX tolerates 1 miss after the first) |
| Packet bytes (HDR, SEQ, PAYLOAD, CRC-16) | — | unchanged |
| Body | 16 nibbles, block-interleaved (depth 4) | 22 nibbles: 2 × RS(11,7), symbol-interleaved |
| Copies K | 2 | 1 |
| On-wire per message | 2 × 21 periods = 273 ms | 27 periods = 175.5 ms |

- **No header flag is possible.** The header sits inside the codeword, and the receiver has to know
  the body length before it can decode. The FEC mode must be a profile setting on both ends, the
  same way Robust and Fast are today (profiles are not signalled on the wire).
- **Coexistence is safe.** A mismatched receiver locks the preamble, reads the wrong body length,
  fails CRC and drops the packet. No false messages.
- **Spec changes if adopted:** a new §4.6 "Body FEC" (field polynomial `x^4+x+1`, generator roots
  α^1..α^4, systematic, wire order data-first/highest-degree-first, two codewords over
  data nibbles 0–6 and 7–13, alternating on the wire). §4.3 interleave applies to uncoded profiles
  only. In §4.4 the dedup window uses the profile's body length. §3.2 gets the erasure and
  preamble-miss rules. §6 gets a new profile entry. `PROTOCOL_VERSION` can stay `0x1`: the packet
  format is identical, only the body coding differs per profile.

## 4. Measurements

Channel simulator (`cricket_core/tests/support`), one message per trial (¾ NoteOn, ¼ MscLite), random
offset and seed, 400 trials per cell. Delivery % and mean time from message start to delivery.
Reproduce with `cargo test --release -p cricket_core --test fec -- --ignored --nocapture`.
SNR is relative to the tone RMS over the full 24 kHz band. "Wrong tone" replaces a symbol period
with another tone (plus SNR 20 dB). A dropout mutes one stretch at a random position (plus SNR 20 dB).

| channel | robust K2 (today) | robust K1 | **fec p4 K1** | fec p2 K1 | fec p4 K2 |
|---|---|---|---|---|---|
| airtime / message | 273 ms | 136.5 ms | **175.5 ms** | 149.5 ms | 351 ms |
| clean | 100 (131 ms) | 100 (131) | **100 (170)** | 100 (144) | 100 (170) |
| AWGN 6 dB | 100 (131) | 100 (131) | **100 (170)** | 100 (144) | 100 (170) |
| AWGN 0 dB | 100 (134) | 97.5 | **97.8** | 97.8 | 100 |
| AWGN −2 dB | 97.0 (147) | 85.8 | **93.8** | 93.8 | 99.5 |
| AWGN −3 dB | 33.0 | 19.5 | **77.2** | 76.2 | 94.5 |
| AWGN −4 dB | 0 | 0 | **18.2** | 3.5 | 32.5 |
| wrong tone 1 % | 97.0 (152) | 82.0 | **99.0 (170)** | 98.2 | 100 |
| wrong tone 2 % | 94.8 (164) | 71.5 | **98.5 (170)** | 96.0 | 100 |
| wrong tone 5 % | 64.0 | 41.0 | **88.5** | 79.5 | 99.0 |
| wrong tone 10 % | 32.2 | 17.5 | **73.0** | 54.0 | 91.8 |
| dropout 10 ms | 100 (163) | 65.5 | **95.8** | 96.2 | 100 |
| dropout 30 ms | 96.5 | 63.0 | **96.2** | 79.2 | 100 |
| dropout 60 ms | 88.8 | 58.2 | **70.2** | 63.5 | 100 |
| gain −40 dB, SNR 0 | 100 | 96.2 | **97.5** | 97.5 | 99.8 |
| low-pass 2.5 kHz, SNR 6 | 100 | 98.5 | **99.0** | 98.8 | 100 |
| drift 500 ppm, SNR 0 | 100 | 98.5 | **98.8** | 98.8 | 100 |

No cell produced a false message. The no-preamble-tolerance variant (first prototype) scored worse
at K = 1, because the preamble was then the main loss: wrong tone 1/2/5 % = 96.8/92.8/77.0,
AWGN −3 dB = 62.5, dropout 10 ms = 93.0.

**Engine latency** (`cricket loopback --profiles robust,robust_fec --notes 60`, MIDI in → MIDI out,
notes held and released, 3 runs, consistent within ±1 ms):

| profile | mean | p95 | jitter |
|---|---|---|---|
| robust (K=2) | 360 ms | 500 ms | 93 ms |
| **robust_fec (K=1)** | **269 ms (−25 %)** | **348 ms (−31 %)** | **55 ms** |

**Stuck-note run** (`tests/stuck_notes.rs`, back-to-back traffic, SNR 20 dB + wrong tones). Messages
lost, robust vs robust_fec: 0.5 % → 2/788 vs 6/918; 1 % → 13/819 vs 10/1046; 2 % → 47/752 vs 26/1048.
robust_fec sends ~30 % more messages in the same time (more NoteOff refreshes fit). No false
messages and no stuck notes in either profile.

## 5. Reading the results

- Single-message latency goes **up** by 39 ms (body 22 vs 16 symbols, and K=2's first copy was
  already a full message). Latency under traffic goes **down** by ~25 % (mean) and ~31 % (p95),
  because each message holds the channel for 175.5 ms instead of 273 ms.
- Against symbol errors (the dominant MFSK failure), FEC K=1 beats K=2 at every level we tested,
  and its advantage grows with the error rate.
- Under AWGN, K=1 loses ~2 % at 0 dB. In those trials the first preamble symbol fails its 3 dB
  gate, and only a second copy helps. Below −2 dB FEC is far better (erasure decoding).
- **Dropouts longer than the preamble** (≳ 30 ms) are where repetition still wins. FEC K=1 cannot
  recover a lost preamble. A dropout that falls inside the body and covers at most 8 symbols
  (~45 ms) is fully recovered.
- p = 2 (RS(9,7)) saves only 26 ms of airtime and is clearly weaker. p = 4 is the better trade.
- FEC with K=2 is the most robust option measured. It costs 351 ms of airtime, so it is an option for
  cue-only links (MSC) where latency doesn't matter.

## 6. Recommendation

1. Adopt **RS(11,7) × 2 over GF(16), with erasure decoding and K = 1**, as a new profile
   ("Robust-FEC", or as the next Robust after a field trial). It cuts airtime by 36 % and loaded
   NoteOn latency by ~25–30 %. It is at least as robust as today's K=2 against symbol errors and
   deep noise. It is slightly worse at the AWGN edge and for long dropouts.
2. Keep K=2 + FEC as an option for latency-insensitive cue links.
3. Before replacing Robust, harden the first preamble symbol: allow a softer gate on preamble[0]
   when FEC is on, or detect the preamble by correlation. That is now the residual K=1 weakness.
4. Do not touch Fast yet. The same layer works there (`fast` + FEC: 72 ms vs 54 ms per message),
   but Fast exists for clean chains, where FEC buys little.

## 7. Reuse by other modulations (LTC/BMC branch)

`FecCodec` is modulation-agnostic. It takes packet nibbles and returns coded nibbles
(`encode_body`), and it takes `BodySymbol { nibble, reliability, erased }` and returns packet nibbles
(`decode_body`). A biphase-mark mode can transmit the 22 coded nibbles as 88 bits. It can mark a
nibble erased when any of its 4 bit cells is ambiguous (e.g. a transition missing or out of place),
and use the weakest cell margin as `reliability`. For bursty bit errors, the BMC mode should keep
each nibble's 4 bits adjacent, so that one burst damages few symbols. The codeword alternation
already spreads bursts across both codewords. Nothing in `rs16.rs`/`fec.rs` depends on MFSK.

This is now implemented as the experimental `ltc_fec` profiles. The BMC receiver uses the weakest bit
margin as `reliability` and erases nibbles that contain a dropout. Marking ambiguous nibbles as erased
did not help: the reliability-ordered candidates already cover them. See `docs/proposals/ltc-bmc.md` §9.

## 8. Open questions

- **False lock swallowing a real packet.** After a false lock whose body completes (≤ 8 erasures)
  but fails RS/CRC, the phy does not rewind, so a real packet that starts inside it is lost. This
  already happens with uncoded bodies. Tolerating erasures and preamble misses makes it somewhat more
  likely, though the stuck-note run shows a net gain. The fix is a "decode failed → rewind" hook
  from the modem to the phy.
- **Miscorrection budget.** Up to 36 candidate pairs are CRC-checked per body. A rough bound on
  undetected error is 36 × 2⁻¹⁶ × P(junk decodes) × (version/type/pad filters ≈ 2⁻⁸), which is
  negligible. No false message was seen in any test (noise, random tone streams, all impairments).
  The bound still needs a proper analysis before this goes in the spec.
- **Variable body length.** NoteOn needs only 12 data nibbles, so 2 × RS(10,6) = 20 symbols would
  save 13 ms. It would require the type to be known early, e.g. a separately protected first codeword.
- **Parity per profile.** Is 4 right for real converter chains? A field trial with
  `fec_corrected` / CRC stats on real Dante/MADI/analog paths should decide it.
- **Should the preamble-miss rule be part of the FEC mode or a separate phy knob?** In the prototype
  it is tied to `FecMode` to keep `PhyConfig` changes minimal.
