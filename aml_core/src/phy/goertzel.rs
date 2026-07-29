use std::f32::consts::PI;

/// Non-coherent Goertzel power for a single target frequency.
pub fn goertzel_power(samples: &[f32], target_freq: f32, sample_rate: u32) -> f32 {
    let n = samples.len() as f32;
    if n < 2.0 {
        return 0.0;
    }
    let k = (n * target_freq) / sample_rate as f32;
    let w = 2.0 * PI * k / n;
    let coeff = 2.0 * w.cos();

    let mut q1 = 0.0f32;
    let mut q2 = 0.0f32;
    for &sample in samples {
        let q0 = coeff * q1 - q2 + sample;
        q2 = q1;
        q1 = q0;
    }
    q1 * q1 + q2 * q2 - q1 * q2 * coeff
}

/// Pick the strongest tone among `tones`, returning (index, power, confidence_db).
pub fn best_tone(
    samples: &[f32],
    tones: &[f32],
    sample_rate: u32,
) -> Option<(u8, f32, f32)> {
    if tones.is_empty() || samples.is_empty() {
        return None;
    }

    let mut best_idx = 0u8;
    let mut best_power = 0.0f32;
    let mut runner_up = 0.0f32;

    for (i, &freq) in tones.iter().enumerate() {
        let power = goertzel_power(samples, freq, sample_rate);
        if power > best_power {
            runner_up = best_power;
            best_power = power;
            best_idx = i as u8;
        } else if power > runner_up {
            runner_up = power;
        }
    }

    let confidence_db = if runner_up > 0.0 {
        10.0 * (best_power / runner_up).log10()
    } else {
        100.0
    };

    Some((best_idx, best_power, confidence_db))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phy::{ModemTx, PhyConfig};
    use crate::link::{bytes_to_nibbles, encode_packet_bytes, interleave, Packet};
    use crate::transport::TransportMessage;
    use crate::modem::MAX_BODY_NIBBLES;

    #[test]
    fn perfect_grid_demod_note60() {
        let config = PhyConfig::robust();
        let msg = TransportMessage::NoteOn {
            note: 60,
            velocity: 100,
        };
        let packet = Packet {
            msg_type: msg.msg_type(),
            seq: 0,
            payload: msg.to_payload(),
        };
        let bytes = encode_packet_bytes(&packet).unwrap();
        let mut nibbles = bytes_to_nibbles(&bytes);
        while nibbles.len() < MAX_BODY_NIBBLES {
            nibbles.push(0);
        }
        nibbles.truncate(MAX_BODY_NIBBLES);
        let interleaved = interleave(&nibbles, config.interleave_depth);

        let mut all_syms = Vec::new();
        all_syms.extend_from_slice(&config.preamble);
        all_syms.extend_from_slice(&interleaved);

        let tx = ModemTx::new(config.clone());
        let audio = tx.synthesize_nibbles(&all_syms);
        let period = config.samples_per_symbol_period();
        let sym = config.symbol_samples();
        let tones = config.tone_table();

        for (i, &expect) in all_syms.iter().enumerate() {
            let start = i * period;
            let window = &audio[start..start + sym];
            let (got, power, conf) = best_tone(window, &tones, config.sample_rate_hz).unwrap();
            assert_eq!(
                got, expect,
                "symbol {i}: expected {expect} got {got} power={power} conf={conf}"
            );
        }
    }
}
