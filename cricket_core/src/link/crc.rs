/// CRC-16/CCITT: poly 0x1021, init 0xFFFF, refin=false, refout=false, xorout=0x0000.
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

pub fn verify_crc16(data_without_crc: &[u8], crc_hi: u8, crc_lo: u8) -> bool {
    let expected = crc16_ccitt(data_without_crc);
    expected == ((crc_hi as u16) << 8) | (crc_lo as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vector() {
        // Empty → init value
        assert_eq!(crc16_ccitt(&[]), 0xFFFF);
        let c = crc16_ccitt(&[0x01, 0x02, 0x03]);
        assert_ne!(c, 0);
        assert!(verify_crc16(
            &[0x01, 0x02, 0x03],
            (c >> 8) as u8,
            (c & 0xFF) as u8
        ));
    }
}
