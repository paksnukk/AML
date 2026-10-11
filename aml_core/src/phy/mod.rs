//! AML V2 physical layer: MFSK-16 synthesis, Goertzel demod, preamble sync.

mod config;
mod goertzel;
mod synth;
mod demod;

pub use config::PhyConfig;
pub use goertzel::{best_tone, goertzel_power, tone_purity, ToneDetection};
pub use demod::{DemodResult, LinkState, ModemRx, RxEvent};
pub use synth::ModemTx;
