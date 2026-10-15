//! Events workers report to their owner (GUI, CLI, tests).

use cricket_core::phy::LinkState;
use cricket_core::transport::{TransportMessage, PITCH_BEND_CENTER};

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
    /// Encoder queue status, about every 200 ms. `queued` = messages waiting
    /// in the TX scheduler, `latency_ms` = how long the oldest of them has
    /// already waited plus the audio buffered ahead of the output.
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
    /// The encoder received a message it must not send because it cannot
    /// carry it exactly (e.g. an MSC cue with more than one decimal). The
    /// message was dropped.
    MessageRejected,
}

impl AlarmKind {
    /// Stable identifier used by the GUI (`"link_dead"`,
    /// `"audio_input_stalled"`, `"audio_device_error"`, `"message_rejected"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            AlarmKind::LinkDead => "link_dead",
            AlarmKind::AudioInputStalled => "audio_input_stalled",
            AlarmKind::AudioDeviceError => "audio_device_error",
            AlarmKind::MessageRejected => "message_rejected",
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

/// `(frame_type, payload)` display strings for a message. MIDI channels
/// are shown 1–16.
pub fn format_message(msg: &TransportMessage) -> (String, String) {
    use TransportMessage as M;
    let (kind, payload) = match *msg {
        M::NoteOn {
            channel,
            note,
            velocity,
        } => (
            "NoteOn",
            format!("ch={} note={} vel={}", channel + 1, note, velocity),
        ),
        M::NoteOff {
            channel,
            note,
            velocity,
        } => (
            "NoteOff",
            format!("ch={} note={} vel={}", channel + 1, note, velocity),
        ),
        M::PolyPressure {
            channel,
            note,
            pressure,
        } => (
            "PolyPressure",
            format!("ch={} note={} pressure={}", channel + 1, note, pressure),
        ),
        M::ControlChange {
            channel,
            controller,
            value,
        } => (
            "ControlChange",
            format!("ch={} cc={} value={}", channel + 1, controller, value),
        ),
        M::ProgramChange { channel, program } => (
            "ProgramChange",
            format!("ch={} program={}", channel + 1, program),
        ),
        M::ChannelPressure { channel, pressure } => (
            "ChannelPressure",
            format!("ch={} pressure={}", channel + 1, pressure),
        ),
        M::PitchBend { channel, value } => (
            "PitchBend",
            format!(
                "ch={} value={:+}",
                channel + 1,
                i32::from(value) - i32::from(PITCH_BEND_CENTER)
            ),
        ),
        M::SongPosition { beats } => ("SongPosition", format!("beats={}", beats)),
        M::SongSelect { song } => ("SongSelect", format!("song={}", song)),
        M::TuneRequest => ("TuneRequest", String::new()),
        M::Msc(m) => ("MSC", m.to_string()),
        M::Heartbeat => ("Heartbeat", String::new()),
        M::Panic => ("Panic", String::new()),
    };
    (kind.into(), payload)
}
