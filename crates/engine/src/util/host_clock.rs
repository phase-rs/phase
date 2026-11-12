//! The host's wall clock, in the epoch-millisecond domain of host-stamped
//! observations (`GameAction::SayPhrase::at_ms`).
//!
//! The deterministic reducer never reads a clock: it judges the instants hosts
//! stamp onto actions. This helper is for the hosts themselves — a submission
//! boundary that stamps an action it is about to apply (an in-process AI
//! driver, the WASM bridge). It is never called while projecting a decision,
//! so a projected action payload stays identical across reads.
//!
//! `web_time::SystemTime` is `std::time::SystemTime` natively and reads
//! `Date.now()` on `wasm32-unknown-unknown`, where `std`'s clock panics.

use web_time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, as observed by this host now.
pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
