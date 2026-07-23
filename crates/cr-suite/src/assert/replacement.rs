//! Replacement / prevention assertions (CR 614 / CR 615) — documentation stubs.
//!
//! CR 614 (replacement) and CR 615 (prevention) effects act as "shields" that
//! watch for an event and modify or prevent it before it happens. The engine
//! resolves these inside the event pipeline, so the observable post-conditions
//! are downstream state (e.g. a creature entered tapped, damage was reduced to
//! 0, a shield counter was consumed). Those are already reachable with existing
//! zone / creature-damage / life assertions; a dedicated "a replacement effect
//! applied N times" ledger assertion is deferred until a fixture needs it.

/// CR 614/615 predicate vocabulary for future assertions.
pub const REPLACEMENT_NOTES: &[&str] = &[
    "damage_prevented: assert damage that would be dealt was prevented (CR 615).",
    "damage_reduced_to: assert a replacement lowered a damage event (CR 614.1).",
    "entered_modified: assert an as-enters replacement fired (tapped, with \
     counters, as a copy) (CR 614.1c).",
    "shield_consumed: assert a one-shot replacement (shield counter / \
     regeneration shield) was consumed (CR 614.13).",
];
