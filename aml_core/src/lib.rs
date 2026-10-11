//! AML core library — V2 bitstream modem (default) + legacy V1 phy.

pub mod link;
pub mod modem;
pub mod phy;
pub mod phy_v1;
pub mod transport;
pub mod wav;

// Convenience re-exports for the V2 stack
pub use modem::{
    decode_samples, encode_to_samples, AmlModemRx, AmlModemTx, ModemOutput, ModemStats,
    MAX_BODY_NIBBLES,
};
pub use phy::{LinkState, PhyConfig};
pub use transport::{MessageType, MscCommand, TransportMessage};
