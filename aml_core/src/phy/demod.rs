use super::config::PhyConfig;
use super::goertzel::{best_tone, goertzel_power, rms_dbfs, ToneDetection};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Search,
    Locked,
    /// No valid packet within `link_dead_ms` after the link was alive. Never
    /// produced by `ModemRx` (phy lock only); set by `AmlModemRx`, which owns
    /// link-dead detection.
    Dead,
}

#[derive(Debug, Clone)]
pub struct DemodResult {
    pub nibble: u8,
    pub power: f32,
    pub confidence_db: f32,
    /// Level-independent tone purity (0..~1).
    pub purity: f32,
}

#[derive(Debug, Clone)]
pub enum RxEvent {
    /// Demodulated body nibble after a successful preamble lock.
    /// `end_sample` is the absolute stream position (samples pushed since
    /// creation) one past the symbol period the nibble was read from.
    BodyNibble {
        result: DemodResult,
        end_sample: u64,
    },
    /// Preamble acquired; body collection starts.
    PreambleLocked { confidence_db: f32 },
    /// Lost lock mid-packet.
    SyncLost,
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
///
/// All positions are absolute stream sample indices. `read` is the start of
/// the next symbol window. The buffer keeps `hop` samples of history behind
/// `read` (for backward fine alignment) and, while a lock attempt is in
/// progress, everything from `hop` before the window that triggered it, so a
/// failed attempt can be rewound: Search resumes at `trigger + hop`, exactly
/// as if the trigger window had been rejected, and no consumed sample is lost.
/// Memory stays bounded by one packet (preamble + body periods) plus history
/// and the caller's chunk.
pub struct ModemRx {
    config: PhyConfig,
    tones: Vec<f32>,
    buffer: VecDeque<f32>,
    phase: RxPhase,
    link_state: LinkState,
    /// Total samples pushed since creation (never reset).
    samples_pushed: u64,
    /// Absolute position of the next symbol window.
    read: u64,
    /// Search window that started the current lock attempt (ConfirmPreamble
    /// or Body); `None` in Search.
    attempt: Option<u64>,
    /// Fine alignment never moves before this position (end of the last
    /// completed packet body), so it cannot slide into a decoded packet.
    align_floor: u64,
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
            samples_pushed: 0,
            read: 0,
            attempt: None,
            align_floor: 0,
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
        self.read = self.samples_pushed;
        self.attempt = None;
        self.align_floor = self.samples_pushed;
    }

    /// Total samples pushed since creation (absolute stream position of the
    /// input head).
    pub fn samples_pushed(&self) -> u64 {
        self.samples_pushed
    }

    /// Absolute stream position of the first sample still buffered.
    fn base(&self) -> u64 {
        self.samples_pushed - self.buffer.len() as u64
    }

    /// Whether samples `[at, at + len)` have been pushed.
    fn available(&self, at: u64, len: usize) -> bool {
        at + len as u64 <= self.samples_pushed
    }

    /// Copy of the buffered samples `[at, at + len)`.
    fn window_at(&self, at: u64, len: usize) -> Vec<f32> {
        let start = (at - self.base()) as usize;
        self.buffer.range(start..start + len).copied().collect()
    }

    /// Drop samples no longer needed: keep `hop` of history behind the
    /// current read position, or behind the trigger of a pending lock attempt.
    fn trim(&mut self, hop: usize) {
        let keep_from = self.attempt.unwrap_or(self.read).min(self.read);
        let keep_from = keep_from.saturating_sub(hop as u64);
        let drop = keep_from.saturating_sub(self.base()) as usize;
        self.buffer.drain(0..drop.min(self.buffer.len()));
    }

    /// Abandon the current lock attempt and rewind Search to just after the
    /// window that triggered it.
    fn fail_attempt(&mut self, hop: usize, events: &mut Vec<RxEvent>) {
        self.phase = RxPhase::Search;
        self.link_state = LinkState::Search;
        events.push(RxEvent::SyncLost);
        let trigger = self.attempt.take().unwrap_or(self.read);
        self.read = trigger + hop as u64;
    }

    fn preamble0_ok(&self, det: &DemodResult) -> bool {
        det.nibble == self.config.preamble[0]
            && self.level_ok(det)
            && det.confidence_db >= self.config.min_confidence_db
    }

    pub fn push_samples(&mut self, samples: &[f32]) -> Vec<RxEvent> {
        for &s in samples {
            self.buffer.push_back(s);
        }
        self.samples_pushed += samples.len() as u64;
        self.process()
    }

    fn process(&mut self) -> Vec<RxEvent> {
        let mut events = Vec::new();
        let sym = self.config.symbol_samples();
        let period = self.config.samples_per_symbol_period();
        let hop = self.config.hop_samples();

        loop {
            match self.phase {
                RxPhase::Search => {
                    if !self.available(self.read, sym) {
                        break;
                    }
                    let trigger = self.read;
                    let hit = self
                        .detect_nibble(&self.window_at(trigger, sym))
                        .is_some_and(|det| self.preamble0_ok(&det));
                    if hit {
                        // Fine alignment looks up to one symbol ahead and the lock
                        // needs a full period behind that. Wait for all of it (the
                        // window is re-detected on the next call) so that alignment
                        // never depends on how the caller chunks its input.
                        if !self.available(trigger, sym + period) {
                            break;
                        }
                        let aligned = self.best_preamble0_position(trigger, sym, hop);
                        debug_assert!(self.available(aligned, period));
                        // Re-check the gate on the aligned window.
                        if let Some(det) = self
                            .detect_nibble(&self.window_at(aligned, sym))
                            .filter(|det| self.preamble0_ok(det))
                        {
                            self.attempt = Some(trigger);
                            self.read = aligned + period as u64;
                            self.phase = RxPhase::ConfirmPreamble { matched: 1 };
                            self.last_confidence_db = det.confidence_db;
                            self.trim(hop);
                            continue;
                        }
                    }
                    self.read = trigger + hop as u64;
                    self.trim(hop);
                }
                RxPhase::ConfirmPreamble { matched } => {
                    if !self.available(self.read, period) {
                        break;
                    }
                    let window = self.window_at(self.read, sym);
                    let expect = self.config.preamble[matched];
                    if let Some(det) = self.detect_nibble(&window) {
                        if det.nibble == expect
                            && self.level_ok(&det)
                            && det.confidence_db >= self.config.min_confidence_db * 0.5
                        {
                            self.read += period as u64;
                            let next = matched + 1;
                            self.last_confidence_db = det.confidence_db;
                            if next >= self.config.preamble.len() {
                                self.phase = RxPhase::Body {
                                    collected: 0,
                                    expected: self.expected_body_nibbles,
                                };
                                self.link_state = LinkState::Locked;
                                events.push(RxEvent::PreambleLocked {
                                    confidence_db: det.confidence_db,
                                });
                            } else {
                                self.phase = RxPhase::ConfirmPreamble { matched: next };
                            }
                            continue;
                        }
                    }
                    // Preamble mismatch → rewind and search again
                    self.fail_attempt(hop, &mut events);
                    self.trim(hop);
                }
                RxPhase::Body {
                    collected,
                    expected,
                } => {
                    if !self.available(self.read, period) {
                        break;
                    }
                    let window = self.window_at(self.read, sym);
                    if let Some(det) = self.detect_nibble(&window) {
                        if self.level_ok(&det)
                            && det.confidence_db >= self.config.min_confidence_db * 0.5
                        {
                            self.read += period as u64;
                            self.last_confidence_db = det.confidence_db;
                            events.push(RxEvent::BodyNibble {
                                result: det,
                                end_sample: self.read,
                            });
                            let next = collected + 1;
                            if next >= expected {
                                self.phase = RxPhase::Search;
                                self.attempt = None;
                                self.align_floor = self.read;
                                self.trim(hop);
                            } else {
                                self.phase = RxPhase::Body {
                                    collected: next,
                                    expected,
                                };
                            }
                            continue;
                        }
                    }
                    // Missed body symbol → rewind and search again
                    self.fail_attempt(hop, &mut events);
                    self.trim(hop);
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
        let ToneDetection {
            index,
            power,
            confidence_db,
            purity,
            energy,
        } = best_tone(window, &self.tones, self.config.sample_rate_hz)?;
        if rms_dbfs(energy, window.len()) < self.config.min_rms_dbfs {
            return None;
        }
        Some(DemodResult {
            nibble: index,
            power,
            confidence_db,
            purity,
        })
    }

    /// Level-independent gate (silence floor is applied in `detect_nibble`).
    fn level_ok(&self, det: &DemodResult) -> bool {
        det.purity >= self.config.min_tone_purity
    }


    /// Symbol-boundary fine alignment around a preamble[0] hit at `trigger`.
    ///
    /// Maximizes the preamble[0] tone's Goertzel power over window start
    /// positions in `[trigger - hop, trigger + sym]`: backward by up to one
    /// search hop (clamped to buffered history and `align_floor`), forward by
    /// up to one symbol, because a window only partly covering the symbol can
    /// already pass the gate (purity is roughly the covered fraction). Coarse
    /// grid of `hop / 2`, then halving refinement around the best point down
    /// to one sample. The power peaks where the window coincides with the
    /// symbol, so the residual error is a sample or two on a clean signal.
    /// Needs samples up to `trigger + 2 * sym`.
    fn best_preamble0_position(&self, trigger: u64, sym: usize, hop: usize) -> u64 {
        let target = self.config.tone_hz(self.config.preamble[0]);
        let lo = trigger
            .saturating_sub(hop as u64)
            .max(self.base())
            .max(self.align_floor.min(trigger));
        let hi = trigger + sym as u64;
        let power =
            |at: u64| goertzel_power(&self.window_at(at, sym), target, self.config.sample_rate_hz);
        let mut best = trigger;
        let mut best_power = power(trigger);
        let mut consider = |at: u64, best: &mut u64| {
            if (lo..=hi).contains(&at) && at != *best {
                let p = power(at);
                if p > best_power {
                    best_power = p;
                    *best = at;
                }
            }
        };
        let step = (hop / 2).max(1) as u64;
        // Coarse grid through `trigger`, earliest first (ties keep the earlier).
        let mut at = trigger - (trigger - lo) / step * step;
        while at <= hi {
            consider(at, &mut best);
            at += step;
        }
        let mut step = step / 2;
        while step >= 1 {
            let center = best;
            if let Some(before) = center.checked_sub(step) {
                consider(before, &mut best);
            }
            consider(center + step, &mut best);
            step /= 2;
        }
        best
    }
}
