//! Build-time exclusion of cards Wizards of the Coast officially removed and
//! banned in every format for racist or culturally offensive depictions (2020
//! announcement). These names are dropped from every generated artifact — the
//! card database and the precon deck listings — so they never surface in
//! search, deckbuilding, coverage, or gameplay.
//!
//! This is data-pipeline tooling, not game-rules logic, so no Comprehensive
//! Rules annotations apply.

/// Officially-removed card names, lowercased for case-insensitive EXACT
/// matching. Exact match is mandatory: a substring match on "crusade",
/// "imprison", or "cleanse" would wrongly drop legitimate cards such as
/// "Cathars' Crusade", the ~27 "...Crusader" cards, "Imprisoned in the Moon",
/// and "Suncleanser".
pub const REMOVED_OFFENSIVE_CARDS: [&str; 7] = [
    "invoke prejudice",
    "cleanse",
    "stone-throwing devils",
    "pradesh gypsies",
    "jihad",
    "imprison",
    "crusade",
];

/// True if `name` is an officially-removed card that must be excluded from all
/// build outputs. EXACT case-insensitive match against [`REMOVED_OFFENSIVE_CARDS`].
pub fn is_removed_offensive_card(name: &str) -> bool {
    REMOVED_OFFENSIVE_CARDS.contains(&name.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_names_case_insensitively() {
        assert!(is_removed_offensive_card("Crusade"));
        assert!(is_removed_offensive_card("crusade"));
        assert!(is_removed_offensive_card("CRUSADE"));
        assert!(is_removed_offensive_card("Stone-Throwing Devils"));
        assert!(is_removed_offensive_card("Invoke Prejudice"));
        assert!(is_removed_offensive_card("Pradesh Gypsies"));
        assert!(is_removed_offensive_card("Imprison"));
        assert!(is_removed_offensive_card("Jihad"));
        assert!(is_removed_offensive_card("Cleanse"));
    }

    #[test]
    fn does_not_match_substring_lookalikes() {
        // These are legitimate cards that share a substring and must be kept.
        assert!(!is_removed_offensive_card("Cathars' Crusade"));
        assert!(!is_removed_offensive_card("Mirran Crusader"));
        assert!(!is_removed_offensive_card("Phyrexian Crusader"));
        assert!(!is_removed_offensive_card("Imprisoned in the Moon"));
        assert!(!is_removed_offensive_card("Imprison This Insolent Wretch"));
        assert!(!is_removed_offensive_card("Suncleanser"));
        assert!(!is_removed_offensive_card(""));
    }
}
