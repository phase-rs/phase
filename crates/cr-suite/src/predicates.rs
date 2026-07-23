//! Predicate catalog metadata — maps CR sections to assertion kinds agents can use.
//!
//! This is documentation-as-code for extending fixtures. The runner does not
//! invent game logic here; it only lists which typed assertions apply where.

/// A catalogued assertion capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateEntry {
    pub id: &'static str,
    pub assertion_kind: &'static str,
    pub cr_sections: &'static [u32],
    pub description: &'static str,
}

/// Full predicate catalog shipped with cr-suite.
pub const PREDICATE_CATALOG: &[PredicateEntry] = &[
    PredicateEntry {
        id: "player_life",
        assertion_kind: "player_life",
        cr_sections: &[104, 119, 120, 704],
        description: "Assert a player's life total equals an expected value (CR 119); \
                      damage to a player is observed here as life loss (CR 120.3a).",
    },
    PredicateEntry {
        id: "game_over",
        assertion_kind: "game_over",
        cr_sections: &[104, 704],
        description: "Assert the game has ended, optionally with a specific winner (CR 104).",
    },
    PredicateEntry {
        id: "game_not_over",
        assertion_kind: "game_not_over",
        cr_sections: &[104, 704],
        description: "Assert the game is still in progress (CR 104 / CR 704.7 continuity).",
    },
    PredicateEntry {
        id: "creature_zone",
        assertion_kind: "creature_zone",
        cr_sections: &[400, 403, 404, 406, 704],
        description: "Assert a named creature handle is in a specific zone (CR 400).",
    },
    PredicateEntry {
        id: "creature_on_battlefield",
        assertion_kind: "creature_on_battlefield",
        cr_sections: &[110, 403, 704],
        description: "Assert a creature remains on the battlefield.",
    },
    PredicateEntry {
        id: "creature_in_graveyard",
        assertion_kind: "creature_in_graveyard",
        cr_sections: &[404, 704],
        description: "Assert a creature is in a graveyard (typically after SBAs).",
    },
    PredicateEntry {
        id: "creature_damage",
        assertion_kind: "creature_damage",
        cr_sections: &[120, 510, 514, 704],
        description:
            "Assert marked damage on a creature (CR 120.3); cleared in cleanup (CR 514.2).",
    },
    PredicateEntry {
        id: "phase_is",
        assertion_kind: "phase_is",
        cr_sections: &[
            500, 501, 502, 503, 504, 505, 506, 507, 508, 509, 510, 511, 512,
        ],
        description: "Assert the current turn step/phase.",
    },
    PredicateEntry {
        id: "hand_count_equals",
        assertion_kind: "hand_count_equals",
        cr_sections: &[121, 401, 402],
        description: "Assert exact hand size (CR 402); draw changes it by one (CR 121 / CR 401).",
    },
    PredicateEntry {
        id: "hand_count_at_least",
        assertion_kind: "hand_count_at_least",
        cr_sections: &[121, 402, 504],
        description: "Assert minimum hand size; the draw step adds a card (CR 504.1).",
    },
    PredicateEntry {
        id: "stack_is_empty",
        assertion_kind: "stack_is_empty",
        cr_sections: &[405, 608],
        description: "Assert the stack has no objects (CR 405.1); true after all resolve (CR 608).",
    },
    PredicateEntry {
        id: "priority_player",
        assertion_kind: "priority_player",
        cr_sections: &[117],
        description: "Assert which player currently holds priority (CR 117.1 / CR 117.3).",
    },
    PredicateEntry {
        id: "library_count_equals",
        assertion_kind: "library_count_equals",
        cr_sections: &[401],
        description: "Assert a player's library size (CR 401.1).",
    },
    PredicateEntry {
        id: "player_poison",
        assertion_kind: "player_poison",
        cr_sections: &[122, 704],
        description: "Assert a player's poison counters (CR 122.1); 10+ loses (CR 704.5c).",
    },
    PredicateEntry {
        id: "attacker_declared",
        assertion_kind: "attacker_declared",
        cr_sections: &[508],
        description: "Assert a creature was declared as an attacker (CR 508.1). \
                      TODO: needs a DeclareAttackers runner step to reach in fixtures.",
    },
    PredicateEntry {
        id: "blocker_declared",
        assertion_kind: "blocker_declared",
        cr_sections: &[509],
        description: "Assert a creature was declared as a blocker (CR 509.1). \
                      TODO: needs a DeclareBlockers runner step to reach in fixtures.",
    },
    PredicateEntry {
        id: "creature_has_keyword",
        assertion_kind: "creature_has_keyword",
        cr_sections: &[702],
        description: "Assert a creature has a keyword after CR 613 layers (CR 702).",
    },
    PredicateEntry {
        id: "in_command_zone",
        assertion_kind: "in_command_zone",
        cr_sections: &[408],
        description: "Assert an object is in the command zone (CR 408.1).",
    },
];

/// Look up predicates that advertise coverage for a CR section.
pub fn predicates_for_section(section: u32) -> Vec<&'static PredicateEntry> {
    PREDICATE_CATALOG
        .iter()
        .filter(|p| p.cr_sections.contains(&section))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_non_empty_and_unique_ids() {
        assert!(!PREDICATE_CATALOG.is_empty());
        let mut ids: Vec<_> = PREDICATE_CATALOG.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PREDICATE_CATALOG.len());
    }

    #[test]
    fn sba_section_has_zone_and_life_predicates() {
        let preds = predicates_for_section(704);
        let kinds: Vec<_> = preds.iter().map(|p| p.assertion_kind).collect();
        assert!(kinds.contains(&"player_life") || kinds.contains(&"game_over"));
        assert!(kinds.contains(&"creature_in_graveyard") || kinds.contains(&"creature_zone"));
    }

    fn has(section: u32, kind: &str) -> bool {
        predicates_for_section(section)
            .iter()
            .any(|p| p.assertion_kind == kind)
    }

    #[test]
    fn review_section_advertisements_present() {
        // Advertisements added for #6514 review (plans must sync to sections).
        assert!(
            has(704, "game_not_over"),
            "game_not_over must advertise 704"
        );
        for step in [507, 508, 509, 510, 511] {
            assert!(has(step, "phase_is"), "phase_is must advertise {step}");
        }
        assert!(has(120, "player_life"), "player_life must advertise 120");
        assert!(
            has(504, "hand_count_at_least"),
            "hand_count_at_least must advertise 504"
        );
        assert!(
            has(514, "creature_damage"),
            "creature_damage must advertise 514"
        );
        assert!(
            has(401, "hand_count_equals"),
            "hand_count_equals must advertise 401"
        );
    }

    #[test]
    fn new_predicates_registered() {
        for id in [
            "stack_is_empty",
            "priority_player",
            "library_count_equals",
            "player_poison",
            "attacker_declared",
            "blocker_declared",
        ] {
            assert!(
                PREDICATE_CATALOG.iter().any(|p| p.id == id),
                "missing predicate {id}"
            );
        }
        assert!(has(405, "stack_is_empty"));
        assert!(has(608, "stack_is_empty"));
        assert!(has(117, "priority_player"));
        assert!(has(401, "library_count_equals"));
    }
}
