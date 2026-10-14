use crate::link::{
    body_nibble_count, bytes_to_nibbles, decode_packet_bytes, deinterleave, encode_packet_bytes,
    interleave, nibbles_to_bytes, Packet,
};
use crate::phy::{LinkState, ModemRx, ModemTx, PhyConfig, RxEvent};
use crate::transport::{
    fail_safe_burst, message_to_midi, FailSafeMonitor, MessageType, TransportMessage,
};
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

/// Fixed interleaved body size so RX can lock without knowing the type early.
pub const MAX_BODY_NIBBLES: usize = 16;

#[derive(Debug, Clone)]
pub struct ModemStats {
    pub link_state: LinkState,
    pub crc_pass: u64,
    pub crc_fail: u64,
    pub last_confidence_db: f32,
    pub last_seq: Option<u8>,
}

#[derive(Debug, Clone)]
pub enum ModemOutput {
    Message(TransportMessage),
    Midi(Vec<u8>),
    LinkDead,
    SyncLost,
    Stats(ModemStats),
}

/// High-level TX: transport message → repeated interleaved MFSK audio.
pub struct CricketModemTx {
    phy: ModemTx,
    config: PhyConfig,
    seq: u8,
}

impl CricketModemTx {
    pub fn new(config: PhyConfig) -> Self {
        let phy = ModemTx::new(config.clone());
        Self {
            phy,
            config,
            seq: 0,
        }
    }

    pub fn config(&self) -> &PhyConfig {
        &self.config
    }

    pub fn next_seq(&mut self) -> u8 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    pub fn encode_message(&mut self, msg: &TransportMessage) -> Vec<f32> {
        let seq = self.next_seq();
        self.encode_message_with_seq(msg, seq)
    }

    pub fn encode_message_with_seq(&self, msg: &TransportMessage, seq: u8) -> Vec<f32> {
        let packet = Packet {
            msg_type: msg.msg_type(),
            seq,
            payload: msg.to_payload(),
        };
        let bytes = encode_packet_bytes(&packet).expect("valid packet");
        let mut nibbles = bytes_to_nibbles(&bytes);
        while nibbles.len() < MAX_BODY_NIBBLES {
            nibbles.push(0);
        }
        nibbles.truncate(MAX_BODY_NIBBLES);
        let interleaved = interleave(&nibbles, self.config.interleave_depth);

        let mut audio = Vec::new();
        for _ in 0..self.config.repetition_k {
            audio.extend(self.phy.synthesize_framed(&interleaved));
            // Small gap between repetitions
            let gap = self.config.samples_per_symbol_period();
            audio.extend(core::iter::repeat_n(0.0, gap));
        }
        audio
    }

    pub fn encode_heartbeat(&mut self) -> Vec<f32> {
        self.encode_message(&TransportMessage::Heartbeat)
    }
}

/// Samples one on-wire repetition occupies: preamble + body symbol periods
/// plus the one-period gap `CricketModemTx` inserts after each repetition.
pub fn repetition_samples(config: &PhyConfig) -> u64 {
    ((config.preamble.len() + MAX_BODY_NIBBLES + 1) * config.samples_per_symbol_period()) as u64
}

/// Duplicate-suppression window in samples (docs/aml-v1-spec.md §4.4): one
/// full transmission of K repetitions plus half a repetition of margin.
/// Robust: (2 + 0.5) x 6552 = 16380 samples (~341 ms @ 48 kHz);
/// Fast: (1 + 0.5) x 2736 = 4104 samples (~86 ms).
pub fn dedup_window_samples(config: &PhyConfig) -> u64 {
    let rep = repetition_samples(config);
    rep * config.repetition_k.max(1) as u64 + rep / 2
}

/// High-level RX: audio samples → transport messages + fail-safe.
///
/// Link state: `ModemRx` reports phy lock (`Search` / `Locked`); this type
/// owns link-dead. `stats().link_state` is `Dead` from the moment link-dead
/// fires until the next CRC-valid packet.
///
/// Link-dead timing is in absolute stream samples (see [`FailSafeMonitor`]):
/// it fires at sample `end_of_last_valid_packet + link_dead_ms`, independent
/// of chunking. It is reported by the `push_samples` call whose input contains
/// that sample (resolution: one chunk); the exact sample is available from
/// [`CricketModemRx::last_link_dead_sample`].
pub struct CricketModemRx {
    phy: ModemRx,
    config: PhyConfig,
    body_nibbles: Vec<u8>,
    collecting: bool,
    last_seq: Option<u8>,
    crc_pass: u64,
    crc_fail: u64,
    fail_safe: FailSafeMonitor,
    link_dead: bool,
    last_link_dead_sample: Option<u64>,
    /// Recently accepted packets (raw bytes, end sample) for duplicate suppression.
    recent: VecDeque<(Vec<u8>, u64)>,
    dedup_window: u64,
}

impl CricketModemRx {
    pub fn new(config: PhyConfig) -> Self {
        let mut phy = ModemRx::new(config.clone());
        phy.set_expected_body_nibbles(MAX_BODY_NIBBLES);
        let fail_safe = FailSafeMonitor::new(config.link_dead_ms, config.sample_rate_hz);
        let dedup_window = dedup_window_samples(&config);
        Self {
            phy,
            config,
            body_nibbles: Vec::with_capacity(MAX_BODY_NIBBLES),
            collecting: false,
            last_seq: None,
            crc_pass: 0,
            crc_fail: 0,
            fail_safe,
            link_dead: false,
            last_link_dead_sample: None,
            recent: VecDeque::new(),
            dedup_window,
        }
    }

    pub fn stats(&self) -> ModemStats {
        ModemStats {
            link_state: if self.link_dead {
                LinkState::Dead
            } else {
                self.phy.link_state()
            },
            crc_pass: self.crc_pass,
            crc_fail: self.crc_fail,
            last_confidence_db: self.phy.last_confidence_db(),
            last_seq: self.last_seq,
        }
    }

    /// Total samples pushed so far (absolute stream position).
    pub fn samples_processed(&self) -> u64 {
        self.phy.samples_pushed()
    }

    /// Stream sample at which the most recent link-dead fired.
    pub fn last_link_dead_sample(&self) -> Option<u64> {
        self.last_link_dead_sample
    }

    /// True while link-dead is pending: a CRC-valid packet has been received
    /// and link-dead has not fired (or been declared) since.
    pub fn is_link_armed(&self) -> bool {
        self.fail_safe.is_armed()
    }

    /// Declare the link dead from outside the sample clock, e.g. when a
    /// wall-clock watchdog sees the audio input stop delivering samples.
    ///
    /// Same effect as link-dead firing (state `Dead`, duplicate history
    /// cleared, monitor disarmed until the next valid packet) except that
    /// **no output is produced**: the caller sends the fail-safe burst itself.
    /// Because the monitor is disarmed, the same outage does not fire a second
    /// burst when samples resume. Returns false (and does nothing) if the link
    /// was not armed.
    pub fn declare_link_dead(&mut self) -> bool {
        if !self.fail_safe.disarm() {
            return false;
        }
        self.link_dead = true;
        self.last_link_dead_sample = Some(self.samples_processed());
        self.recent.clear();
        true
    }

    /// Hard reset of the decoder: phy back to search with its buffer flushed.
    /// Not used internally (flushing would make timing depend on chunking).
    pub fn reset(&mut self) {
        self.phy.reset();
        self.body_nibbles.clear();
        self.collecting = false;
    }

    pub fn push_samples(&mut self, samples: &[f32]) -> Vec<ModemOutput> {
        let mut outputs = Vec::new();

        let events = self.phy.push_samples(samples);
        for ev in events {
            match ev {
                RxEvent::PreambleLocked { .. } => {
                    self.collecting = true;
                    self.body_nibbles.clear();
                }
                RxEvent::BodyNibble { result, end_sample } => {
                    if self.collecting {
                        self.body_nibbles.push(result.nibble);
                        if self.body_nibbles.len() >= MAX_BODY_NIBBLES {
                            self.collecting = false;
                            self.try_decode_body(end_sample, &mut outputs);
                        }
                    }
                }
                RxEvent::SyncLost => {
                    self.collecting = false;
                    self.body_nibbles.clear();
                    outputs.push(ModemOutput::SyncLost);
                }
                RxEvent::Metrics { .. } => {}
            }
        }
        self.poll_fail_safe(self.phy.samples_pushed(), &mut outputs);

        outputs.push(ModemOutput::Stats(self.stats()));
        outputs
    }

    /// Fire link-dead if its deadline sample lies before stream position `now`.
    /// Emits one fail-safe burst (CC64=0, CC123) and `LinkDead`, then clears
    /// the duplicate history. The phy is not flushed: during silence it is
    /// already searching, and a packet still in flight may revive the link.
    fn poll_fail_safe(&mut self, now: u64, outputs: &mut Vec<ModemOutput>) {
        if let Some(at) = self.fail_safe.poll(now) {
            outputs.extend(fail_safe_burst().into_iter().map(ModemOutput::Midi));
            outputs.push(ModemOutput::LinkDead);
            self.link_dead = true;
            self.last_link_dead_sample = Some(at);
            self.recent.clear();
        }
    }

    /// Duplicate if the same packet bytes (same SEQ, type and payload) were
    /// accepted within the dedup window before `end`.
    fn is_duplicate(&mut self, bytes: &[u8], end: u64) -> bool {
        let window = self.dedup_window;
        self.recent
            .retain(|(_, at)| end.saturating_sub(*at) <= window);
        self.recent.iter().any(|(b, _)| b.as_slice() == bytes)
    }

    /// Decode the collected body; the last body symbol ended at stream
    /// position `end`.
    fn try_decode_body(&mut self, end: u64, outputs: &mut Vec<ModemOutput>) {
        let depth = self.config.interleave_depth;
        let deinterleaved = deinterleave(&self.body_nibbles, depth);
        let bytes = nibbles_to_bytes(&deinterleaved);

        // Try trimming trailing pad zeros from the byte stream by attempting
        // each valid message size from the HDR type.
        if bytes.len() < 4 {
            self.crc_fail += 1;
            return;
        }

        let hdr = bytes[0];
        let type_nibble = hdr & 0x0F;
        let Some(msg_type) = MessageType::from_u8(type_nibble) else {
            return;
        };
        let needed = 2 + msg_type.payload_len() + 2;
        if bytes.len() < needed {
            self.crc_fail += 1;
            return;
        }
        let slice = &bytes[..needed];
        match decode_packet_bytes(slice) {
            Ok(packet) => {
                self.crc_pass += 1;
                // A deadline that passed before this packet ended fires first.
                self.poll_fail_safe(end, outputs);
                self.fail_safe.on_valid_packet(end);
                self.link_dead = false;

                if self.is_duplicate(slice, end) {
                    return;
                }
                self.recent.push_back((slice.to_vec(), end));
                self.last_seq = Some(packet.seq);

                let Some(msg) =
                    TransportMessage::from_type_payload(packet.msg_type, &packet.payload)
                else {
                    return;
                };
                // Panic: the burst below is the only one; the phy has finished
                // this packet and is back in search. The duplicate history is
                // kept so the next repetition of the Panic is suppressed.
                outputs.extend(message_to_midi(&msg).into_iter().map(ModemOutput::Midi));
                outputs.push(ModemOutput::Message(msg));
            }
            Err(_) => {
                self.crc_fail += 1;
            }
        }
    }
}

/// Offline helpers used by tests and CLI.
pub fn encode_to_samples(config: PhyConfig, messages: &[TransportMessage]) -> Vec<f32> {
    let mut tx = CricketModemTx::new(config);
    let mut all = Vec::new();
    for msg in messages {
        all.extend(tx.encode_message(msg));
        // Inter-message silence
        all.extend(core::iter::repeat_n(
            0.0,
            tx.config().samples_per_symbol_period() * 2,
        ));
    }
    all
}

pub fn decode_samples(config: PhyConfig, samples: &[f32]) -> Vec<TransportMessage> {
    let mut rx = CricketModemRx::new(config);
    let mut messages = Vec::new();
    // Process in chunks to simulate streaming
    const CHUNK: usize = 256;
    for chunk in samples.chunks(CHUNK) {
        for out in rx.push_samples(chunk) {
            if let ModemOutput::Message(m) = out {
                messages.push(m);
            }
        }
    }
    // Flush remaining by pushing silence
    let silence = vec![0.0; rx.config.samples_per_symbol_period() * 8];
    for out in rx.push_samples(&silence) {
        if let ModemOutput::Message(m) = out {
            messages.push(m);
        }
    }
    messages
}

pub fn expected_body_nibbles_for(msg_type: MessageType, interleave_depth: usize) -> usize {
    let n = body_nibble_count(msg_type, interleave_depth);
    // Note: always MAX_BODY_NIBBLES (bodies are padded to that length).
    n.clamp(MAX_BODY_NIBBLES, MAX_BODY_NIBBLES)
}
