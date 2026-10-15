//! Transport layer: messages, MIDI and MSC mapping, scheduler, fail-safe policy.

mod fail_safe;
mod message;
mod midi;
mod msc;
mod scheduler;

pub use fail_safe::FailSafeMonitor;
pub use message::{midi_data_len, MessageType, TransportMessage, PITCH_BEND_CENTER};
pub use midi::{
    all_notes_off, fail_safe_burst, message_to_midi, midi_input, midi_to_message,
    pitch_bend_center, sustain_off, MidiInput, MidiOutput,
};
pub use msc::{
    MscCommand, MscCue, MscError, MscMessage, MscTarget, MSC_MAX_CUE_TENTHS, MSC_MAX_LIST,
};
pub use scheduler::{PollResult, TxScheduler};
