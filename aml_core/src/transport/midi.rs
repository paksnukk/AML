use super::message::{MessageType, TransportMessage};

/// Convert a transport message to MIDI 1.0 messages (channel 1).
///
/// Returns one entry per MIDI message (each a complete short message), so a
/// sink can send them one by one. Empty for messages without a MIDI mapping.
pub fn message_to_midi(msg: &TransportMessage) -> Vec<Vec<u8>> {
    match msg {
        TransportMessage::NoteOn { note, velocity } => {
            if *velocity == 0 {
                vec![vec![0x80, *note & 0x7F, 0x40]]
            } else {
                vec![vec![0x90, *note & 0x7F, *velocity & 0x7F]]
            }
        }
        TransportMessage::NoteOff { note } => vec![vec![0x80, *note & 0x7F, 0x40]],
        TransportMessage::ProgramChange { program } => vec![vec![0xC0, *program & 0x7F]],
        TransportMessage::Panic => fail_safe_burst(),
        TransportMessage::Heartbeat | TransportMessage::MscLite { .. } => Vec::new(),
    }
}

/// CC123 (All Notes Off), channel 1.
pub fn all_notes_off() -> Vec<u8> {
    vec![0xB0, 0x7B, 0x00]
}

/// CC64 = 0 (sustain pedal off), channel 1.
pub fn sustain_off() -> Vec<u8> {
    vec![0xB0, 0x40, 0x00]
}

/// Fail-safe burst sent on Panic and on link-dead, in this order:
/// CC64 = 0 (sustain off, so released notes cannot keep ringing), then
/// CC123 = 0 (All Notes Off). Channel 1.
pub fn fail_safe_burst() -> Vec<Vec<u8>> {
    vec![sustain_off(), all_notes_off()]
}

/// Parse a short MIDI message into a transport message (channel voice only).
pub fn midi_to_message(bytes: &[u8]) -> Option<TransportMessage> {
    if bytes.is_empty() {
        return None;
    }
    let status = bytes[0] & 0xF0;
    match status {
        0x90 if bytes.len() >= 3 => {
            let note = bytes[1] & 0x7F;
            let vel = bytes[2] & 0x7F;
            if vel == 0 {
                Some(TransportMessage::NoteOff { note })
            } else {
                Some(TransportMessage::NoteOn {
                    note,
                    velocity: vel,
                })
            }
        }
        0x80 if bytes.len() >= 3 => Some(TransportMessage::NoteOff {
            note: bytes[1] & 0x7F,
        }),
        0xC0 if bytes.len() >= 2 => Some(TransportMessage::ProgramChange {
            program: bytes[1] & 0x7F,
        }),
        0xB0 if bytes.len() >= 3 && bytes[1] == 0x7B => Some(TransportMessage::Panic),
        _ => {
            // Ignore other CC / messages
            let _ = MessageType::Heartbeat;
            None
        }
    }
}
