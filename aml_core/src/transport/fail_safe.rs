use super::midi::all_notes_off;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailSafeAction {
    AllNotesOff,
    ResetDecoder,
}

/// Tracks link health and produces fail-safe actions.
pub struct FailSafeMonitor {
    link_dead_ms: u32,
    sample_rate: u32,
    samples_since_packet: u64,
    was_alive: bool,
}

impl FailSafeMonitor {
    pub fn new(link_dead_ms: u32, sample_rate: u32) -> Self {
        Self {
            link_dead_ms,
            sample_rate,
            samples_since_packet: 0,
            was_alive: false,
        }
    }

    pub fn on_valid_packet(&mut self) {
        self.samples_since_packet = 0;
        self.was_alive = true;
    }

    pub fn on_samples(&mut self, n: usize) -> Vec<FailSafeAction> {
        self.samples_since_packet = self.samples_since_packet.saturating_add(n as u64);
        let limit = (self.link_dead_ms as u64) * (self.sample_rate as u64) / 1000;
        let mut actions = Vec::new();
        if self.was_alive && self.samples_since_packet > limit {
            actions.push(FailSafeAction::AllNotesOff);
            actions.push(FailSafeAction::ResetDecoder);
            self.was_alive = false;
            self.samples_since_packet = 0;
        }
        actions
    }

    pub fn midi_for_action(action: &FailSafeAction) -> Vec<u8> {
        match action {
            FailSafeAction::AllNotesOff => all_notes_off(),
            FailSafeAction::ResetDecoder => Vec::new(),
        }
    }
}
