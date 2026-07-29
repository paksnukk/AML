use crate::link::{
    body_nibble_count, bytes_to_nibbles, decode_packet_bytes, deinterleave, encode_packet_bytes,
    interleave, nibbles_to_bytes, Packet,
};
use crate::phy::{LinkState, ModemRx, ModemTx, PhyConfig, RxEvent};
use crate::transport::{
    message_to_midi, FailSafeAction, FailSafeMonitor, MessageType, TransportMessage,
};

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
pub struct AmlModemTx {
    phy: ModemTx,
    config: PhyConfig,
    seq: u8,
}

impl AmlModemTx {
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
            audio.extend(std::iter::repeat(0.0).take(gap));
        }
        audio
    }

    pub fn encode_heartbeat(&mut self) -> Vec<f32> {
        self.encode_message(&TransportMessage::Heartbeat)
    }
}

/// High-level RX: audio samples → transport messages + fail-safe.
pub struct AmlModemRx {
    phy: ModemRx,
    config: PhyConfig,
    body_nibbles: Vec<u8>,
    collecting: bool,
    last_seq: Option<u8>,
    crc_pass: u64,
    crc_fail: u64,
    fail_safe: FailSafeMonitor,
}

impl AmlModemRx {
    pub fn new(config: PhyConfig) -> Self {
        let mut phy = ModemRx::new(config.clone());
        phy.set_expected_body_nibbles(MAX_BODY_NIBBLES);
        let fail_safe = FailSafeMonitor::new(config.link_dead_ms, config.sample_rate_hz);
        Self {
            phy,
            config,
            body_nibbles: Vec::with_capacity(MAX_BODY_NIBBLES),
            collecting: false,
            last_seq: None,
            crc_pass: 0,
            crc_fail: 0,
            fail_safe,
        }
    }

    pub fn stats(&self) -> ModemStats {
        ModemStats {
            link_state: self.phy.link_state(),
            crc_pass: self.crc_pass,
            crc_fail: self.crc_fail,
            last_confidence_db: self.phy.last_confidence_db(),
            last_seq: self.last_seq,
        }
    }

    pub fn reset(&mut self) {
        self.phy.reset();
        self.body_nibbles.clear();
        self.collecting = false;
    }

    pub fn push_samples(&mut self, samples: &[f32]) -> Vec<ModemOutput> {
        let mut outputs = Vec::new();

        for action in self.fail_safe.on_samples(samples.len()) {
            match action {
                FailSafeAction::AllNotesOff => {
                    outputs.push(ModemOutput::Midi(FailSafeMonitor::midi_for_action(&action)));
                    outputs.push(ModemOutput::LinkDead);
                }
                FailSafeAction::ResetDecoder => {
                    self.reset();
                }
            }
        }

        let events = self.phy.push_samples(samples);
        for ev in events {
            match ev {
                RxEvent::PreambleLocked { .. } => {
                    self.collecting = true;
                    self.body_nibbles.clear();
                }
                RxEvent::BodyNibble(det) => {
                    if self.collecting {
                        self.body_nibbles.push(det.nibble);
                        if self.body_nibbles.len() >= MAX_BODY_NIBBLES {
                            self.collecting = false;
                            if let Some(out) = self.try_decode_body() {
                                outputs.extend(out);
                            }
                        }
                    }
                }
                RxEvent::SyncLost => {
                    self.collecting = false;
                    self.body_nibbles.clear();
                    outputs.push(ModemOutput::SyncLost);
                }
                RxEvent::LinkDead => {
                    outputs.push(ModemOutput::Midi(
                        FailSafeMonitor::midi_for_action(&FailSafeAction::AllNotesOff),
                    ));
                    outputs.push(ModemOutput::LinkDead);
                    self.reset();
                }
                RxEvent::Metrics { .. } => {}
            }
        }

        outputs.push(ModemOutput::Stats(self.stats()));
        outputs
    }

    fn try_decode_body(&mut self) -> Option<Vec<ModemOutput>> {
        let depth = self.config.interleave_depth;
        let deinterleaved = deinterleave(&self.body_nibbles, depth);
        let bytes = nibbles_to_bytes(&deinterleaved);

        // Try trimming trailing pad zeros from the byte stream by attempting
        // each valid message size from the HDR type.
        if bytes.len() < 4 {
            self.crc_fail += 1;
            return None;
        }

        let hdr = bytes[0];
        let type_nibble = hdr & 0x0F;
        let msg_type = MessageType::from_u8(type_nibble)?;
        let needed = 2 + msg_type.payload_len() + 2;
        if bytes.len() < needed {
            self.crc_fail += 1;
            return None;
        }
        let slice = &bytes[..needed];
        match decode_packet_bytes(slice) {
            Ok(packet) => {
                self.crc_pass += 1;
                self.fail_safe.on_valid_packet();

                // Duplicate suppression
                if self.last_seq == Some(packet.seq) {
                    return Some(Vec::new());
                }
                self.last_seq = Some(packet.seq);

                let msg = TransportMessage::from_type_payload(packet.msg_type, &packet.payload)?;
                let mut outs = Vec::new();

                if matches!(msg, TransportMessage::Panic) {
                    outs.push(ModemOutput::Midi(message_to_midi(&msg)));
                    outs.push(ModemOutput::Message(msg));
                    self.reset();
                    return Some(outs);
                }

                let midi = message_to_midi(&msg);
                if !midi.is_empty() {
                    outs.push(ModemOutput::Midi(midi));
                }
                outs.push(ModemOutput::Message(msg));
                Some(outs)
            }
            Err(_) => {
                self.crc_fail += 1;
                None
            }
        }
    }
}

/// Offline helpers used by tests and CLI.
pub fn encode_to_samples(config: PhyConfig, messages: &[TransportMessage]) -> Vec<f32> {
    let mut tx = AmlModemTx::new(config);
    let mut all = Vec::new();
    for msg in messages {
        all.extend(tx.encode_message(msg));
        // Inter-message silence
        all.extend(std::iter::repeat(0.0).take(tx.config().samples_per_symbol_period() * 2));
    }
    all
}

pub fn decode_samples(config: PhyConfig, samples: &[f32]) -> Vec<TransportMessage> {
    let mut rx = AmlModemRx::new(config);
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
    n.max(MAX_BODY_NIBBLES).min(MAX_BODY_NIBBLES)
}
