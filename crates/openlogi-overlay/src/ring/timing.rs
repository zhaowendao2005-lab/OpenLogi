//! Opt-in frame cadence diagnostics: no timers and no per-frame logging.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct FrameTiming {
    phase: Option<&'static str>,
    last: Option<Instant>,
    started: Option<Instant>,
    opening_paint: Option<Instant>,
    intervals: Vec<Duration>,
}
impl FrameTiming {
    pub fn begin(&mut self, phase: &'static str) {
        self.report();
        if tracing::enabled!(tracing::Level::DEBUG) {
            self.phase = Some(phase);
            let now = Instant::now();
            self.started = Some(now);
            self.opening_paint = (phase == "opening").then_some(now);
        }
    }
    pub fn frame(&mut self, active: bool, outer_visible: bool) -> Option<Instant> {
        self.phase?;
        let now = Instant::now();
        if let Some(started) = self.started.take() {
            tracing::debug!(
                phase = self.phase,
                elapsed_us = now.saturating_duration_since(started).as_micros(),
                "Actions Ring first CPU render after phase start"
            );
        }
        let opening_paint = if outer_visible {
            self.opening_paint.take()
        } else {
            None
        };
        if let Some(last) = self.last.replace(now) {
            self.intervals.push(now.saturating_duration_since(last));
        }
        if !active {
            self.report();
        }
        opening_paint
    }
    fn report(&mut self) {
        if let Some(phase) = self.phase.take() {
            self.intervals.sort_unstable();
            let median = self
                .intervals
                .get(self.intervals.len() / 2)
                .copied()
                .unwrap_or_default();
            let maximum = self.intervals.last().copied().unwrap_or_default();
            tracing::debug!(
                phase,
                frames = self.intervals.len() + usize::from(self.last.is_some()),
                median_us = median.as_micros(),
                maximum_us = maximum.as_micros(),
                "Actions Ring frame cadence (CPU render samples, not GPU presentation)"
            );
        }
        self.last = None;
        self.started = None;
        self.opening_paint = None;
        self.intervals.clear();
    }
}
impl Drop for FrameTiming {
    fn drop(&mut self) {
        self.report();
    }
}
