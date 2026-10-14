//! Transport layer: messages, MIDI mapping, fail-safe policy.

mod fail_safe;
mod message;
mod midi;
mod scheduler;

pub use fail_safe::FailSafeMonitor;
pub use message::{MessageType, MscCommand, TransportMessage};
pub use midi::{all_notes_off, fail_safe_burst, message_to_midi, midi_to_message, sustain_off};
pub use scheduler::{PollResult, TxScheduler};
