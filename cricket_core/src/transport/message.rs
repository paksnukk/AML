use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    NoteOn = 0,
    NoteOff = 1,
    ProgramChange = 2,
    MscLite = 3,
    Heartbeat = 4,
    Panic = 5,
}

impl MessageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::NoteOn),
            1 => Some(Self::NoteOff),
            2 => Some(Self::ProgramChange),
            3 => Some(Self::MscLite),
            4 => Some(Self::Heartbeat),
            5 => Some(Self::Panic),
            _ => None,
        }
    }

    pub fn payload_len(self) -> usize {
        match self {
            Self::NoteOn => 2,
            Self::NoteOff => 1,
            Self::ProgramChange => 1,
            Self::MscLite => 3,
            Self::Heartbeat | Self::Panic => 0,
        }
    }

    /// Whether `payload` (of [`payload_len`](Self::payload_len) bytes) is
    /// within the value ranges of spec §5: MIDI data bytes 0–127, MscLite
    /// `cmd` 0–3, `cuelist` 0–15, `cue` 0–127. A transmitter never sends
    /// anything else; the FEC receivers reject out-of-range payloads as an
    /// extra check against false decodes (`docs/proposals/fec.md` §9).
    pub fn payload_in_range(self, payload: &[u8]) -> bool {
        if payload.len() != self.payload_len() {
            return false;
        }
        match self {
            Self::NoteOn | Self::NoteOff | Self::ProgramChange => payload.iter().all(|&b| b < 0x80),
            Self::MscLite => {
                MscCommand::from_u8(payload[0]).is_some() && payload[1] < 0x10 && payload[2] < 0x80
            }
            Self::Heartbeat | Self::Panic => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MscCommand {
    Go = 0,
    Stop = 1,
    Resume = 2,
    Load = 3,
}

impl MscCommand {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Go),
            1 => Some(Self::Stop),
            2 => Some(Self::Resume),
            3 => Some(Self::Load),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportMessage {
    NoteOn {
        note: u8,
        velocity: u8,
    },
    NoteOff {
        note: u8,
    },
    ProgramChange {
        program: u8,
    },
    MscLite {
        cmd: MscCommand,
        cuelist: u8,
        cue: u8,
    },
    Heartbeat,
    Panic,
}

impl TransportMessage {
    pub fn msg_type(&self) -> MessageType {
        match self {
            Self::NoteOn { .. } => MessageType::NoteOn,
            Self::NoteOff { .. } => MessageType::NoteOff,
            Self::ProgramChange { .. } => MessageType::ProgramChange,
            Self::MscLite { .. } => MessageType::MscLite,
            Self::Heartbeat => MessageType::Heartbeat,
            Self::Panic => MessageType::Panic,
        }
    }

    pub fn to_payload(&self) -> Vec<u8> {
        match self {
            Self::NoteOn { note, velocity } => vec![*note & 0x7F, *velocity & 0x7F],
            Self::NoteOff { note } => vec![*note & 0x7F],
            Self::ProgramChange { program } => vec![*program & 0x7F],
            Self::MscLite { cmd, cuelist, cue } => {
                vec![*cmd as u8, *cuelist & 0x0F, *cue & 0x7F]
            }
            Self::Heartbeat | Self::Panic => Vec::new(),
        }
    }

    pub fn from_type_payload(msg_type: MessageType, payload: &[u8]) -> Option<Self> {
        if payload.len() != msg_type.payload_len() {
            return None;
        }
        match msg_type {
            MessageType::NoteOn => Some(Self::NoteOn {
                note: payload[0] & 0x7F,
                velocity: payload[1] & 0x7F,
            }),
            MessageType::NoteOff => Some(Self::NoteOff {
                note: payload[0] & 0x7F,
            }),
            MessageType::ProgramChange => Some(Self::ProgramChange {
                program: payload[0] & 0x7F,
            }),
            MessageType::MscLite => Some(Self::MscLite {
                cmd: MscCommand::from_u8(payload[0])?,
                cuelist: payload[1] & 0x0F,
                cue: payload[2] & 0x7F,
            }),
            MessageType::Heartbeat => Some(Self::Heartbeat),
            MessageType::Panic => Some(Self::Panic),
        }
    }
}
