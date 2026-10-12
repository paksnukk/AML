//! Pure TX message scheduler: priority queue, idle keepalive / NoteOff refresh,
//! and bounded NoteOn dropping. Time is a sample counter supplied by the caller.
//!
//! The caller encodes only what [`TxScheduler::poll`] returns and assumes each
//! returned message is transmitted immediately. Do not call `poll` again until
//! that transmission has *started*; pass `now_samples` as the stream sample index
//! where that transmission begins.

use super::TransportMessage;
use std::collections::{HashSet, VecDeque};

const REFRESH_CAPACITY: usize = 32;
const REFRESH_LIFETIME_MS: u64 = 10_000;
/// Repeats per released note. Enough that no note stayed stuck past
/// link-dead time in `tests/stuck_notes.rs` (up to 2 % symbol errors), while
/// each release costs a bounded burst of airtime instead of every idle slot.
const REFRESH_MAX_REPEATS: u32 = 6;
const NOTEON_BUDGET_MS: u64 = 250;
/// Queued NoteOns that are never dropped for the airtime budget, so a chord
/// survives even when one message is longer than the budget (Robust).
const MIN_KEPT_NOTE_ONS: usize = 4;

#[derive(Clone, Copy)]
struct RefreshEntry {
    note: u8,
    /// Stream sample when the original user NoteOff was emitted.
    emitted_at: u64,
    sent: u32,
}

struct Queued {
    msg: TransportMessage,
    push_sample: u64,
    /// NoteOn only: a NoteOff for this note arrived while this NoteOn was
    /// still queued (push sample of that NoteOff). It is sent right after
    /// this NoteOn, so a short note is not released before it is struck.
    release_after: Option<u64>,
}

impl Queued {
    fn new(msg: TransportMessage, push_sample: u64) -> Self {
        Self {
            msg,
            push_sample,
            release_after: None,
        }
    }

    fn is_note_on(&self, note: u8) -> bool {
        matches!(self.msg, TransportMessage::NoteOn { note: n, .. } if n == note)
    }
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
    refresh: VecDeque<RefreshEntry>,

    last_emit_sample: Option<u64>,
    /// Set when a queued NoteOn was dropped because the queue exceeded its
    /// airtime budget. The caller reports and clears it.
    pub note_on_dropped: bool,
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
            refresh: VecDeque::new(),
            last_emit_sample: None,
            note_on_dropped: false,
        }
    }

    /// Messages waiting to be sent (a NoteOff held behind its NoteOn counts).
    pub fn queued(&self) -> usize {
        let held_offs = self
            .normal_q
            .iter()
            .filter(|q| q.release_after.is_some())
            .count();
        self.panic_q.len() + self.note_off_q.len() + self.normal_q.len() + held_offs
    }

    /// How long the oldest queued message has waited at `now_samples`.
    pub fn oldest_wait_samples(&self, now_samples: u64) -> u64 {
        [&self.panic_q, &self.note_off_q, &self.normal_q]
            .iter()
            .filter_map(|q| q.iter().map(|e| e.push_sample).min())
            .min()
            .map_or(0, |oldest| now_samples.saturating_sub(oldest))
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
                // A NoteOn queued before the Panic would sound after it.
                self.normal_q
                    .retain(|q| !matches!(q.msg, TransportMessage::NoteOn { .. }));
                self.panic_q.push_back(Queued::new(msg, push_sample));
            }
            TransportMessage::NoteOff { note } => {
                // Release after the latest queued strike of this note, if any;
                // otherwise the note is (or will be) sounding from an earlier,
                // already sent NoteOn and the release goes out with priority.
                match self.normal_q.iter_mut().rev().find(|q| q.is_note_on(note)) {
                    Some(on) => {
                        on.release_after.get_or_insert(push_sample);
                    }
                    None => self.note_off_q.push_back(Queued::new(msg, push_sample)),
                }
            }
            TransportMessage::NoteOn { .. } => {
                while self
                    .droppable_airtime_samples()
                    .saturating_add(self.message_samples)
                    > self.noteon_budget_samples
                    && self.queued_note_ons() >= MIN_KEPT_NOTE_ONS
                {
                    if !self.drop_oldest_note_on() {
                        break;
                    }
                }
                self.normal_q.push_back(Queued::new(msg, push_sample));
            }
            other => self.normal_q.push_back(Queued::new(other, push_sample)),
        }
    }

    pub fn poll(&mut self, now_samples: u64) -> Option<TransportMessage> {
        self.poll_detailed(now_samples).map(|r| r.msg)
    }

    pub fn poll_detailed(&mut self, now_samples: u64) -> Option<PollResult> {
        if let Some(q) = self.panic_q.pop_front() {
            self.sounding.clear();
            return Some(self.emit(q.msg, now_samples, true));
        }
        if let Some(q) = self.note_off_q.pop_front() {
            if let TransportMessage::NoteOff { note } = q.msg {
                self.sounding.remove(&note);
                self.push_refresh(note, now_samples);
            }
            return Some(self.emit(q.msg, now_samples, true));
        }
        if let Some(q) = self.normal_q.pop_front() {
            if let TransportMessage::NoteOn { note, .. } = q.msg {
                self.cancel_refresh(note);
                self.sounding.insert(note);
                if let Some(push_sample) = q.release_after {
                    self.note_off_q
                        .push_back(Queued::new(TransportMessage::NoteOff { note }, push_sample));
                }
            }
            return Some(self.emit(q.msg, now_samples, true));
        }

        self.prune_refresh(now_samples);
        if self.last_emit_sample == Some(now_samples) {
            return None;
        }

        if let Some(note) = self.take_refresh() {
            return Some(self.emit(TransportMessage::NoteOff { note }, now_samples, false));
        }

        if now_samples < self.next_keepalive_sample() {
            return None;
        }
        Some(self.emit(TransportMessage::Heartbeat, now_samples, false))
    }

    fn emit(&mut self, msg: TransportMessage, now_samples: u64, user: bool) -> PollResult {
        self.last_emit_sample = Some(now_samples);
        PollResult { msg, user }
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
        });
    }

    /// Next note to repeat. Notes that have been repeated fewer times go first, and among those the oldest
    /// release. A fresh NoteOff is not stuck behind notes that already had
    /// several repeats, and the release that has waited longest is not stuck
    /// behind newer ones.
    fn take_refresh(&mut self) -> Option<u8> {
        let i = (0..self.refresh.len())
            .min_by_key(|&i| (self.refresh[i].sent, self.refresh[i].emitted_at))?;
        let note = self.refresh[i].note;
        self.refresh[i].sent += 1;
        if self.refresh[i].sent >= REFRESH_MAX_REPEATS {
            self.refresh.remove(i);
        }
        Some(note)
    }

    fn cancel_refresh(&mut self, note: u8) {
        self.refresh.retain(|e| e.note != note);
    }

    fn prune_refresh(&mut self, now_samples: u64) {
        self.refresh
            .retain(|e| now_samples.saturating_sub(e.emitted_at) <= self.refresh_lifetime_samples);
    }

    fn queued_note_ons(&self) -> usize {
        self.normal_q
            .iter()
            .filter(|q| matches!(q.msg, TransportMessage::NoteOn { .. }))
            .count()
    }

    /// Airtime of queued NoteOns, plus the NoteOffs held behind them.
    fn droppable_airtime_samples(&self) -> u64 {
        self.normal_q
            .iter()
            .filter(|q| matches!(q.msg, TransportMessage::NoteOn { .. }))
            .map(|q| {
                if q.release_after.is_some() {
                    self.message_samples.saturating_mul(2)
                } else {
                    self.message_samples
                }
            })
            .fold(0u64, u64::saturating_add)
    }

    fn drop_oldest_note_on(&mut self) -> bool {
        let Some(pos) = self
            .normal_q
            .iter()
            .position(|q| matches!(q.msg, TransportMessage::NoteOn { .. }))
        else {
            return false;
        };
        let q = self.normal_q.remove(pos).expect("position is in range");
        // The held NoteOff goes with its NoteOn, unless the note is still
        // sounding from an earlier strike: then the release must still go out.
        if let (TransportMessage::NoteOn { note, .. }, Some(push_sample)) = (q.msg, q.release_after)
        {
            if self.sounding.contains(&note) {
                self.note_off_q
                    .push_back(Queued::new(TransportMessage::NoteOff { note }, push_sample));
            }
        }
        self.note_on_dropped = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modem::repetition_samples;
    use crate::phy::PhyConfig;
    use TransportMessage::{Heartbeat, NoteOff, NoteOn, Panic};

    fn scheduler(config: &PhyConfig) -> TxScheduler {
        let msg_samples = repetition_samples(config) * config.repetition_k.max(1) as u64;
        let hb = config.sample_rate_hz as u64 * config.heartbeat_interval_ms as u64 / 1000;
        TxScheduler::new(config.sample_rate_hz, hb, msg_samples)
    }

    fn fast_scheduler() -> TxScheduler {
        scheduler(&PhyConfig::fast())
    }

    fn on(note: u8) -> TransportMessage {
        NoteOn {
            note,
            velocity: 100,
        }
    }

    /// Plays `note` and releases it; returns the sample after the release.
    fn play_and_release(s: &mut TxScheduler, note: u8, t: u64) -> u64 {
        let msg = s.message_samples;
        s.push(on(note), t);
        assert_eq!(s.poll(t), Some(on(note)));
        s.push(NoteOff { note }, t + msg);
        assert_eq!(s.poll(t + msg), Some(NoteOff { note }));
        t + msg
    }

    fn refresh_notes(s: &TxScheduler) -> Vec<u8> {
        s.refresh.iter().map(|e| e.note).collect()
    }

    #[test]
    fn priority_panic_note_off_note_on() {
        let mut s = fast_scheduler();
        s.push(NoteOff { note: 70 }, 0);
        s.push(Panic, 0);
        s.push(on(60), 0);
        assert_eq!(s.poll(0), Some(Panic));
        assert_eq!(s.poll(0), Some(NoteOff { note: 70 }));
        assert_eq!(s.poll(0), Some(on(60)));
        assert_eq!(s.poll(0), None);
    }

    #[test]
    fn panic_discards_note_ons_queued_before_it() {
        let mut s = fast_scheduler();
        s.push(on(60), 0);
        s.push(on(61), 0);
        s.push(Panic, 0);
        assert_eq!(s.queued(), 1);
        assert_eq!(s.poll(0), Some(Panic));
        assert_eq!(s.poll(0), None);
    }

    #[test]
    fn no_heartbeat_while_queued_then_wait_interval() {
        let mut s = fast_scheduler();
        let hb = s.heartbeat_interval_samples;
        s.push(on(1), 0);
        assert_eq!(s.poll(0), Some(on(1)));
        assert_eq!(s.poll(hb - 1), None);
        assert_eq!(s.poll(hb), Some(Heartbeat));
    }

    #[test]
    fn refresh_right_after_note_off() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        let t = play_and_release(&mut s, 60, 0);
        assert_eq!(s.poll(t), None);
        assert_eq!(s.poll(t + msg), Some(NoteOff { note: 60 }));
        assert_eq!(s.poll(t + msg), None);
        assert_eq!(s.poll(t + 2 * msg), Some(NoteOff { note: 60 }));
    }

    #[test]
    fn two_notes_refresh_round_robin() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        play_and_release(&mut s, 10, 0);
        let t = play_and_release(&mut s, 20, 2 * msg);
        assert_eq!(s.poll(t + msg), Some(NoteOff { note: 10 }));
        assert_eq!(s.poll(t + 2 * msg), Some(NoteOff { note: 20 }));
        assert_eq!(s.poll(t + 3 * msg), Some(NoteOff { note: 10 }));
    }

    #[test]
    fn note_on_push_does_not_cancel_refresh() {
        let mut s = fast_scheduler();
        let t = play_and_release(&mut s, 60, 0);
        s.push(on(70), t + 1);
        assert!(refresh_notes(&s).contains(&60));
        assert_eq!(s.poll(t + s.message_samples), Some(on(70)));
        assert!(refresh_notes(&s).contains(&60));
    }

    #[test]
    fn note_on_emit_cancels_refresh() {
        let mut s = fast_scheduler();
        let hb = s.heartbeat_interval_samples;
        let t = play_and_release(&mut s, 60, 0);
        s.push(on(60), t + 1);
        assert_eq!(s.poll(t + 1), Some(on(60)));
        assert!(refresh_notes(&s).is_empty());
        assert_eq!(s.poll(t + 1 + hb), Some(Heartbeat));
    }

    #[test]
    fn refresh_evicts_oldest_at_capacity() {
        let mut s = fast_scheduler();
        let mut t = 0u64;
        for note in 0..33u8 {
            s.push(on(note), t);
            let _ = s.poll(t);
            s.push(NoteOff { note }, t + 1);
            let _ = s.poll(t + 1);
            t += 2;
        }
        assert!(!refresh_notes(&s).contains(&0));
        assert_eq!(s.poll(t + s.message_samples), Some(NoteOff { note: 1 }));
    }

    #[test]
    fn refresh_expires_after_lifetime() {
        let mut s = fast_scheduler();
        let hb = s.heartbeat_interval_samples;
        let t = play_and_release(&mut s, 60, 0);
        assert_eq!(s.poll(t + s.refresh_lifetime_samples + hb), Some(Heartbeat));
    }

    /// Each release costs a bounded number of repeats, then heartbeats resume.
    #[test]
    fn refresh_stops_after_max_repeats() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        let mut t = play_and_release(&mut s, 60, 0);
        for _ in 0..REFRESH_MAX_REPEATS {
            t += msg;
            assert_eq!(s.poll(t), Some(NoteOff { note: 60 }));
        }
        assert!(refresh_notes(&s).is_empty());
        assert_eq!(s.poll(t + msg), None);
        assert_eq!(s.poll(t + s.heartbeat_interval_samples), Some(Heartbeat));
    }

    #[test]
    fn note_off_while_note_on_queued_keeps_both_in_order() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        s.push(on(60), 0);
        s.push(NoteOff { note: 60 }, 0);
        assert_eq!(s.queued(), 2);
        assert_eq!(s.poll(0), Some(on(60)));
        assert_eq!(s.poll(msg), Some(NoteOff { note: 60 }));
    }

    /// Off, on, off on a note that is already sounding (a trill or a roll)
    /// must end released, not with the NoteOn sent last.
    #[test]
    fn retrigger_while_sounding_keeps_order() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        s.push(on(60), 0);
        assert_eq!(s.poll(0), Some(on(60)));
        s.push(NoteOff { note: 60 }, 10);
        s.push(on(60), 20);
        s.push(NoteOff { note: 60 }, 30);
        s.push(on(60), 40);
        s.push(NoteOff { note: 60 }, 50);
        let sent: Vec<_> = (1..=5).map(|i| s.poll(i * msg).unwrap()).collect();
        assert_eq!(
            sent,
            vec![
                NoteOff { note: 60 },
                on(60),
                NoteOff { note: 60 },
                on(60),
                NoteOff { note: 60 }
            ]
        );
        assert_eq!(s.queued(), 0);
        assert_eq!(refresh_notes(&s), vec![60]);
    }

    #[test]
    fn emitted_note_off_never_dropped() {
        let mut s = fast_scheduler();
        let msg = s.message_samples;
        s.push(on(60), 0);
        let _ = s.poll(0);
        s.push(NoteOff { note: 60 }, msg);
        for i in 0..50u8 {
            s.push(on(70 + i), msg + i as u64);
        }
        assert_eq!(s.poll(msg), Some(NoteOff { note: 60 }));
    }

    #[test]
    fn noteon_budget_drops_oldest_not_panic() {
        let mut s = fast_scheduler();
        let max_noteons = (s.noteon_budget_samples / s.message_samples).max(1) as usize;
        for i in 0..max_noteons + 2 {
            s.push(on(i as u8), 0);
        }
        assert!(s.note_on_dropped);
        s.push(Panic, 0);
        assert_eq!(s.poll(0), Some(Panic));
    }

    /// A Robust message is longer than the airtime budget; a chord pushed at
    /// once must still be sent whole.
    #[test]
    fn robust_chord_is_not_dropped() {
        let mut s = scheduler(&PhyConfig::robust());
        assert!(s.message_samples > s.noteon_budget_samples);
        for n in [60u8, 64, 67, 72] {
            s.push(on(n), 0);
        }
        assert!(!s.note_on_dropped);
        let sent: Vec<_> = (0..4).map(|i| s.poll(i * s.message_samples)).collect();
        assert_eq!(
            sent,
            vec![Some(on(60)), Some(on(64)), Some(on(67)), Some(on(72))]
        );
    }

    /// Dropping a queued re-strike must not lose the release of the note
    /// that is still sounding from its earlier strike.
    #[test]
    fn dropped_restrike_keeps_release_of_sounding_note() {
        let mut s = fast_scheduler();
        s.push(on(60), 0);
        assert_eq!(s.poll(0), Some(on(60)));
        s.push(on(60), 1);
        s.push(NoteOff { note: 60 }, 2);
        for n in 70..80u8 {
            s.push(on(n), 3);
        }
        assert!(s.note_on_dropped);
        assert_eq!(s.poll(4), Some(NoteOff { note: 60 }));
    }

    #[test]
    fn fast_10_per_sec_60s_latency_under_250ms() {
        let mut s = fast_scheduler();
        let hz = PhyConfig::fast().sample_rate_hz as u64;
        let msg = s.message_samples;
        let interval = hz / 10;
        let duration = hz * 60;
        let budget = s.noteon_budget_samples;
        let (mut next_push, mut stream, mut max_wait) = (0u64, 0u64, 0u64);
        let (mut note, mut user_emitted) = (0u8, 0u64);

        while stream < duration || s.queued() > 0 {
            while next_push <= stream && next_push < duration {
                s.push(on(note), next_push);
                note = note.wrapping_add(1);
                next_push += interval;
            }
            let wait = s.oldest_wait_samples(stream);
            if let Some(result) = s.poll_detailed(stream) {
                if result.user {
                    user_emitted += 1;
                    max_wait = max_wait.max(wait);
                }
                stream += msg;
            } else {
                stream += 1;
            }
        }
        assert!(user_emitted >= duration / interval);
        assert!(
            max_wait <= budget,
            "max queue wait {} samples (> {} = 250 ms)",
            max_wait,
            budget
        );
    }
}
