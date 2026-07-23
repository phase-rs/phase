//! Coverage plan: CR 702 Keyword Abilities — larger evergreen/common set.
//!
//! Every CR number here was grep-verified against `docs/MagicCompRules.txt`.
//! Note: Menace is CR **702.111** (CR 702.110 is Exploit) — the commonly
//! mis-cited "702.110 Menace" is wrong.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "702.2", // Deathtouch
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "creature_in_graveyard"],
        notes: "Deathtouch — any nonzero damage is lethal (CR 702.2b, interacts with 704.5h).",
    },
    CoveragePlanEntry {
        rule: "702.4", // Double Strike
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "creature_damage"],
        notes: "Double strike — deals both first-strike and regular combat damage.",
    },
    CoveragePlanEntry {
        rule: "702.7", // First Strike
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "creature_damage"],
        notes: "First strike — combat damage in the first combat-damage step (CR 510.5).",
    },
    CoveragePlanEntry {
        rule: "702.9", // Flying
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword"],
        notes: "Flying — can only be blocked by flying/reach (CR 509.1b).",
    },
    CoveragePlanEntry {
        rule: "702.10", // Haste
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "attacker_declared"],
        notes: "Haste — ignores summoning sickness (CR 302.6). Deferred for attack proof: needs \
                DeclareAttackers step.",
    },
    CoveragePlanEntry {
        rule: "702.11", // Hexproof
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "creature_on_battlefield"],
        notes: "Hexproof — can't be the target of opponents' spells/abilities.",
    },
    CoveragePlanEntry {
        rule: "702.12", // Indestructible
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "creature_on_battlefield"],
        notes: "Indestructible — not destroyed by lethal damage or 'destroy' (CR 702.12b).",
    },
    CoveragePlanEntry {
        rule: "702.15", // Lifelink
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "player_life"],
        notes: "Lifelink — damage also gains its controller that much life (CR 702.15b).",
    },
    CoveragePlanEntry {
        rule: "702.17", // Reach
        section: 702,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_has_keyword"],
        notes: "Reach — may block creatures with flying (CR 509.1b).",
    },
    CoveragePlanEntry {
        rule: "702.19", // Trample
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_has_keyword", "player_life"],
        notes: "Trample — excess combat damage assigned to the player (CR 702.19c).",
    },
    CoveragePlanEntry {
        rule: "702.20", // Vigilance
        section: 702,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_has_keyword"],
        notes: "Vigilance — attacking doesn't cause the creature to tap (CR 508.1g).",
    },
    CoveragePlanEntry {
        rule: "702.111", // Menace (NOT 702.110, which is Exploit)
        section: 702,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_has_keyword"],
        notes: "Menace — can't be blocked except by two or more creatures (CR 702.111b).",
    },
];
