//! Pure TX message scheduler: priority queue, idle keepalive / NoteOff refresh,
//! and bounded NoteOn dropping. Time is a sample counter supplied by the caller.
//!
//! The caller encodes only what [`TxScheduler::poll`] returns and assumes each
//! returned message is transmitted immediately. Do not call `poll` again until
//! that transmission has *started*; pass `now_samples` as the stream sample index
//! where that transmission begins.

use super::TransportMessage;
use std::collections::{HashMap, HashSet, VecDeque};

const REFRESH_CAPACITY: usize = 32;
const REFRESH_LIFETIME_MS: u64 = 10_000;
const NOTEON_BUDGET_MS: u64 = 250;

#[derive(Clone, Copy)]
struct RefreshEntry {
    note: u8,
    /// Stream sample when the original user NoteOff was emitted.
    emitted_at: u64,
    sent: u32,
    /// Stream sample of the last refresh. `0` means none yet.
    last_sent_at: u64,
}

struct Queued {
    msg: TransportMessage,
    #[cfg_attr(not(test), allow(dead_code))]
    push_sample: u64,
}

/// Result of [`TxScheduler::poll_detailed`].
pub struct PollResult {
    pub msg: TransportMessage,
    /// True for Panic, NoteOff, NoteOn, ProgramChange, and MscLite from the user queues.
    pub user: bool,
}

/// TX-side scheduler shared by the engine encoder and offline stream builders.
pub struct TxScheduler {
    heartbeat_interval_samples: u64,
    message_samples: u64,
    refresh_lifetime_samples: u64,
    noteon_budget_samples: u64,

    panic_q: VecDeque<Queued>,
    note_off_q: VecDeque<Queued>,
    normal_q: VecDeque<Queued>,

    /// Notes whose NoteOn has been emitted (sounding on the far side).
    sounding: HashSet<u8>,
    /// NoteOff waiting for a NoteOn that is still queued. Sent immediately
    /// after that NoteOn, so a short note is not cancelled before it is aired
    /// and is not released before it is struck.
    parked_off: HashMap<u8, u64>,
    refresh: VecDeque<RefreshEntry>,

    last_emit_sample: Option<u64>,
    pub stale_note_on_dropped: bool,
}

impl TxScheduler {
    pub fn new(sample_rate_hz: u32, heartbeat_interval_samples: u64, message_samples: u64) -> Self {
        let hz = sample_rate_hz.max(1) as u64;
        Self {
            heartbeat_interval_samples,
            message_samples,
            refresh_lifetime_samples: hz * REFRESH_LIFETIME_MS / 1000,
            noteon_budget_samples: hz * NOTEON_BUDGET_MS / 1000,
            panic_q: VecDeque::new(),
            note_off_q: VecDeque::new(),
            normal_q: VecDeque::new(),
            sounding: HashSet::new(),
            parked_off: HashMap::new(),
            refresh: VecDeque::new(),
            last_emit_sample: None,
            stale_note_on_dropped: false,
        }
    }

    pub fn queued(&self) -> usize {
        self.panic_q.len() + self.note_off_q.len() + self.normal_q.len() + self.parked_off.len()
    }

    /// When the refresh list is empty, earliest sample for a Heartbeat.
    pub fn next_keepalive_sample(&self) -> u64 {
        match self.last_emit_sample {
            Some(last) => last + self.heartbeat_interval_samples,
            None => self.heartbeat_interval_samples,
        }
    }

    pub fn push(&mut self, msg: TransportMessage, push_sample: u64) {
        match msg {
            TransportMessage::Panic => {
                self.panic_q.push_back(Queued { msg, push_sample });
            }
            TransportMessage::NoteOff { note } => {
                if self.sounding.contains(&note) || !self.has_queued_note_on(note) {
                    self.note_off_q.push_back(Queued {
                        msg: TransportMessage::NoteOff { note },
                        push_sample,
                    });
                } else {
                    self.parked_off.insert(note, push_sample);
                }
            }
            TransportMessage::NoteOn { note, velocity } => {
                while self
                    .droppable_airtime_samples()
                    .saturating_add(self.message_samples)
                    > self.noteon_budget_samples
                {
                    if !self.drop_oldest_droppable_note_on() {
                        break;
                    }
                }
                self.normal_q.push_back(Queued {
                    msg: TransportMessage::NoteOn { note, velocity },
                    push_sample,
                });
            }
            other => {
                self.normal_q.push_back(Queued {
                    msg: other,
                    push_sample,
                });
            }
        }
    }

    pub fn poll(&mut self, now_samples: u64) -> Option<TransportMessage> {
        self.poll_detailed(now_samples).map(|r| r.msg)
    }

    pub fn poll_detailed(&mut self, now_samples: u64) -> Option<PollResult> {
        if let Some(q) = self.panic_q.pop_front() {
            return Some(PollResult {
                msg: self.finish_emit(q.msg, now_samples, false),
                user: true,
            });
        }
        if let Some(q) = self.note_off_q.pop_front() {
            return Some(PollResult {
                msg: self.finish_emit(q.msg, now_samples, true),
                user: true,
            });
        }
        if let Some(q) = self.normal_q.pop_front() {
            let msg = q.msg;
            if let TransportMessage::NoteOn { note, .. } = msg {
                self.cancel_refresh(note);
                self.sounding.insert(note);
                if let Some(push_sample) = self.parked_off.remove(&note) {
                    self.note_off_q.push_back(Queued {
                        msg: TransportMessage::NoteOff { note },
                        push_sample,
                    });
                }
            }
            return Some(PollResult {
                msg: self.finish_emit(msg, now_samples, false),
                user: true,
            });
        }

        self.prune_refresh(now_samples);
        if self.keepalive_throttled(now_samples) {
            return None;
        }

        if let Some(note) = self.take_refresh(now_samples) {
            let msg = TransportMessage::NoteOff { note };
            return Some(PollResult {
                msg: self.finish_emit(msg, now_samples, false),
                user: false,
            });
        }

        if now_samples < self.next_keepalive_sample() {
            return None;
        }

        Some(PollResult {
            msg: self.finish_emit(TransportMessage::Heartbeat, now_samples, false),
            user: false,
        })
    }

    fn keepalive_throttled(&self, now_samples: u64) -> bool {
        self.last_emit_sample == Some(now_samples)
    }

    fn finish_emit(
        &mut self,
        msg: TransportMessage,
        now_samples: u64,
        track_refresh: bool,
    ) -> TransportMessage {
        if track_refresh {
            if let TransportMessage::NoteOff { note } = msg {
                self.sounding.remove(&note);
                self.push_refresh(note, now_samples);
            }
        }
        self.last_emit_sample = Some(now_samples);
        msg
    }

    fn push_refresh(&mut self, note: u8, emitted_at: u64) {
        self.cancel_refresh(note);
        if self.refresh.len() >= REFRESH_CAPACITY {
            self.refresh.pop_front();
        }
        self.refresh.push_back(RefreshEntry {
            note,
            emitted_at,
            sent: 0,
            last_sent_at: 0,
        });
    }

    /// Next note to repeat. Notes that have been repeated fewer times go first,
    /// and among those the oldest release. A fresh NoteOff is not stuck behind
    /// notes that already had several repeats, and the release that has waited
    /// longest is not stuck behind newer ones.
    fn take_refresh(&mut self, now_samples: u64) -> Option<u8> {
        let mut best: Option<usize> = None;
        for (i, e) in self.refresh.iter().enumerate() {
            let better = match best {
                None => true,
                Some(b) => {
                    let o = &self.refresh[b];
                    e.sent < o.sent
                        || (e.sent == o.sent && e.emitted_at < o.emitted_at)
                        || (e.sent == o.sent
                            && e.emitted_at == o.emitted_at
                            && e.last_sent_at < o.last_sent_at)
                }
            };
            if better {
                best = Some(i);
            }
        }
        let i = best?;
        let note = self.refresh[i].note;
        self.refresh[i].sent = self.refresh[i].sent.saturating_add(1);
        self.refresh[i].last_sent_at = now_samples;
        Some(note)
    }

    fn cancel_refresh(&mut self, note: u8) {
        self.refresh.retain(|e| e.note != note);
    }

    fn prune_refresh(&mut self, now_samples: u64) {
        self.refresh
            .retain(|e| now_samples.saturating_sub(e.emitted_at) <= self.refresh_lifetime_samples);
    }

    fn has_queued_note_on(&self, note: u8) -> bool {
        self.normal_q
            .iter()
            .any(|q| matches!(q.msg, TransportMessage::NoteOn { note: n, .. } if n == note))
    }

    fn has_queued_note_off(&self, note: u8) -> bool {
        self.parked_off.contains_key(&note)
            || self
                .note_off_q
                .iter()
                .any(|q| matches!(q.msg, TransportMessage::NoteOff { note: n } if n == note))
    }

    fn droppable_airtime_samples(&self) -> u64 {
        let mut total = 0u64;
        for q in &self.normal_q {
            if matches!(q.msg, TransportMessage::NoteOn { .. }) {
                total = total.saturating_add(self.message_samples);
                if let TransportMessage::NoteOn { note, .. } = q.msg {
                    if self.has_queued_note_off(note) && !self.sounding.contains(&note) {
                        total = total.saturating_add(self.message_samples);
                    }
                }
            }
        }
        total
    }

    fn drop_oldest_droppable_note_on(&mut self) -> bool {
        let note = match self.normal_q.iter().find_map(|q| {
            if let TransportMessage::NoteOn { note, .. } = q.msg {
                Some(note)
            } else {
                None
            }
        }) {
            Some(n) => n,
            None => return false,
        };
        let pos = self
            .normal_q
            .iter()
            .position(|q| matches!(q.msg, TransportMessage::NoteOn { note: n, .. } if n == note))
            .unwrap();
        self.normal_q.remove(pos);
        if !self.sounding.contains(&note) {
            self.parked_off.remove(&note);
            self.note_off_q
                .retain(|q| !matches!(q.msg, TransportMessage::NoteOff { note: n } if n == note));
        }
        self.stale_note_on_dropped = true;
        true
    }

    #[cfg(test)]
    fn push_time(&mut self, msg: TransportMessage, push_sample: u64) {
        self.push(msg, push_sample);
    }

    #[cfg(test)]
    fn refresh_notes(&self) -> Vec<u8> {
        self.refresh.iter().map(|e| e.note).collect()
    }

    #[cfg(test)]
    pub(crate) fn peek_next_wait(&self, now_samples: u64) -> Option<u64> {
        if let Some(q) = self.panic_q.front() {
            return Some(now_samples.saturating_sub(q.push_sample));
        }
        if let Some(q) = self.note_off_q.front() {
            return Some(now_samples.saturating_sub(q.push_sample));
        }
        if let Some(q) = self.normal_q.front() {
            return Some(now_samples.saturating_sub(q.push_sample));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modem::repetition_samples;
    use crate::phy::PhyConfig;

    fn fast_scheduler() -> TxScheduler {
        let config = PhyConfig::fast();
        let msg_samples = repetition_samples(&config) * config.repetition_k.max(1) as u64;
        let hb = config.sample_rate_hz as u64 * config.heartbeat_interval_ms as u64 / 1000;
        TxScheduler::new(config.sample_rate_hz, hb, msg_samples)
    }

    fn ms_samples(hz: u32, ms: u64) -> u64 {
        hz as u64 * ms / 1000
    }

    #[test]
    fn priority_panic_note_off_note_on() {
        let mut s = fast_scheduler();
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 100,
            },
            0,
        );
        s.push_time(TransportMessage::NoteOff { note: 70 }, 0);
        s.push_time(TransportMessage::Panic, 0);
        assert_eq!(s.poll(0), Some(TransportMessage::Panic));
        assert_eq!(s.poll(0), Some(TransportMessage::NoteOff { note: 70 }));
        assert_eq!(
            s.poll(0),
            Some(TransportMessage::NoteOn {
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(s.poll(0), None);
    }

    #[test]
    fn no_heartbeat_while_queued_then_wait_interval() {
        let mut s = fast_scheduler();
        let hb = s.heartbeat_interval_samples;
        let msg = s.message_samples;
        s.push_time(
            TransportMessage::NoteOn {
                note: 1,
                velocity: 100,
            },
            0,
        );
        assert_eq!(
            s.poll(0),
            Some(TransportMessage::NoteOn {
                note: 1,
                velocity: 100
            })
        );
        assert_eq!(s.poll(hb - 1), None);
        assert_eq!(s.poll(hb), Some(TransportMessage::Heartbeat));
        let _ = msg;
    }

    #[test]
    fn refresh_immediately_after_note_off() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 100,
            },
            0,
        );
        let _ = s.poll(0);
        s.push_time(TransportMessage::NoteOff { note: 60 }, msg);
        let _ = s.poll(msg);
        assert_eq!(
            s.poll(msg + msg),
            Some(TransportMessage::NoteOff { note: 60 })
        );
        assert_eq!(s.poll(msg + msg), None);
        assert_eq!(
            s.poll(msg + msg + msg),
            Some(TransportMessage::NoteOff { note: 60 })
        );
    }

    #[test]
    fn two_notes_refresh_round_robin() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        for (note, t_on) in [(10u8, 0u64), (20, msg * 2)] {
            s.push_time(
                TransportMessage::NoteOn {
                    note,
                    velocity: 100,
                },
                t_on,
            );
            let _ = s.poll(t_on);
            let t_off = t_on + msg;
            s.push_time(TransportMessage::NoteOff { note }, t_off);
            let _ = s.poll(t_off);
        }
        let base = msg * 4;
        assert_eq!(s.poll(base), Some(TransportMessage::NoteOff { note: 10 }));
        assert_eq!(
            s.poll(base + msg),
            Some(TransportMessage::NoteOff { note: 20 })
        );
        assert_eq!(
            s.poll(base + msg * 2),
            Some(TransportMessage::NoteOff { note: 10 })
        );
    }

    #[test]
    fn note_on_push_does_not_cancel_refresh() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 100,
            },
            0,
        );
        let _ = s.poll(0);
        s.push_time(TransportMessage::NoteOff { note: 60 }, msg);
        let _ = s.poll(msg);
        s.push_time(
            TransportMessage::NoteOn {
                note: 70,
                velocity: 80,
            },
            msg + 1,
        );
        assert!(s.refresh_notes().contains(&60));
        assert_eq!(
            s.poll(msg + msg),
            Some(TransportMessage::NoteOn {
                note: 70,
                velocity: 80
            })
        );
        assert!(s.refresh_notes().contains(&60));
    }

    #[test]
    fn note_on_emit_cancels_refresh() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        let hb = s.heartbeat_interval_samples;
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 100,
            },
            0,
        );
        let _ = s.poll(0);
        s.push_time(TransportMessage::NoteOff { note: 60 }, msg);
        let _ = s.poll(msg);
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 80,
            },
            msg + msg,
        );
        let _ = s.poll(msg + msg);
        let t = msg + msg + msg + hb;
        assert_eq!(s.poll(t), Some(TransportMessage::Heartbeat));
    }

    #[test]
    fn refresh_evicts_oldest_at_capacity() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        let mut t = 0u64;
        for note in 0..33u8 {
            s.push_time(
                TransportMessage::NoteOn {
                    note,
                    velocity: 100,
                },
                t,
            );
            let _ = s.poll(t);
            t += msg;
            s.push_time(TransportMessage::NoteOff { note }, t);
            let _ = s.poll(t);
            t += msg;
        }
        assert!(!s.refresh_notes().contains(&0));
        assert_eq!(s.poll(t), Some(TransportMessage::NoteOff { note: 1 }));
    }

    #[test]
    fn refresh_expires_after_10s() {
        let mut s = fast_scheduler();
        let hb = s.heartbeat_interval_samples;
        let msg = s.message_samples;
        let lifetime = s.refresh_lifetime_samples;
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 100,
            },
            0,
        );
        let _ = s.poll(0);
        s.push_time(TransportMessage::NoteOff { note: 60 }, msg);
        let _ = s.poll(msg);
        let t = msg + lifetime + hb;
        assert_eq!(s.poll(t), Some(TransportMessage::Heartbeat));
    }

    #[test]
    fn note_off_while_note_on_queued_keeps_both_in_order() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 100,
            },
            0,
        );
        s.push_time(TransportMessage::NoteOff { note: 60 }, 0);
        assert_eq!(s.queued(), 2);
        assert_eq!(
            s.poll(0),
            Some(TransportMessage::NoteOn {
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(s.poll(msg), Some(TransportMessage::NoteOff { note: 60 }));
    }

    #[test]
    fn emitted_note_off_never_dropped() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        s.push_time(
            TransportMessage::NoteOn {
                note: 60,
                velocity: 100,
            },
            0,
        );
        let _ = s.poll(0);
        s.push_time(TransportMessage::NoteOff { note: 60 }, msg);
        for i in 0..50u8 {
            s.push_time(
                TransportMessage::NoteOn {
                    note: 70 + i,
                    velocity: 100,
                },
                msg + i as u64,
            );
        }
        assert!(!s.note_off_q.is_empty());
        assert_eq!(s.poll(msg), Some(TransportMessage::NoteOff { note: 60 }));
    }

    #[test]
    fn noteon_budget_drops_oldest_not_panic() {
        let mut s = fast_scheduler();
        let budget = s.noteon_budget_samples;
        let msg = s.message_samples;
        let max_noteons = (budget / msg).max(1) as usize;
        for i in 0..max_noteons + 2 {
            s.push_time(
                TransportMessage::NoteOn {
                    note: i as u8,
                    velocity: 100,
                },
                0,
            );
        }
        assert!(s.stale_note_on_dropped);
        s.push_time(TransportMessage::Panic, 0);
        assert_eq!(s.poll(0), Some(TransportMessage::Panic));
    }

    #[test]
    fn fast_10_per_sec_60s_latency_under_250ms() {
        let config = PhyConfig::fast();
        let hz = config.sample_rate_hz;
        let msg = repetition_samples(&config) * config.repetition_k.max(1) as u64;
        let hb = hz as u64 * config.heartbeat_interval_ms as u64 / 1000;
        let mut s = TxScheduler::new(hz, hb, msg);
        let interval = hz as u64 / 10;
        let duration = hz as u64 * 60;
        let mut next_push = 0u64;
        let mut stream = 0u64;
        let mut max_wait = 0u64;
        let budget = ms_samples(hz, NOTEON_BUDGET_MS);
        let mut note = 0u8;
        let mut user_emitted = 0u64;

        while stream < duration || s.queued() > 0 {
            while next_push <= stream && next_push < duration {
                s.push(
                    TransportMessage::NoteOn {
                        note,
                        velocity: 100,
                    },
                    next_push,
                );
                note = note.wrapping_add(1);
                next_push += interval;
            }
            let wait = s.peek_next_wait(stream);
            if let Some(result) = s.poll_detailed(stream) {
                if result.user {
                    user_emitted += 1;
                    if let Some(w) = wait {
                        max_wait = max_wait.max(w);
                    }
                }
                stream += msg;
            } else if s.queued() == 0 {
                stream = stream.saturating_add(1);
            } else {
                stream += 1;
            }
        }
        assert!(
            user_emitted >= duration / interval,
            "expected ~{} user messages, got {}",
            duration / interval,
            user_emitted
        );
        assert!(
            max_wait <= budget,
            "max queue wait {} samples (> {} = 250 ms)",
            max_wait,
            budget
        );
    }
}
