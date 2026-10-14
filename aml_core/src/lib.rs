//! AML core library — V2 bitstream modem (default) + legacy V1 phy.
//!
//! The crate is `no_std` + `alloc` so the modem can run on microcontrollers. The
//! default `std` feature adds WAV file I/O (`wav`) and the legacy V1 phy.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod link;
pub mod modem;
pub mod phy;
#[cfg(feature = "std")]
pub mod phy_v1;
pub mod transport;
#[cfg(feature = "std")]
pub mod wav;

// Convenience re-exports for the V2 stack
pub use modem::{
    decode_samples, encode_to_samples, AmlModemRx, AmlModemTx, ModemOutput, ModemStats,
    MAX_BODY_NIBBLES,
};
pub use phy::{LinkState, PhyConfig};
pub use transport::{MessageType, MscCommand, TransportMessage};
