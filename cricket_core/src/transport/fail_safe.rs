//! Link-dead detection (docs/aml-v1-spec.md §5.2).
//!
//! `FailSafeMonitor` is the single authority for link-dead. It works on
//! absolute stream positions (sample indices since the receiver started), so
//! the moment it fires does not depend on how the caller chunks its input.

/// Tracks the time since the last CRC-valid packet, in samples.
///
/// The monitor is armed by the first valid packet. Once armed, link-dead fires
/// at stream sample `last_valid_end + limit_samples()`, where `last_valid_end`
/// is the position one past the last sample of the latest valid packet. After
/// firing it disarms until the next valid packet, so each link-dead event
/// fires exactly once.
#[derive(Debug, Clone)]
pub struct FailSafeMonitor {
    limit_samples: u64,
    last_valid_end: u64,
    armed: bool,
}

impl FailSafeMonitor {
    pub fn new(link_dead_ms: u32, sample_rate: u32) -> Self {
        Self {
            limit_samples: (link_dead_ms as u64) * (sample_rate as u64) / 1000,
            last_valid_end: 0,
            armed: false,
        }
    }

    /// Link-dead timeout in samples.
    pub fn limit_samples(&self) -> u64 {
        self.limit_samples
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// A CRC-valid packet ended at stream position `end` (exclusive): arm and
    /// restart the timeout.
    pub fn on_valid_packet(&mut self, end: u64) {
        self.last_valid_end = self.last_valid_end.max(end);
        self.armed = true;
    }

    /// Disarm without firing (link-dead was declared by someone else, e.g. a
    /// wall-clock watchdog). Returns true if the monitor was armed.
    pub fn disarm(&mut self) -> bool {
        core::mem::replace(&mut self.armed, false)
    }

    /// Stream sample at which link-dead fires if no valid packet arrives first.
    pub fn deadline(&self) -> Option<u64> {
        self.armed
            .then(|| self.last_valid_end.saturating_add(self.limit_samples))
    }

    /// Check whether link-dead fires before stream position `now` (exclusive),
    /// i.e. whether the deadline sample has already been received. Returns the
    /// deadline sample and disarms when it fires.
    pub fn poll(&mut self, now: u64) -> Option<u64> {
        let deadline = self.deadline()?;
        if deadline < now {
            self.armed = false;
            Some(deadline)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_at_deadline() {
        let mut m = FailSafeMonitor::new(2000, 1000);
        assert_eq!(m.poll(10_000), None, "never armed");
        m.on_valid_packet(100);
        assert_eq!(m.deadline(), Some(2100));
        assert_eq!(m.poll(2100), None);
        assert_eq!(m.poll(2101), Some(2100));
        assert_eq!(m.poll(50_000), None, "fires once");
        m.on_valid_packet(60_000);
        assert_eq!(m.poll(62_001), Some(62_000));
    }

    #[test]
    fn disarm_suppresses_pending_deadline() {
        let mut m = FailSafeMonitor::new(2000, 1000);
        assert!(!m.disarm(), "never armed");
        m.on_valid_packet(100);
        assert!(m.disarm());
        assert!(!m.is_armed());
        assert_eq!(m.poll(10_000), None);
        m.on_valid_packet(20_000);
        assert_eq!(m.poll(22_001), Some(22_000), "re-armed by next packet");
    }
}
