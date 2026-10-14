//! Experimental LTC-style physical layer: biphase-mark code (BMC).
//!
//! Status: **phase-1 prototype** (see `docs/proposals/ltc-bmc.md`); not part
//! of the AML v1 spec. Selected with [`PhyConfig::ltc`](super::PhyConfig::ltc).
//!
//! # Line code
//!
//! Biphase mark, as SMPTE LTC and AES3 use: every bit cell starts with a
//! transition; a `1` has a second transition in the middle of the cell, a `0`
//! has none. Information is carried by *where the transitions are*, never by
//! the level, so the code is polarity-insensitive, DC-free and self-clocking.
//! Between two transitions the receiver sees one of two intervals: a half
//! cell (`H`, half of a `1`) or a full cell (`F`, a `0`).
//!
//! # Frame (one repetition)
//!
//! ```text
//! | preamble: P x '1' | sync: V V | body: 4 x N bits | postamble: '1' | tail |
//! ```
//!
//! * **Preamble** — `preamble_bits` ones: a square wave at the bit rate. The
//!   receiver measures the half-cell length on it (clock acquisition) and
//!   lets its envelope follower settle.
//! * **Sync** — two *code violations* `V`: intervals of 1.5 cells (3 half
//!   cells), first one level, then the other. Valid BMC data only ever
//!   produces `H` and `F` intervals, so `V V` cannot occur inside a body
//!   whatever the payload: the sync is unambiguous without a reserved data
//!   pattern or bit stuffing (AES3 uses violations for the same reason). Being
//!   a pair of opposite-level intervals it is DC-balanced and, like the rest
//!   of the code, polarity-free. The edge ending the second `V` is the start
//!   of the first body bit.
//! * **Body** — the link-layer nibbles, MSB first (exactly what the MFSK phy
//!   would send as tones), 4 bits per nibble.
//! * **Postamble** — one `1` bit: terminates the last body bit with a
//!   transition (its length could not be measured otherwise).
//! * **Tail** — one half cell at the last level, then a shaped fade to 0.
//!
//! The burst is followed by `gap_bits` of silence before the next repetition
//! or message.
//!
//! # Shaping
//!
//! Transitions are raised-cosine steps `edge_frac` half cells wide, placed at
//! exact (fractional) sample positions, so the signal is band-limited (LTC
//! specifies a 25 us +-5 rise time for the same reason) and any sample rate
//! works. Level: `amplitude / sqrt(2)`, i.e. the same RMS as an MFSK tone of
//! the same `amplitude` (fair SNR comparisons, lower peak).
//!
//! # Receiver
//!
//! DC blocker -> 2nd-order low-pass -> peak envelope -> Schmitt trigger with
//! a threshold proportional to the envelope (level-independent) -> transition
//! times taken from the interpolated zero crossing that preceded each Schmitt
//! flip (sub-sample timing, chatter-free) -> interval classification against
//! a half-cell estimate that is acquired on the preamble and tracked through
//! the body. Each interval is measured on its own, so clock drift never
//! accumulates; only the ratio to the estimate matters.

mod rx;
mod tx;

pub use rx::BmcRx;
pub use tx::BmcTx;

/// Physical-layer modulation selected by [`PhyConfig`](super::PhyConfig).
#[derive(Debug, Clone, PartialEq)]
pub enum Modulation {
    /// AML v1 MFSK-16 (Goertzel bank). Used by the robust and fast profiles.
    Mfsk,
    /// Experimental biphase-mark code (LTC-style), see the module docs.
    Bmc(BmcParams),
}

/// Parameters of the experimental BMC phy.
#[derive(Debug, Clone, PartialEq)]
pub struct BmcParams {
    /// Bits per second (one bit = one BMC cell).
    pub bit_rate: u32,
    /// Number of `1` bits before the sync.
    pub preamble_bits: usize,
    /// Number of `1` bits after the body.
    pub postamble_bits: usize,
    /// Silence after each burst, in bit cells.
    pub gap_bits: usize,
    /// Raised-cosine transition width as a fraction of a half cell (0..=1).
    pub edge_frac: f32,
    /// RX low-pass cutoff as a multiple of the bit rate.
    pub rx_lowpass_ratio: f32,
    /// RX DC-blocker cutoff as a fraction of the bit rate.
    pub rx_highpass_ratio: f32,
    /// Schmitt threshold as a fraction of the peak envelope.
    pub hysteresis: f32,
    /// Peak-envelope release time constant, in bit cells.
    pub envelope_release_bits: f32,
    /// Consecutive preamble half-cell intervals needed before a sync is accepted.
    pub min_train_intervals: usize,
    /// Accepted deviation of the measured half cell from nominal (0.05 = +-5 %).
    pub max_rate_error: f32,
}

impl BmcParams {
    /// Defaults for `bit_rate` (all other parameters are bit-rate relative).
    pub fn new(bit_rate: u32) -> Self {
        Self {
            bit_rate,
            preamble_bits: 8,
            postamble_bits: 1,
            gap_bits: 4,
            edge_frac: 0.5,
            rx_lowpass_ratio: 1.5,
            rx_highpass_ratio: 0.05,
            hysteresis: 0.2,
            envelope_release_bits: 8.0,
            min_train_intervals: 8,
            max_rate_error: 0.05,
        }
    }

    /// Samples per half cell (fractional) at `sample_rate_hz`.
    pub fn half_cell_samples(&self, sample_rate_hz: u32) -> f32 {
        sample_rate_hz as f32 / (2.0 * self.bit_rate as f32)
    }

    /// Half cells in one burst carrying `body_nibbles` nibbles: preamble,
    /// sync (2 x 3), body, postamble and the one-half-cell tail.
    pub fn frame_half_cells(&self, body_nibbles: usize) -> usize {
        2 * self.preamble_bits + 6 + 8 * body_nibbles + 2 * self.postamble_bits + 1
    }

    /// Samples one burst occupies, including the shaped fade at the end.
    pub fn frame_samples(&self, sample_rate_hz: u32, body_nibbles: usize) -> usize {
        let half = self.half_cell_samples(sample_rate_hz);
        // The TX leads by half an edge and the final fade takes another half.
        let len = self.frame_half_cells(body_nibbles) as f32 + self.edge_frac.clamp(0.0, 1.0);
        libm::ceilf(len * half) as usize + 1
    }

    /// Silence after each burst, in samples.
    pub fn gap_samples(&self, sample_rate_hz: u32) -> usize {
        libm::roundf(2.0 * self.gap_bits as f32 * self.half_cell_samples(sample_rate_hz)) as usize
    }

    /// Burst + trailing gap, in samples.
    pub fn repetition_samples(&self, sample_rate_hz: u32, body_nibbles: usize) -> usize {
        self.frame_samples(sample_rate_hz, body_nibbles) + self.gap_samples(sample_rate_hz)
    }

    /// Burst duration in milliseconds (without the gap).
    pub fn frame_ms(&self, body_nibbles: usize) -> f32 {
        self.frame_half_cells(body_nibbles) as f32 * 500.0 / self.bit_rate as f32
    }
}

/// Transition intervals of one burst, in half cells (1 = `H`, 2 = `F`,
/// 3 = sync violation `V`). The burst starts at a level, each interval is
/// followed by a transition, and the tail half cell follows the last one.
pub(crate) fn frame_intervals(params: &BmcParams, body_nibbles: &[u8]) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::with_capacity(params.frame_half_cells(body_nibbles.len()));
    for _ in 0..params.preamble_bits {
        out.extend_from_slice(&[1, 1]);
    }
    out.extend_from_slice(&[3, 3]);
    for &n in body_nibbles {
        for shift in (0..4).rev() {
            if (n >> shift) & 1 == 1 {
                out.extend_from_slice(&[1, 1]);
            } else {
                out.push(2);
            }
        }
    }
    for _ in 0..params.postamble_bits {
        out.extend_from_slice(&[1, 1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phy::PhyConfig;
    use alloc::vec;
    use alloc::vec::Vec;

    fn decode_stream(config: &PhyConfig, samples: &[f32], chunk: usize) -> Vec<u8> {
        let mut rx = BmcRx::new(config.clone());
        rx.set_expected_body_nibbles(16);
        let mut nibbles = Vec::new();
        for c in samples.chunks(chunk) {
            for ev in rx.push_samples(c) {
                if let crate::phy::RxEvent::BodyNibble { result, .. } = ev {
                    nibbles.push(result.nibble);
                }
            }
        }
        nibbles
    }

    #[test]
    fn frame_layout_counts() {
        let p = BmcParams::new(4800);
        let body = [0u8; 16];
        let iv = frame_intervals(&p, &body);
        let total: usize = iv.iter().map(|&u| u as usize).sum::<usize>() + 1;
        assert_eq!(total, p.frame_half_cells(16));
        assert_eq!(p.half_cell_samples(48_000), 5.0);
    }

    #[test]
    fn roundtrip_all_nibbles_both_polarities() {
        for rate in [2400, 4800, 9600] {
            let config = PhyConfig::ltc_with_bit_rate(rate);
            let body: Vec<u8> = (0..16).collect();
            let tx = BmcTx::new(config.clone());
            let mut audio = vec![0.0; 37];
            audio.extend(tx.synthesize_framed(&body));
            audio.extend(vec![0.0; 200]);
            assert_eq!(decode_stream(&config, &audio, 64), body, "rate {rate}");
            let inverted: Vec<f32> = audio.iter().map(|s| -s).collect();
            assert_eq!(
                decode_stream(&config, &inverted, 7),
                body,
                "rate {rate} inv"
            );
        }
    }

    #[test]
    fn silence_produces_nothing() {
        let config = PhyConfig::ltc();
        assert!(decode_stream(&config, &vec![0.0; 48_000], 256).is_empty());
    }
}
