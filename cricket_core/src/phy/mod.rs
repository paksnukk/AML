//! AML v1 physical layer: MFSK-16 synthesis, Goertzel demod, preamble sync.

mod config;
mod demod;
mod goertzel;
mod synth;

pub use config::PhyConfig;
pub use demod::{DemodResult, LinkState, ModemRx, RxEvent};
pub use goertzel::{best_tone, goertzel_power, tone_purity, ToneDetection};
pub use synth::ModemTx;
