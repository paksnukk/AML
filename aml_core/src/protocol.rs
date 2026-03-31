#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    NoteOn,
    NoteOff,
    ProgramChange,
    MscLite,
    Heartbeat,
    Panic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MscCommand {
    Go = 0,
    Stop = 1,
    Resume = 2,
    Load = 3,
}

// Representing the logical meaning of a decoded symbol
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SymbolValue {
    Preamble,
    Type(FrameType),
    Note(u8),       // 0-11
    Octave(u8),     // 0-10
    Value(u8),      // 0-15 (Generic 4-bit value)
    MscCmd(MscCommand),
    CueList(u8),    // 0-15
}

impl FrameType {
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(FrameType::NoteOn),
            1 => Some(FrameType::NoteOff),
            2 => Some(FrameType::ProgramChange),
            3 => Some(FrameType::MscLite),
            4 => Some(FrameType::Heartbeat),
            5 => Some(FrameType::Panic),
            _ => None,
        }
    }

    pub fn to_u8(&self) -> u8 {
        match self {
            FrameType::NoteOn => 0,
            FrameType::NoteOff => 1,
            FrameType::ProgramChange => 2,
            FrameType::MscLite => 3,
            FrameType::Heartbeat => 4,
            FrameType::Panic => 5,
        }
    }
}

impl MscCommand {
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(MscCommand::Go),
            1 => Some(MscCommand::Stop),
            2 => Some(MscCommand::Resume),
            3 => Some(MscCommand::Load),
            _ => None,
        }
    }

    pub fn to_u8(&self) -> u8 {
        *self as u8
    }
}
