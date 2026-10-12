/// Block interleave: write column-major into `depth` rows, read row-major.
/// Pads with 0 nibbles to a multiple of `depth`.
pub fn interleave(nibbles: &[u8], depth: usize) -> Vec<u8> {
    if depth == 0 {
        return nibbles.to_vec();
    }
    let mut padded = nibbles.to_vec();
    while !padded.len().is_multiple_of(depth) {
        padded.push(0);
    }
    let cols = padded.len() / depth;
    let mut out = Vec::with_capacity(padded.len());
    for r in 0..depth {
        for c in 0..cols {
            out.push(padded[c * depth + r] & 0x0F);
        }
    }
    out
}

pub fn deinterleave(nibbles: &[u8], depth: usize) -> Vec<u8> {
    if depth == 0 || nibbles.is_empty() {
        return nibbles.to_vec();
    }
    let len = nibbles.len();
    let cols = len / depth;
    if cols * depth != len {
        // Truncate to full columns
        let usable = cols * depth;
        return deinterleave(&nibbles[..usable], depth);
    }
    let mut out = vec![0u8; len];
    let mut i = 0;
    for r in 0..depth {
        for c in 0..cols {
            out[c * depth + r] = nibbles[i] & 0x0F;
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_roundtrip() {
        let data: Vec<u8> = (0..12).map(|x| x & 0x0F).collect();
        let enc = interleave(&data, 4);
        let dec = deinterleave(&enc, 4);
        assert_eq!(dec, data);
    }
}
