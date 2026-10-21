use super::*;

impl ActivityRecord {
    pub(super) fn add_phase_time(&self, timings: &mut ActivityTimings, end: Instant) {
        let slot = match self.snapshot.phase.as_str() {
            "admitted" | "preparing" => &mut timings.prepare_ms,
            "routing" | "waiting" => &mut timings.route_wait_ms,
            "attempting" => &mut timings.upstream_headers_ms,
            "headers_received" if self.snapshot.streamed => &mut timings.response_wait_ms,
            "headers_received" => &mut timings.response_read_ms,
            "streaming" => &mut timings.generation_ms,
            _ => return,
        };
        *slot = Some(
            slot.unwrap_or_default()
                .saturating_add(elapsed_ms(self.phase_started, end)),
        );
    }

    pub(super) fn transition(&mut self, phase: &str) {
        let now = Instant::now();
        let mut timings = self.snapshot.timings.clone();
        self.add_phase_time(&mut timings, now);
        self.snapshot.timings = timings;
        self.snapshot.phase = phase.to_string();
        self.phase_started = now;
    }
}
