use super::config::PhyConfig;
use super::goertzel::{best_tone, goertzel_power};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Search,
    Locked,
    Dead,
}

#[derive(Debug, Clone)]
pub struct DemodResult {
    pub nibble: u8,
    pub power: f32,
    pub confidence_db: f32,
}

#[derive(Debug, Clone)]
pub enum RxEvent {
    /// Demodulated body nibble after a successful preamble lock.
    BodyNibble(DemodResult),
    /// Preamble acquired; body collection starts.
    PreambleLocked { confidence_db: f32 },
    /// Lost lock mid-packet.
    SyncLost,
    /// Link considered dead (no traffic within timeout after prior lock).
    LinkDead,
    Metrics {
        state: LinkState,
        confidence_db: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RxPhase {
    Search,
    /// Collecting remaining preamble symbols after first matched.
    ConfirmPreamble { matched: usize },
    /// Reading body nibbles for the current packet repetition.
    Body { collected: usize, expected: usize },
}

/// Streaming MFSK-16 receiver with preamble search and symbol clock.
pub struct ModemRx {
    config: PhyConfig,
    tones: Vec<f32>,
    buffer: VecDeque<f32>,
    phase: RxPhase,
    link_state: LinkState,
    samples_since_valid: u64,
    ever_locked: bool,
    last_confidence_db: f32,
    /// Expected body nibble count for the current packet (set by caller via `expect_body_nibbles`).
    expected_body_nibbles: usize,
}

impl ModemRx {
    pub fn new(config: PhyConfig) -> Self {
        let tones = config.tone_table();
        Self {
            config,
            tones,
            buffer: VecDeque::with_capacity(4096),
            phase: RxPhase::Search,
            link_state: LinkState::Search,
            samples_since_valid: 0,
            ever_locked: false,
            last_confidence_db: 0.0,
            expected_body_nibbles: 16, // default; encoder pads to multiple of interleave depth
        }
    }

    pub fn config(&self) -> &PhyConfig {
        &self.config
    }

    pub fn link_state(&self) -> LinkState {
        self.link_state
    }

    pub fn last_confidence_db(&self) -> f32 {
        self.last_confidence_db
    }

    pub fn set_expected_body_nibbles(&mut self, n: usize) {
        self.expected_body_nibbles = n;
    }

    pub fn reset(&mut self) {
        self.phase = RxPhase::Search;
        self.link_state = LinkState::Search;
        self.buffer.clear();
        self.samples_since_valid = 0;
        self.ever_locked = false;
    }

    pub fn push_samples(&mut self, samples: &[f32]) -> Vec<RxEvent> {
        for &s in samples {
            self.buffer.push_back(s);
        }
        self.samples_since_valid = self.samples_since_valid.saturating_add(samples.len() as u64);
        self.process()
    }

    fn process(&mut self) -> Vec<RxEvent> {
        let mut events = Vec::new();
        let sym = self.config.symbol_samples();
        let period = self.config.samples_per_symbol_period();
        let hop = self.config.hop_samples();
        let dead_samples =
            (self.config.link_dead_ms as u64) * (self.config.sample_rate_hz as u64) / 1000;

        if self.ever_locked
            && self.link_state != LinkState::Dead
            && self.samples_since_valid > dead_samples
        {
            self.link_state = LinkState::Dead;
            self.phase = RxPhase::Search;
            events.push(RxEvent::LinkDead);
        }

        loop {
            match self.phase {
                RxPhase::Search => {
                    // Need the full fine-alignment search range (up to `hop`) plus a
                    // full symbol+gap period behind it, so that a preamble[0] hit is
                    // always aligned before locking. Deciding with less would make the
                    // alignment depend on how the caller chunks its input.
                    if self.buffer.len() < period + hop {
                        break;
                    }
                    let window: Vec<f32> = self.buffer.iter().take(sym).copied().collect();
                    if let Some(det) = self.detect_nibble(&window) {
                        if det.nibble == self.config.preamble[0]
                            && det.power >= self.config.absolute_power_min
                            && det.confidence_db >= self.config.min_confidence_db
                        {
                            // Fine-align within hop window
                            let align = self.best_preamble0_offset(sym, hop);
                            debug_assert!(align <= hop && align + period <= self.buffer.len());
                            if align > 0 {
                                self.buffer.drain(0..align);
                                continue;
                            }
                            self.consume(period);
                            self.phase = RxPhase::ConfirmPreamble { matched: 1 };
                            self.last_confidence_db = det.confidence_db;
                            continue;
                        }
                    }
                    let advance = hop.min(self.buffer.len());
                    self.buffer.drain(0..advance);
                }
                RxPhase::ConfirmPreamble { matched } => {
                    if self.buffer.len() < period {
                        break;
                    }
                    let window: Vec<f32> = self.buffer.iter().take(sym).copied().collect();
                    let expect = self.config.preamble[matched];
                    if let Some(det) = self.detect_nibble(&window) {
                        if det.nibble == expect
                            && det.power >= self.config.absolute_power_min
                            && det.confidence_db >= self.config.min_confidence_db * 0.5
                        {
                            self.consume(period);
                            let next = matched + 1;
                            self.last_confidence_db = det.confidence_db;
                            if next >= self.config.preamble.len() {
                                self.phase = RxPhase::Body {
                                    collected: 0,
                                    expected: self.expected_body_nibbles,
                                };
                                self.link_state = LinkState::Locked;
                                self.ever_locked = true;
                                self.samples_since_valid = 0;
                                events.push(RxEvent::PreambleLocked {
                                    confidence_db: det.confidence_db,
                                });
                            } else {
                                self.phase = RxPhase::ConfirmPreamble { matched: next };
                            }
                            continue;
                        }
                    }
                    // Preamble mismatch → back to search
                    self.phase = RxPhase::Search;
                    self.link_state = LinkState::Search;
                    events.push(RxEvent::SyncLost);
                    let advance = hop.min(self.buffer.len());
                    self.buffer.drain(0..advance);
                }
                RxPhase::Body {
                    collected,
                    expected,
                } => {
                    if self.buffer.len() < period {
                        break;
                    }
                    let window: Vec<f32> = self.buffer.iter().take(sym).copied().collect();
                    if let Some(det) = self.detect_nibble(&window) {
                        if det.power >= self.config.absolute_power_min
                            && det.confidence_db >= self.config.min_confidence_db * 0.5
                        {
                            self.consume(period);
                            self.last_confidence_db = det.confidence_db;
                            self.samples_since_valid = 0;
                            events.push(RxEvent::BodyNibble(det));
                            let next = collected + 1;
                            if next >= expected {
                                self.phase = RxPhase::Search;
                            } else {
                                self.phase = RxPhase::Body {
                                    collected: next,
                                    expected,
                                };
                            }
                            continue;
                        }
                    }
                    // Missed body symbol
                    self.phase = RxPhase::Search;
                    self.link_state = LinkState::Search;
                    events.push(RxEvent::SyncLost);
                    let advance = hop.min(self.buffer.len());
                    self.buffer.drain(0..advance);
                }
            }
        }

        events.push(RxEvent::Metrics {
            state: self.link_state,
            confidence_db: self.last_confidence_db,
        });
        events
    }

    fn detect_nibble(&self, window: &[f32]) -> Option<DemodResult> {
        let (nibble, power, confidence_db) =
            best_tone(window, &self.tones, self.config.sample_rate_hz)?;
        Some(DemodResult {
            nibble,
            power,
            confidence_db,
        })
    }

    fn best_preamble0_offset(&self, sym: usize, hop: usize) -> usize {
        let target = self.config.tone_hz(self.config.preamble[0]);
        let max_offset = hop.min(self.buffer.len().saturating_sub(sym));
        let step = (hop / 2).max(1);
        let mut best_offset = 0usize;
        let mut best_power = 0.0f32;
        let mut offset = 0usize;
        while offset <= max_offset {
            let window: Vec<f32> = self
                .buffer
                .iter()
                .skip(offset)
                .take(sym)
                .copied()
                .collect();
            let power = goertzel_power(&window, target, self.config.sample_rate_hz);
            if power > best_power {
                best_power = power;
                best_offset = offset;
            }
            offset += step;
        }
        best_offset
    }

    fn consume(&mut self, n: usize) {
        let n = n.min(self.buffer.len());
        self.buffer.drain(0..n);
    }
}
