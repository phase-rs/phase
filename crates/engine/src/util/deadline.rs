//! Cooperative time budgets for search and simulation.
//!
//! Uses `web_time::Instant` so the same primitive works on native and WASM.
//! `std::time::Instant` panics in the browser and must not be used on any
//! code path that compiles to WASM.

use std::time::Duration;
use web_time::Instant;

/// A shared deadline checked by long-running operations (search, rollout,
/// candidate validation) so they can return best-so-far results when the
/// wall-clock budget expires.
///
/// A [`Deadline::none`] deadline never expires and short-circuits all checks,
/// which is the correct behavior for deterministic tests bounded by
/// `max_nodes` / `max_depth` rather than wall time.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    limit: Option<Instant>,
}

impl Deadline {
    pub fn after(budget_ms: u32) -> Self {
        Self {
            limit: Some(Instant::now() + Duration::from_millis(budget_ms as u64)),
        }
    }

    pub const fn none() -> Self {
        Self { limit: None }
    }

    pub fn expired(&self) -> bool {
        match self.limit {
            Some(limit) => Instant::now() >= limit,
            None => false,
        }
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.limit
            .map(|limit| limit.saturating_duration_since(Instant::now()))
    }
}

impl Default for Deadline {
    fn default() -> Self {
        Self::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn none_never_expires() {
        let d = Deadline::none();
        assert!(!d.expired());
        assert!(d.remaining().is_none());
    }

    #[test]
    fn after_expires_when_budget_exhausted() {
        let d = Deadline::after(10);
        thread::sleep(Duration::from_millis(25));
        assert!(d.expired());
    }

    #[test]
    fn after_reports_remaining() {
        let d = Deadline::after(500);
        let remaining = d.remaining().unwrap();
        assert!(remaining > Duration::from_millis(100));
    }
}
