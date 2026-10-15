use crate::link::{
    body_nibble_count, bytes_to_nibbles, decode_packet_bytes, deinterleave, encode_packet_bytes,
    fec_candidate_is_valid, interleave, nibbles_to_bytes, BodySymbol, FecCodec, Packet,
};
use crate::phy::{LinkState, Modulation, PhyConfig, PhyRx, PhyTx, RxEvent};
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
    /// Body symbols repaired by FEC (0 unless `PhyConfig::fec` is enabled).
    pub fec_corrected: u64,
}

#[derive(Debug, Clone)]
pub enum ModemOutput {
    Message(TransportMessage),
    Midi(Vec<u8>),
    LinkDead,
    SyncLost,
    Stats(ModemStats),
}

/// High-level TX: transport message → repeated interleaved audio (MFSK, or
/// the experimental BMC phy, per `config.modulation`).
pub struct CricketModemTx {
    phy: PhyTx,
    config: PhyConfig,
    seq: u8,
    fec: Option<FecCodec>,
}

impl CricketModemTx {
    pub fn new(config: PhyConfig) -> Self {
        let phy = PhyTx::new(config.clone());
        let fec = FecCodec::new(config.fec);
        Self {
            phy,
            config,
            seq: 0,
            fec,
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
        let interleaved = if let Some(fec) = &self.fec {
            // FEC replaces padding + block interleave (codeword interleave).
            fec.encode_body(&nibbles)
        } else {
            while nibbles.len() < MAX_BODY_NIBBLES {
                nibbles.push(0);
            }
            nibbles.truncate(MAX_BODY_NIBBLES);
            interleave(&nibbles, self.config.interleave_depth)
        };

        let mut audio = Vec::new();
        for _ in 0..self.config.repetition_k {
            audio.extend(self.phy.synthesize_framed(&interleaved));
            // Small gap between repetitions
            let gap = repetition_gap_samples(&self.config);
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
/// The body is [`MAX_BODY_NIBBLES`], or the coded length when FEC is enabled.
pub fn repetition_samples(config: &PhyConfig) -> u64 {
    let body_nibbles = config.fec.body_nibbles();
    if let Modulation::Bmc(p) = &config.modulation {
        return p.repetition_samples(config.sample_rate_hz, body_nibbles) as u64;
    }
    ((config.preamble.len() + body_nibbles + 1) * config.samples_per_symbol_period()) as u64
}

/// Silence `CricketModemTx` appends after each repetition: one symbol
/// period (MFSK) or `gap_bits` cells (BMC).
pub fn repetition_gap_samples(config: &PhyConfig) -> usize {
    match &config.modulation {
        Modulation::Mfsk => config.samples_per_symbol_period(),
        Modulation::Bmc(p) => p.gap_samples(config.sample_rate_hz),
    }
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
    phy: PhyRx,
    config: PhyConfig,
    body_nibbles: Vec<u8>,
    /// Per-symbol detail for the FEC decoder (filled only when FEC is on).
    body_symbols: Vec<BodySymbol>,
    body_len: usize,
    fec: Option<FecCodec>,
    fec_corrected: u64,
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
        let mut phy = PhyRx::new(config.clone());
        let body_len = config.fec.body_nibbles();
        phy.set_expected_body_nibbles(body_len);
        let fail_safe = FailSafeMonitor::new(config.link_dead_ms, config.sample_rate_hz);
        let dedup_window = dedup_window_samples(&config);
        let fec = FecCodec::new(config.fec);
        Self {
            phy,
            config,
            body_nibbles: Vec::with_capacity(body_len),
            body_symbols: Vec::new(),
            body_len,
            fec,
            fec_corrected: 0,
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
            fec_corrected: self.fec_corrected,
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

        let mut events: VecDeque<RxEvent> = self.phy.push_samples(samples).into();
        while let Some(ev) = events.pop_front() {
            match ev {
                RxEvent::PreambleLocked { .. } => {
                    self.collecting = true;
                    self.body_nibbles.clear();
                    self.body_symbols.clear();
                }
                RxEvent::BodyNibble { result, end_sample } => {
                    if self.collecting {
                        self.body_nibbles.push(result.nibble);
                        if self.fec.is_some() {
                            self.body_symbols.push(BodySymbol {
                                nibble: result.nibble,
                                reliability: result.confidence_db,
                                erased: result.erased,
                            });
                        }
                        if self.body_nibbles.len() >= self.body_len {
                            self.collecting = false;
                            // The phy pauses here until told whether the body
                            // decoded; a failure rewinds it so a packet that
                            // starts inside a false lock is not lost.
                            let more = if self.try_decode_body(end_sample, &mut outputs) {
                                self.phy.confirm()
                            } else {
                                self.phy.reject()
                            };
                            events.extend(more);
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
    /// position `end`. Returns whether it held a valid packet (CRC pass,
    /// including duplicates), i.e. whether the phy lock was genuine.
    fn try_decode_body(&mut self, end: u64, outputs: &mut Vec<ModemOutput>) -> bool {
        let bytes = if self.fec.is_some() {
            match self.fec_packet_bytes() {
                Some(bytes) => bytes,
                None => {
                    self.crc_fail += 1;
                    return false;
                }
            }
        } else {
            let depth = self.config.interleave_depth;
            let deinterleaved = deinterleave(&self.body_nibbles, depth);
            nibbles_to_bytes(&deinterleaved)
        };

        // Try trimming trailing pad zeros from the byte stream by attempting
        // each valid message size from the HDR type.
        if bytes.len() < 4 {
            self.crc_fail += 1;
            return false;
        }

        let hdr = bytes[0];
        let type_nibble = hdr & 0x0F;
        let Some(msg_type) = MessageType::from_u8(type_nibble) else {
            return false;
        };
        let needed = 2 + msg_type.payload_len() + 2;
        if bytes.len() < needed {
            self.crc_fail += 1;
            return false;
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
                    return true;
                }
                self.recent.push_back((slice.to_vec(), end));
                self.last_seq = Some(packet.seq);

                let Some(msg) =
                    TransportMessage::from_type_payload(packet.msg_type, &packet.payload)
                else {
                    return true;
                };
                // Panic: the burst below is the only one; the phy is confirmed
                // past this packet (back in search) right after. The duplicate history is
                // kept so the next repetition of the Panic is suppressed.
                outputs.extend(message_to_midi(&msg).into_iter().map(ModemOutput::Midi));
                outputs.push(ModemOutput::Message(msg));
                true
            }
            Err(_) => {
                self.crc_fail += 1;
                false
            }
        }
    }
}

impl CricketModemRx {
    /// FEC-decode the collected body into packet bytes (+ zero pad). A
    /// candidate is accepted only if its packet passes CRC-16 / version /
    /// type checks and its pad bytes are zero.
    fn fec_packet_bytes(&mut self) -> Option<Vec<u8>> {
        let fec = self.fec.as_ref()?;
        let decoded = fec.decode_body(&self.body_symbols, fec_candidate_is_valid)?;
        self.fec_corrected += decoded.corrected as u64;
        Some(nibbles_to_bytes(&decoded.nibbles))
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
