//! Events workers report to their owner (GUI, CLI, tests).

use aml_core::phy::LinkState;
use aml_core::transport::TransportMessage;

/// Something a decoder or encoder worker reports. Delivered in order on the
/// handle's event channel; [`EngineEvent::Stopped`] is always the last one.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// Human-readable status line.
    Log(String),
    /// Decoder input RMS level, reported every 1/20 s of audio.
    Level(f32),
    /// A decoded message (including heartbeats).
    Decoded(TransportMessage),
    /// Decoder link statistics, at most every 200 ms.
    Stats(LinkStats),
    /// A condition the operator should see.
    Alarm { kind: AlarmKind, message: String },
    /// The condition behind an earlier [`EngineEvent::Alarm`] of `kind` has
    /// ended (e.g. audio input resumed after a stall). Only sent for kinds
    /// whose end is observable: currently [`AlarmKind::AudioInputStalled`].
    AlarmCleared { kind: AlarmKind, message: String },
    /// Encoder queue status, about every 200 ms. Best-effort for now:
    /// `queued` = MIDI messages waiting to be encoded, `latency_ms` = audio
    /// already queued ahead of the output (what a new message waits behind).
    TxStatus { queued: usize, latency_ms: f32 },
    /// The worker has finished; no further events follow.
    Stopped,
}

/// Decoder link statistics (same fields the GUI's `link_stats` event carries).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkStats {
    pub state: LinkState,
    pub crc_pass: u64,
    pub crc_fail: u64,
    pub confidence_db: f32,
    pub last_seq: Option<u8>,
    pub sync_lost: u64,
    /// Percentage 0–100 (100 before any packet).
    pub crc_pass_rate: f32,
    pub est_latency_ms: f32,
    pub link_alive: bool,
    /// Input samples lost because the capture buffer was full.
    pub buffer_drops: u64,
}

/// Alarm categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmKind {
    /// No valid packet for `link_dead_ms`; All Notes Off was sent.
    LinkDead,
    /// The audio input delivered no samples for `link_dead_ms` (device
    /// unplugged, driver stall). If the link was alive, All Notes Off was
    /// sent. Cleared by [`EngineEvent::AlarmCleared`] when samples resume.
    AudioInputStalled,
    /// The audio device (input or output stream) reported an error.
    AudioDeviceError,
}

impl AlarmKind {
    /// Stable identifier used by the GUI (`"link_dead"`,
    /// `"audio_input_stalled"`, `"audio_device_error"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            AlarmKind::LinkDead => "link_dead",
            AlarmKind::AudioInputStalled => "audio_input_stalled",
            AlarmKind::AudioDeviceError => "audio_device_error",
        }
    }
}

/// Display name of a link state ("Search" / "Locked" / "Dead").
pub fn link_state_name(state: LinkState) -> &'static str {
    match state {
        LinkState::Search => "Search",
        LinkState::Locked => "Locked",
        LinkState::Dead => "Dead",
    }
}

/// `(frame_type, payload)` display strings for a message.
pub fn format_message(msg: &TransportMessage) -> (String, String) {
    match msg {
        TransportMessage::NoteOn { note, velocity } => {
            ("NoteOn".into(), format!("note={} vel={}", note, velocity))
        }
        TransportMessage::NoteOff { note } => ("NoteOff".into(), format!("note={}", note)),
        TransportMessage::ProgramChange { program } => {
            ("ProgramChange".into(), format!("program={}", program))
        }
        TransportMessage::MscLite { cmd, cuelist, cue } => (
            "MscLite".into(),
            format!("{:?} cuelist={} cue={}", cmd, cuelist, cue),
        ),
        TransportMessage::Heartbeat => ("Heartbeat".into(), String::new()),
        TransportMessage::Panic => ("Panic".into(), String::new()),
    }
}
