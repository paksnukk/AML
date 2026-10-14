//! Link layer: CRC-16, packet framing, interleave, repetition.

mod crc;
mod interleave;
mod packet;

pub use crc::{crc16_ccitt, verify_crc16};
pub use interleave::{deinterleave, interleave};
pub use packet::{
    body_nibble_count, bytes_to_nibbles, decode_packet_bytes, encode_packet_bytes,
    nibbles_to_bytes, Packet, PacketDecodeError, PROTOCOL_VERSION,
};
