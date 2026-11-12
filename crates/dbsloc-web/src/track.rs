// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Pure run-tracking state: status polling cadence, SSE de-duplication and reconnect rules.
//!
//! Times are milliseconds from any monotonic-enough clock (`Date.now()` in the browser), passed
//! in so the logic is unit-tested natively.

use dbsloc_api::RunPhase;

/// How often a non-terminal run's status is polled.
pub(crate) const POLL_INTERVAL_MS: f64 = 1500.0;
/// A poll with no answer after this long no longer blocks the next one.
const POLL_TIMEOUT_MS: f64 = 10_000.0;
/// Delay before a closed event stream is reopened.
pub(crate) const SSE_RETRY_MS: f64 = 2000.0;

/// `true` for phases the run can leave on its own (`queued`, `running`, `awaiting_qc`).
pub(crate) fn is_active(phase: RunPhase) -> bool {
    matches!(
        phase,
        RunPhase::Queued | RunPhase::Running | RunPhase::AwaitingQc
    )
}

/// Polling, event de-duplication and stream reconnect state of the run being watched.
#[derive(Debug, Default)]
pub(crate) struct RunTracker {
    last_event_id: Option<u64>,
    poll_sent_at: Option<f64>,
    next_poll_at: f64,
    sse_ended: bool,
    sse_retry_at: Option<f64>,
}

impl RunTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The id of the newest event accepted, to resume the stream after it.
    pub(crate) fn last_event_id(&self) -> Option<u64> {
        self.last_event_id
    }

    /// `true` when an event with this SSE id is new; ids that are missing cannot be de-duplicated
    /// and are accepted.
    pub(crate) fn accept_event(&mut self, id: Option<u64>) -> bool {
        match id {
            Some(id) if self.last_event_id.is_some_and(|last| id <= last) => false,
            Some(id) => {
                self.last_event_id = Some(id);
                true
            }
            None => true,
        }
    }

    /// `true` when a status poll should go out now (none in flight, the interval has passed).
    pub(crate) fn poll_due(&self, now: f64) -> bool {
        let in_flight = self
            .poll_sent_at
            .is_some_and(|sent| now - sent < POLL_TIMEOUT_MS);
        !in_flight && now >= self.next_poll_at
    }

    pub(crate) fn poll_sent(&mut self, now: f64) {
        self.poll_sent_at = Some(now);
        self.next_poll_at = now + POLL_INTERVAL_MS;
    }

    pub(crate) fn poll_answered(&mut self) {
        self.poll_sent_at = None;
    }

    /// Make the next poll due immediately (after an event that changes the status).
    pub(crate) fn poll_soon(&mut self) {
        self.next_poll_at = 0.0;
    }

    /// The stream sent its `end` event; it must not be reopened.
    pub(crate) fn stream_ended(&mut self) {
        self.sse_ended = true;
        self.sse_retry_at = None;
    }

    /// The stream closed without an `end` event: schedule a reopen.
    pub(crate) fn stream_lost(&mut self, now: f64) {
        if !self.sse_ended {
            self.sse_retry_at = Some(now + SSE_RETRY_MS);
        }
    }

    /// `true` when the stream should be reopened now.
    pub(crate) fn reconnect_due(&self, now: f64) -> bool {
        !self.sse_ended && self.sse_retry_at.is_some_and(|at| now >= at)
    }

    pub(crate) fn reconnect_started(&mut self) {
        self.sse_retry_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_phases_are_the_ones_a_run_leaves_on_its_own() {
        for phase in [RunPhase::Queued, RunPhase::Running, RunPhase::AwaitingQc] {
            assert!(is_active(phase));
        }
        for phase in [
            RunPhase::Done,
            RunPhase::Failed,
            RunPhase::Cancelled,
            RunPhase::Resumed,
            RunPhase::Rejected,
        ] {
            assert!(!is_active(phase));
        }
    }

    #[test]
    fn events_are_de_duplicated_by_sse_id() {
        let mut t = RunTracker::new();
        assert!(t.accept_event(Some(1)));
        assert!(t.accept_event(Some(2)));
        assert!(!t.accept_event(Some(2)));
        assert!(!t.accept_event(Some(1)));
        assert!(t.accept_event(Some(5)));
        assert_eq!(t.last_event_id(), Some(5));
        assert!(t.accept_event(None));
        assert_eq!(t.last_event_id(), Some(5));
    }

    #[test]
    fn polling_is_throttled_and_one_in_flight() {
        let mut t = RunTracker::new();
        assert!(t.poll_due(0.0));
        t.poll_sent(1000.0);
        assert!(!t.poll_due(1200.0), "one in flight");
        t.poll_answered();
        assert!(!t.poll_due(1200.0), "interval not over");
        assert!(t.poll_due(2500.0));
        t.poll_sent(2500.0);
        assert!(!t.poll_due(5000.0), "still in flight");
        assert!(t.poll_due(13_000.0), "a lost answer times out");
    }

    #[test]
    fn an_event_can_make_the_next_poll_due() {
        let mut t = RunTracker::new();
        t.poll_sent(1000.0);
        t.poll_answered();
        assert!(!t.poll_due(1100.0));
        t.poll_soon();
        assert!(t.poll_due(1100.0));
    }

    #[test]
    fn a_lost_stream_reconnects_unless_it_ended() {
        let mut t = RunTracker::new();
        t.stream_lost(1000.0);
        assert!(!t.reconnect_due(2000.0));
        assert!(t.reconnect_due(3000.0));
        t.reconnect_started();
        assert!(!t.reconnect_due(9000.0));
        t.stream_lost(9000.0);
        t.stream_ended();
        assert!(!t.reconnect_due(20_000.0));
        t.stream_lost(20_000.0);
        assert!(!t.reconnect_due(30_000.0));
    }
}
