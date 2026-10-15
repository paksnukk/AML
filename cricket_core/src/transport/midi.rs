//! MIDI mapping (spec §5.2) and the fail-safe burst (spec §5.3).

use super::message::{TransportMessage, PITCH_BEND_CENTER};
use super::msc::{MscError, MscMessage, MscTarget};
use alloc::vec;
use alloc::vec::Vec;

/// CC123 (All Notes Off) on `channel` (0–15).
pub fn all_notes_off(channel: u8) -> Vec<u8> {
    vec![0xB0 | (channel & 0x0F), 0x7B, 0x00]
}

/// CC64 = 0 (sustain pedal off) on `channel` (0–15).
pub fn sustain_off(channel: u8) -> Vec<u8> {
    vec![0xB0 | (channel & 0x0F), 0x40, 0x00]
}

/// Pitch bend to center on `channel` (0–15).
pub fn pitch_bend_center(channel: u8) -> Vec<u8> {
    vec![0xE0 | (channel & 0x0F), 0x00, 0x40]
}

/// Fail-safe burst without pitch-bend state: CC64 = 0 then CC123 on every
/// channel 1–16 (spec §5.3). [`MidiOutput::fail_safe`] adds pitch-bend
/// resets for channels it saw bent.
pub fn fail_safe_burst() -> Vec<Vec<u8>> {
    (0..16u8)
        .flat_map(|ch| [sustain_off(ch), all_notes_off(ch)])
        .collect()
}

/// Decoder-side MIDI output: turns received messages into MIDI and owns
/// the state the fail-safe burst needs (which channels have a pitch bend
/// away from center).
#[derive(Debug, Clone, Default)]
pub struct MidiOutput {
    msc: MscTarget,
    /// Bit `ch` set: the last pitch bend sent on that channel was not center.
    bent: u16,
}

impl MidiOutput {
    pub fn new(msc: MscTarget) -> Self {
        Self { msc, bent: 0 }
    }

    pub fn msc_target(&self) -> MscTarget {
        self.msc
    }

    pub fn set_msc_target(&mut self, msc: MscTarget) {
        self.msc = msc;
    }

    /// MIDI messages for a received message, in order. Panic gives the
    /// fail-safe burst; Heartbeat gives nothing.
    pub fn message(&mut self, msg: &TransportMessage) -> Vec<Vec<u8>> {
        match msg {
            TransportMessage::Heartbeat => Vec::new(),
            TransportMessage::Panic => self.fail_safe(),
            TransportMessage::Msc(m) => vec![m.to_sysex(&self.msc)],
            other => {
                match *other {
                    TransportMessage::PitchBend { channel, value } => {
                        let bit = 1u16 << (channel & 0x0F);
                        if value == PITCH_BEND_CENTER {
                            self.bent &= !bit;
                        } else {
                            self.bent |= bit;
                        }
                    }
                    // Reset All Controllers centers pitch bend (RP-015).
                    TransportMessage::ControlChange {
                        channel,
                        controller: 121,
                        ..
                    } => self.bent &= !(1u16 << (channel & 0x0F)),
                    _ => {}
                }
                other
                    .to_midi_short()
                    .map(|(bytes, len)| vec![bytes[..len].to_vec()])
                    .unwrap_or_default()
            }
        }
    }

    /// The fail-safe burst sent on Panic and link-dead (spec §5.3): on each
    /// channel 1–16, CC64 = 0 then CC123 = 0, and pitch bend to center on
    /// channels whose last pitch bend was off center.
    pub fn fail_safe(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::with_capacity(32 + self.bent.count_ones() as usize);
        for ch in 0..16u8 {
            out.push(sustain_off(ch));
            out.push(all_notes_off(ch));
            if self.bent & (1 << ch) != 0 {
                out.push(pitch_bend_center(ch));
            }
        }
        self.bent = 0;
        out
    }
}

/// Stateless MIDI mapping of one message (MSC with the default
/// [`MscTarget`]; Panic without pitch-bend resets). The decoder uses
/// [`MidiOutput`].
pub fn message_to_midi(msg: &TransportMessage) -> Vec<Vec<u8>> {
    MidiOutput::default().message(msg)
}

/// What a MIDI input message becomes on the transmitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiInput {
    /// Carried: queue it for transmission.
    Message(TransportMessage),
    /// An MSC message that cannot be carried exactly; it is not sent and
    /// the operator should be told why.
    Rejected(MscError),
    /// Not carried by design (other SysEx, MIDI clock and other real-time
    /// messages, MTC quarter frames, undefined bytes).
    Ignored,
}

/// Classify one complete MIDI input message (short message or SysEx).
pub fn midi_input(bytes: &[u8]) -> MidiInput {
    if bytes.first() == Some(&0xF0) {
        return match MscMessage::parse_sysex(bytes) {
            Ok(m) => MidiInput::Message(TransportMessage::Msc(m)),
            Err(MscError::NotMsc) => MidiInput::Ignored,
            Err(e) => MidiInput::Rejected(e),
        };
    }
    match TransportMessage::from_midi_short(bytes) {
        Some(m) => MidiInput::Message(m),
        None => MidiInput::Ignored,
    }
}

/// Parse a MIDI input message into a transport message (short messages and
/// MSC SysEx); `None` for anything not carried or not representable.
pub fn midi_to_message(bytes: &[u8]) -> Option<TransportMessage> {
    match midi_input(bytes) {
        MidiInput::Message(m) => Some(m),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::msc::{MscCommand, MscCue};

    #[test]
    fn burst_covers_every_channel_and_bent_pitch() {
        let mut out = MidiOutput::default();
        let burst = out.fail_safe();
        assert_eq!(burst.len(), 32);
        assert_eq!(burst[0], vec![0xB0, 64, 0]);
        assert_eq!(burst[1], vec![0xB0, 123, 0]);
        assert_eq!(burst[31], vec![0xBF, 123, 0]);
        assert_eq!(burst, fail_safe_burst());

        let bend = |channel, value| TransportMessage::PitchBend { channel, value };
        assert_eq!(out.message(&bend(2, 0x3FFF)), vec![vec![0xE2, 0x7F, 0x7F]]);
        out.message(&bend(5, 100));
        out.message(&bend(9, 1));
        out.message(&bend(9, PITCH_BEND_CENTER)); // returned to center
        out.message(&TransportMessage::ControlChange {
            channel: 5,
            controller: 121,
            value: 0,
        });
        let burst = out.message(&TransportMessage::Panic);
        assert_eq!(burst.len(), 33);
        assert_eq!(
            burst[4..7],
            [vec![0xB2, 64, 0], vec![0xB2, 123, 0], vec![0xE2, 0, 0x40]]
        );
        assert_eq!(out.fail_safe().len(), 32, "bend state cleared");
    }

    #[test]
    fn msc_goes_out_as_sysex_with_receiver_settings() {
        let go = TransportMessage::Msc(MscMessage::cue(
            MscCommand::Go,
            MscCue::from_tenths(15).unwrap(),
            None,
        ));
        assert_eq!(
            message_to_midi(&go),
            vec![vec![
                0xF0, 0x7F, 0x7F, 0x02, 0x01, 0x01, b'1', b'.', b'5', 0xF7
            ]]
        );
        let mut out = MidiOutput::new(MscTarget {
            device_id: 3,
            command_format: 0x10,
        });
        assert_eq!(out.message(&go)[0][2..5], [3, 0x02, 0x10]);
    }

    #[test]
    fn midi_input_classification() {
        let msc = [0xF0, 0x7F, 0x00, 0x02, 0x01, 0x01, b'3', 0xF7];
        assert!(matches!(
            midi_input(&msc),
            MidiInput::Message(TransportMessage::Msc(_))
        ));
        let bad = [
            0xF0, 0x7F, 0x00, 0x02, 0x01, 0x01, b'3', b'.', b'2', b'5', 0xF7,
        ];
        assert!(matches!(midi_input(&bad), MidiInput::Rejected(_)));
        assert_eq!(midi_input(&[0xF0, 0x41, 0x10, 0xF7]), MidiInput::Ignored);
        assert_eq!(midi_input(&[0xF8]), MidiInput::Ignored);
        assert_eq!(midi_input(&[0xF1, 0x20]), MidiInput::Ignored);
        assert_eq!(
            midi_input(&[0x95, 60, 0]),
            MidiInput::Message(TransportMessage::NoteOff {
                channel: 5,
                note: 60,
                velocity: 64
            })
        );
        assert_eq!(
            midi_to_message(&[0xBF, 123, 0]),
            Some(TransportMessage::ControlChange {
                channel: 15,
                controller: 123,
                value: 0
            })
        );
    }
}
