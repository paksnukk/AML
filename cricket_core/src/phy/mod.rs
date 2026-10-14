//! AML v1 physical layer: MFSK-16 synthesis, Goertzel demod, preamble sync.
//! Plus an experimental biphase-mark (LTC-style) phy in [`bmc`].

pub mod bmc;
mod config;
mod demod;
mod goertzel;
mod select;
mod synth;

pub use bmc::{BmcParams, Modulation};
pub use config::PhyConfig;
pub use demod::{DemodResult, LinkState, ModemRx, RxEvent};
pub use goertzel::{best_tone, goertzel_power, tone_purity, ToneDetection};
pub use select::{PhyRx, PhyTx};
pub use synth::ModemTx;
