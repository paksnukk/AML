use super::crc::crc16_ccitt;
use crate::transport::MessageType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub msg_type: MessageType,
    pub seq: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketDecodeError {
    TooShort,
    BadCrc,
    UnknownType(u8),
    BadPayloadLen { expected: usize, got: usize },
    BadVersion(u8),
}

pub fn bytes_to_nibbles(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(b >> 4);
        out.push(b & 0x0F);
    }
    out
}

pub fn nibbles_to_bytes(nibbles: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nibbles.len().div_ceil(2));
    let mut i = 0;
    while i + 1 < nibbles.len() {
        out.push(((nibbles[i] & 0x0F) << 4) | (nibbles[i + 1] & 0x0F));
        i += 2;
    }
    out
}

pub fn encode_packet_bytes(packet: &Packet) -> Result<Vec<u8>, PacketDecodeError> {
    let expected = packet.msg_type.payload_len();
    if packet.payload.len() != expected {
        return Err(PacketDecodeError::BadPayloadLen {
            expected,
            got: packet.payload.len(),
        });
    }
    let hdr = (0x2u8 << 4) | (packet.msg_type as u8 & 0x0F);
    let mut body = Vec::with_capacity(2 + expected + 2);
    body.push(hdr);
    body.push(packet.seq);
    body.extend_from_slice(&packet.payload);
    let crc = crc16_ccitt(&body);
    body.push((crc >> 8) as u8);
    body.push((crc & 0xFF) as u8);
    Ok(body)
}

pub fn decode_packet_bytes(bytes: &[u8]) -> Result<Packet, PacketDecodeError> {
    if bytes.len() < 4 {
        return Err(PacketDecodeError::TooShort);
    }
    let data_end = bytes.len() - 2;
    let crc_hi = bytes[data_end];
    let crc_lo = bytes[data_end + 1];
    let data = &bytes[..data_end];
    let expected_crc = crc16_ccitt(data);
    let got_crc = ((crc_hi as u16) << 8) | (crc_lo as u16);
    if expected_crc != got_crc {
        return Err(PacketDecodeError::BadCrc);
    }

    let hdr = data[0];
    let version = hdr >> 4;
    if version != 0x2 {
        return Err(PacketDecodeError::BadVersion(version));
    }
    let type_nibble = hdr & 0x0F;
    let msg_type =
        MessageType::from_u8(type_nibble).ok_or(PacketDecodeError::UnknownType(type_nibble))?;
    let seq = data[1];
    let payload = data[2..].to_vec();
    let expected = msg_type.payload_len();
    if payload.len() != expected {
        return Err(PacketDecodeError::BadPayloadLen {
            expected,
            got: payload.len(),
        });
    }
    Ok(Packet {
        msg_type,
        seq,
        payload,
    })
}

/// Nibble count for a packet body after interleave padding.
pub fn body_nibble_count(msg_type: MessageType, interleave_depth: usize) -> usize {
    let raw_bytes = 2 + msg_type.payload_len() + 2; // hdr+seq+payload+crc
    let raw_nibbles = raw_bytes * 2;
    let depth = interleave_depth.max(1);
    raw_nibbles.div_ceil(depth) * depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MessageType;

    #[test]
    fn packet_roundtrip() {
        let p = Packet {
            msg_type: MessageType::NoteOn,
            seq: 7,
            payload: vec![60, 100],
        };
        let bytes = encode_packet_bytes(&p).unwrap();
        let decoded = decode_packet_bytes(&bytes).unwrap();
        assert_eq!(decoded, p);
    }
}
