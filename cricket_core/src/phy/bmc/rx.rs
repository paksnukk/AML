use super::{BmcParams, Modulation};
use crate::phy::{DemodResult, LinkState, PhyConfig, RxEvent};
use alloc::vec::Vec;
use core::f32::consts::PI;

/// Absolute stream position: sample index + fraction in `[0, 1)`.
#[derive(Debug, Clone, Copy)]
struct Pos {
    index: u64,
    frac: f32,
}

impl Pos {
    /// `self - earlier` in samples.
    fn since(self, earlier: Pos) -> f32 {
        (self.index - earlier.index) as f32 + (self.frac - earlier.frac)
    }
}

/// Direct-form-I biquad.
#[derive(Debug, Clone)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// RBJ-cookbook Butterworth (Q = 1/sqrt 2) low-pass.
    fn lowpass(cutoff_hz: f32, sample_rate_hz: u32) -> Self {
        let fc = cutoff_hz.min(0.45 * sample_rate_hz as f32);
        let w0 = 2.0 * PI * fc / sample_rate_hz as f32;
        let alpha = libm::sinf(w0) / (2.0 * core::f32::consts::FRAC_1_SQRT_2);
        let cw = libm::cosf(w0);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cw) / 2.0 / a0,
            b1: (1.0 - cw) / a0,
            b2: (1.0 - cw) / 2.0 / a0,
            a1: -2.0 * cw / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Counting preamble half-cell intervals; `half` is their running mean.
    Search { run: usize, half: f32 },
    /// First sync violation seen; the next interval must be the second.
    Sync,
    /// Reading body bits; `pending_half` = first half of a `1` seen.
    Body { pending_half: bool },
}

/// Interval class in half cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interval {
    Half,
    Full,
    Violation,
    Invalid,
}

fn classify(units: f32) -> Interval {
    if units < 0.5 {
        Interval::Invalid
    } else if units < 1.5 {
        Interval::Half
    } else if units < 2.5 {
        Interval::Full
    } else if units < 3.5 {
        Interval::Violation
    } else {
        Interval::Invalid
    }
}

/// Streaming biphase-mark receiver emitting the same [`RxEvent`]s as the
/// MFSK [`ModemRx`](crate::phy::ModemRx) (one `BodyNibble` per 4 bits).
pub struct BmcRx {
    config: PhyConfig,
    params: BmcParams,
    half_nominal: f32,
    // Front end
    hp_r: f32,
    hp_x1: f32,
    hp_y1: f32,
    lp: Biquad,
    env: f32,
    env_decay: f32,
    floor: f32,
    prev_y: f32,
    /// Schmitt state: `Some(true)` high, `Some(false)` low, `None` gated.
    high: Option<bool>,
    last_zc_up: Option<Pos>,
    last_zc_down: Option<Pos>,
    last_edge: Option<Pos>,
    samples_pushed: u64,
    // Decoder
    phase: Phase,
    half_est: f32,
    link_state: LinkState,
    expected_body_nibbles: usize,
    acc: u8,
    acc_bits: u8,
    nibbles: usize,
    dev_sum: f32,
    dev_n: u32,
    last_confidence_db: f32,
}

impl BmcRx {
    /// Panics if `config` does not select [`Modulation::Bmc`].
    pub fn new(config: PhyConfig) -> Self {
        let params = match &config.modulation {
            Modulation::Bmc(p) => p.clone(),
            Modulation::Mfsk => panic!("BmcRx needs a BMC PhyConfig"),
        };
        let sr = config.sample_rate_hz;
        let bit_rate = params.bit_rate as f32;
        let half_nominal = params.half_cell_samples(sr);
        let hp_r = libm::expf(-2.0 * PI * params.rx_highpass_ratio * bit_rate / sr as f32);
        let lp = Biquad::lowpass(params.rx_lowpass_ratio * bit_rate, sr);
        let release_samples = (params.envelope_release_bits * 2.0 * half_nominal).max(1.0);
        let env_decay = libm::expf(-1.0 / release_samples);
        // Silence floor: min_rms_dbfs is relative to a full-scale sine
        // (-3 dBFS RMS = peak 1.0); the envelope is a peak.
        let floor = libm::powf(10.0, (config.min_rms_dbfs + 3.0) / 20.0);
        Self {
            config,
            params,
            half_nominal,
            hp_r,
            hp_x1: 0.0,
            hp_y1: 0.0,
            lp,
            env: 0.0,
            env_decay,
            floor,
            prev_y: 0.0,
            high: None,
            last_zc_up: None,
            last_zc_down: None,
            last_edge: None,
            samples_pushed: 0,
            phase: Phase::Search { run: 0, half: 0.0 },
            half_est: half_nominal,
            link_state: LinkState::Search,
            expected_body_nibbles: 16,
            acc: 0,
            acc_bits: 0,
            nibbles: 0,
            dev_sum: 0.0,
            dev_n: 0,
            last_confidence_db: 0.0,
        }
    }

    pub fn config(&self) -> &PhyConfig {
        &self.config
    }

    pub fn link_state(&self) -> LinkState {
        self.link_state
    }

    /// Timing quality of the last packet: `-10 log10(mean squared interval
    /// error in half cells)`, capped at 40 dB (an ideal line is ~30-40 dB;
    /// the decision boundary is at 6 dB).
    pub fn last_confidence_db(&self) -> f32 {
        self.last_confidence_db
    }

    pub fn set_expected_body_nibbles(&mut self, n: usize) {
        self.expected_body_nibbles = n;
    }

    pub fn samples_pushed(&self) -> u64 {
        self.samples_pushed
    }

    pub fn reset(&mut self) {
        self.phase = Phase::Search { run: 0, half: 0.0 };
        self.link_state = LinkState::Search;
        self.high = None;
        self.last_edge = None;
    }

    pub fn push_samples(&mut self, samples: &[f32]) -> Vec<RxEvent> {
        let mut events = Vec::new();
        for &x in samples {
            self.push_one(x, &mut events);
        }
        events.push(RxEvent::Metrics {
            state: self.link_state,
            confidence_db: self.last_confidence_db,
        });
        events
    }

    fn push_one(&mut self, x: f32, events: &mut Vec<RxEvent>) {
        let index = self.samples_pushed;
        self.samples_pushed += 1;

        // DC blocker, then low-pass.
        let hp = x - self.hp_x1 + self.hp_r * self.hp_y1;
        self.hp_x1 = x;
        self.hp_y1 = hp;
        let y = self.lp.process(hp);

        // Interpolated zero crossings between samples index-1 and index.
        let prev = self.prev_y;
        if prev < 0.0 && y >= 0.0 {
            self.last_zc_up = Some(Pos {
                index: index - 1,
                frac: prev / (prev - y),
            });
        } else if prev >= 0.0 && y < 0.0 && index > 0 {
            self.last_zc_down = Some(Pos {
                index: index - 1,
                frac: prev / (prev - y),
            });
        }
        self.prev_y = y;

        let a = libm::fabsf(y);
        self.env = if a > self.env {
            a
        } else {
            self.env * self.env_decay
        };
        if self.env < self.floor {
            if self.high.is_some() {
                self.high = None;
                self.last_edge = None;
                self.abort(events);
            }
            return;
        }

        let th = self.params.hysteresis * self.env;
        let flipped_to = if y > th && self.high != Some(true) {
            Some(true)
        } else if y < -th && self.high != Some(false) {
            Some(false)
        } else {
            None
        };
        let Some(level) = flipped_to else { return };
        let was = self.high.replace(level);
        if was.is_none() {
            // Leaving the gate: no transition to time this from.
            self.last_edge = None;
            return;
        }
        let zc = if level {
            self.last_zc_up
        } else {
            self.last_zc_down
        };
        let Some(edge) = zc else { return };
        if let Some(prev_edge) = self.last_edge.replace(edge) {
            let interval = edge.since(prev_edge);
            self.on_interval(interval, events);
        }
    }

    /// A body in progress is abandoned (SyncLost); search restarts.
    fn abort(&mut self, events: &mut Vec<RxEvent>) {
        if matches!(self.phase, Phase::Body { .. }) {
            events.push(RxEvent::SyncLost);
        }
        if !matches!(self.phase, Phase::Search { .. }) {
            self.link_state = LinkState::Search;
        }
        self.phase = Phase::Search { run: 0, half: 0.0 };
    }

    /// Search-mode handling of `interval`: extend or restart the preamble run.
    fn search(&mut self, interval: f32, run: usize, half: f32) {
        let tol = 0.3;
        let reference = if run == 0 { self.half_nominal } else { half };
        if libm::fabsf(interval / reference - 1.0) < tol {
            let n = (run + 1).min(8) as f32;
            let half = if run == 0 {
                interval
            } else {
                half + (interval - half) / n
            };
            self.phase = Phase::Search { run: run + 1, half };
            return;
        }
        let rate_ok = libm::fabsf(half / self.half_nominal - 1.0) <= self.params.max_rate_error;
        if run >= self.params.min_train_intervals
            && rate_ok
            && classify(interval / half) == Interval::Violation
        {
            self.half_est = half;
            self.phase = Phase::Sync;
            return;
        }
        // Restart; this interval may itself start a new run.
        self.phase = Phase::Search { run: 0, half: 0.0 };
        if libm::fabsf(interval / self.half_nominal - 1.0) < tol {
            self.phase = Phase::Search {
                run: 1,
                half: interval,
            };
        }
    }

    fn on_interval(&mut self, interval: f32, events: &mut Vec<RxEvent>) {
        match self.phase {
            Phase::Search { run, half } => self.search(interval, run, half),
            Phase::Sync => {
                if classify(interval / self.half_est) == Interval::Violation {
                    self.phase = Phase::Body {
                        pending_half: false,
                    };
                    self.link_state = LinkState::Locked;
                    self.acc = 0;
                    self.acc_bits = 0;
                    self.nibbles = 0;
                    self.dev_sum = 0.0;
                    self.dev_n = 0;
                    events.push(RxEvent::PreambleLocked {
                        confidence_db: self.last_confidence_db,
                    });
                } else {
                    self.phase = Phase::Search { run: 0, half: 0.0 };
                    self.search(interval, 0, 0.0);
                }
            }
            Phase::Body { pending_half } => {
                let units = interval / self.half_est;
                let class = classify(units);
                let (bit, measured_half) = match (class, pending_half) {
                    (Interval::Half, false) => {
                        self.phase = Phase::Body { pending_half: true };
                        (None, interval)
                    }
                    (Interval::Half, true) => (Some(1u8), interval),
                    (Interval::Full, false) => (Some(0u8), interval / 2.0),
                    _ => {
                        self.abort(events);
                        // The offending interval may begin a new preamble.
                        self.search(interval, 0, 0.0);
                        return;
                    }
                };
                // Track the clock (rate drift / SRC) and the timing quality.
                self.half_est += (measured_half - self.half_est) / 16.0;
                let err = units - libm::roundf(units);
                self.dev_sum += err * err;
                self.dev_n += 1;
                if let Some(b) = bit {
                    self.phase = Phase::Body {
                        pending_half: false,
                    };
                    self.push_bit(b, events);
                }
            }
        }
    }

    fn push_bit(&mut self, bit: u8, events: &mut Vec<RxEvent>) {
        self.acc = (self.acc << 1) | bit;
        self.acc_bits += 1;
        if self.acc_bits < 4 {
            return;
        }
        let nibble = self.acc & 0x0F;
        self.acc = 0;
        self.acc_bits = 0;
        let mean = self.dev_sum / self.dev_n.max(1) as f32;
        let confidence_db = (-10.0 * libm::log10f(mean.max(1e-4))).min(40.0);
        self.last_confidence_db = confidence_db;
        events.push(RxEvent::BodyNibble {
            result: DemodResult {
                nibble,
                power: self.env * self.env,
                confidence_db,
                // Fraction of the decision margin (0.5 half cell) left unused.
                purity: (1.0 - 2.0 * libm::sqrtf(mean)).max(0.0),
            },
            end_sample: self.samples_pushed,
        });
        self.nibbles += 1;
        if self.nibbles >= self.expected_body_nibbles {
            // Packet complete; stay Locked (as the MFSK receiver does).
            self.phase = Phase::Search { run: 0, half: 0.0 };
        }
    }
}
