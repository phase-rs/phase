//! Release-gate for hiding configured Magic sets from the playable card pool.
//!
//! A single environment variable, `GATED_SETS` (comma-separated set codes,
//! case-insensitive), is read at card-data / set-list / draft-pool *generation*
//! time. When unset or empty, gating is a no-op and the generated artifacts are
//! identical to ungated builds — existing builds are unaffected.
//!
//! To gate a set before release, generate with `GATED_SETS=MSH,MSC,TMSH`. To
//! unlock on release, regenerate without the variable. There are no hardcoded
//! set codes here: the codes come only from the environment.
//!
//! This is data-pipeline tooling, not game-rules logic, so no Comprehensive
//! Rules annotations apply.

use std::collections::HashSet;

use super::legality::{CardLegalities, LegalityFormat, LegalityStatus};

/// Name of the environment variable that lists gated set codes.
pub const GATED_SETS_ENV: &str = "GATED_SETS";

/// Parse `GATED_SETS` into an uppercased set of set codes.
///
/// Empty/unset → empty set (no gating). Whitespace around each code is trimmed
/// and empty entries are dropped, so `"MSH, MSC ,"` yields `{MSH, MSC}`. Codes
/// are uppercased for case-insensitive comparison against MTGJSON set codes.
pub fn gated_sets_from_env() -> HashSet<String> {
    parse_gated_sets(&std::env::var(GATED_SETS_ENV).unwrap_or_default())
}

/// Parse a raw comma-separated string into an uppercased set of set codes.
///
/// Factored out from [`gated_sets_from_env`] so the parsing logic is unit
/// testable without mutating process environment.
pub fn parse_gated_sets(raw: &str) -> HashSet<String> {
    raw.split(',')
        .map(|code| code.trim().to_uppercase())
        .filter(|code| !code.is_empty())
        .collect()
}

/// Whether a single set code should be hidden from set-list / draft-pool output.
///
/// Comparison is case-insensitive; `gated` is expected to already be uppercased
/// (as produced by [`parse_gated_sets`]).
pub fn is_set_gated(code: &str, gated: &HashSet<String>) -> bool {
    !gated.is_empty() && gated.contains(&code.to_uppercase())
}

/// Reprint-aware predicate: should this card be dropped from the playable pool?
///
/// A card is gated only when **every** set it has been printed in is gated —
/// i.e. it is obtainable exclusively through gated sets (a new card for the
/// gated set). A card that is also printed in any non-gated set (a reprint such
/// as Divine Visitation) is always kept: gating a set must never ban a card
/// that is legally available elsewhere.
///
/// A card with no recorded printings is never gated (there is no gated printing
/// to key off, and dropping it would be a silent data-loss surprise).
pub fn is_card_gated(printings: &[String], gated: &HashSet<String>) -> bool {
    if gated.is_empty() || printings.is_empty() {
        return false;
    }
    printings
        .iter()
        .all(|set| gated.contains(&set.to_uppercase()))
}

/// Legalities map marking a card `Banned` in every format.
///
/// The hybrid release-gate keeps a gated card in card-data (browsable) but
/// overrides its legalities to this map, so it is excluded from every
/// format-scoped deck-builder pool (`database::search` drops non-legal cards).
/// The override is reversed on unlock — regenerate without `GATED_SETS` to
/// restore the card's real MTGJSON legalities. This is a release-gate override,
/// not a statement about the card's true format legality.
pub fn all_formats_banned() -> CardLegalities {
    LegalityFormat::ALL
        .into_iter()
        .map(|format| (format, LegalityStatus::Banned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(codes: &[&str]) -> HashSet<String> {
        codes.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn parse_empty_yields_empty_set() {
        assert!(parse_gated_sets("").is_empty());
        assert!(parse_gated_sets("   ").is_empty());
        assert!(parse_gated_sets(",, ,").is_empty());
    }

    #[test]
    fn parse_trims_uppercases_and_filters_blanks() {
        assert_eq!(
            parse_gated_sets("MSH,MSC, TMSH"),
            set(&["MSH", "MSC", "TMSH"])
        );
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(parse_gated_sets("msh, Msc"), set(&["MSH", "MSC"]));
    }

    #[test]
    fn set_gating_is_noop_when_unset() {
        let gated = set(&[]);
        assert!(!is_set_gated("MSH", &gated));
    }

    #[test]
    fn set_gating_matches_case_insensitively() {
        let gated = set(&["MSH", "MSC"]);
        assert!(is_set_gated("msh", &gated));
        assert!(is_set_gated("MSH", &gated));
        assert!(!is_set_gated("DOM", &gated));
    }

    #[test]
    fn card_gated_when_all_printings_gated() {
        let gated = set(&["MSH", "MSC", "TMSH"]);
        assert!(is_card_gated(
            &["MSH".to_string(), "MSC".to_string()],
            &gated
        ));
    }

    #[test]
    fn card_kept_when_reprinted_in_non_gated_set() {
        let gated = set(&["MSH", "MSC", "TMSH"]);
        // Divine Visitation-style reprint: gated set + a legal Standard set.
        assert!(!is_card_gated(
            &["MSH".to_string(), "DOM".to_string()],
            &gated
        ));
    }

    #[test]
    fn card_kept_when_no_gated_printing() {
        let gated = set(&["MSH", "MSC", "TMSH"]);
        assert!(!is_card_gated(
            &["DOM".to_string(), "M21".to_string()],
            &gated
        ));
    }

    #[test]
    fn card_kept_when_no_printings_recorded() {
        let gated = set(&["MSH"]);
        assert!(!is_card_gated(&[], &gated));
    }

    #[test]
    fn card_never_gated_when_env_empty() {
        let gated = set(&[]);
        assert!(!is_card_gated(&["MSH".to_string()], &gated));
    }

    #[test]
    fn card_gating_is_case_insensitive() {
        let gated = set(&["MSH"]);
        assert!(is_card_gated(&["msh".to_string()], &gated));
    }

    #[test]
    fn all_formats_banned_covers_every_format() {
        let banned = all_formats_banned();
        assert_eq!(banned.len(), LegalityFormat::ALL.len());
        assert!(banned
            .values()
            .all(|status| *status == LegalityStatus::Banned));
        // Every known format must be present (the hybrid gate bans everywhere).
        assert!(LegalityFormat::ALL
            .iter()
            .all(|format| banned.get(format) == Some(&LegalityStatus::Banned)));
    }
}
