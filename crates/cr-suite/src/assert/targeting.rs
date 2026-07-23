//! Targeting assertions (CR 115) — documentation stubs.
//!
//! CR 115 governs target legality: how many targets a spell/ability has, which
//! objects/players are legal, and re-checking legality on resolution
//! (CR 608.2b/2c). The cr-suite runner already exercises target *selection*
//! through `GameAction::SelectTargets` in the Lightning Bolt step; explicit
//! assertions about the *set of chosen targets* are not yet wired because no
//! fixture needs them beyond the implicit "the spell resolved and dealt damage"
//! check.
//!
//! These notes enumerate the predicates a future `targeting` assertion family
//! would expose so fixtures can be authored against a stable vocabulary.

/// CR 115 predicate vocabulary agents can request when this family is wired.
pub const TARGETING_NOTES: &[&str] = &[
    "target_count: number of targets a stack object has chosen (CR 115.1a).",
    "target_is_object: a chosen target resolves to a specific object handle (CR 115.1).",
    "target_is_player: a chosen target resolves to a specific player (CR 115.1).",
    "target_became_illegal: a target was legal on announcement but illegal on \
     resolution, so the object doesn't affect it (CR 608.2b).",
];
