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
        cr_sections: &[119, 104, 704],
        description: "Assert a player's life total equals an expected value (CR 119).",
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
        cr_sections: &[104],
        description: "Assert the game is still in progress.",
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
        cr_sections: &[120, 510, 704],
        description: "Assert marked damage on a creature (CR 120.3).",
    },
    PredicateEntry {
        id: "phase_is",
        assertion_kind: "phase_is",
        cr_sections: &[500, 501, 502, 503, 504, 505, 506, 512],
        description: "Assert the current turn step/phase.",
    },
    PredicateEntry {
        id: "hand_count_equals",
        assertion_kind: "hand_count_equals",
        cr_sections: &[402, 121],
        description: "Assert exact hand size.",
    },
    PredicateEntry {
        id: "hand_count_at_least",
        assertion_kind: "hand_count_at_least",
        cr_sections: &[402, 121],
        description: "Assert minimum hand size.",
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
}
