//! Transport layer: messages, MIDI mapping, fail-safe policy.

mod fail_safe;
mod message;
mod midi;

pub use fail_safe::{FailSafeAction, FailSafeMonitor};
pub use message::{MessageType, MscCommand, TransportMessage};
pub use midi::{midi_to_message, message_to_midi};
