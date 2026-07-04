//! Release-gate for hiding configured Magic sets from the playable card pool.
//!
//! A single environment variable, `GATED_SETS` (comma-separated set codes,
//! case-insensitive), is read at card-data / set-list / draft-pool *generation*
//! time. When unset or empty, gating is a no-op and the generated artifacts are
//! identical to ungated builds — existing builds are unaffected.
//!
//! To gate a set before release, generate with `GATED_SETS=MSH,MSC,TMSH`. Sets
//! whose MTGJSON `releaseDate` is on or before the generation "as of" date
//! (`GATED_SETS_AS_OF`, defaulting to UTC today) are **automatically ungated**
//! even when still listed in `GATED_SETS` — so a forgotten env cleanup after
//! release cannot leave cards marked Banned in every format (issue #4365).
//!
//! This is data-pipeline tooling, not game-rules logic, so no Comprehensive
//! Rules annotations apply.

use std::collections::HashSet;

use super::legality::{CardLegalities, LegalityFormat, LegalityStatus};
use super::set_catalog::{gated_sets_as_of, ReleaseDate, SetCatalog};

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
/// (as produced by [`parse_gated_sets`] or [`effective_gated_sets`]).
pub fn is_set_gated(code: &str, gated: &HashSet<String>) -> bool {
    !gated.is_empty() && gated.contains(&code.to_uppercase())
}

/// Filter `configured` gated sets to those whose release date is still in the
/// future relative to `as_of`. Sets without a `releaseDate` in the catalog stay
/// gated (conservative preview behavior).
pub fn effective_gated_sets(
    configured: &HashSet<String>,
    catalog: &SetCatalog,
    as_of: ReleaseDate,
) -> HashSet<String> {
    if configured.is_empty() {
        return HashSet::new();
    }
    configured
        .iter()
        .filter(|code| set_still_gated(code, catalog, as_of))
        .cloned()
        .collect()
}

fn set_still_gated(code: &str, catalog: &SetCatalog, as_of: ReleaseDate) -> bool {
    let Some(meta) = catalog.get(code) else {
        // Unknown to SetList — stay gated when explicitly configured.
        return true;
    };
    !meta.is_released_as_of(as_of)
}

/// Resolve the gated-set list for the current generation run: `GATED_SETS` from
/// the environment, minus any sets whose MTGJSON release date has passed.
pub fn resolve_gated_sets(catalog: &SetCatalog) -> HashSet<String> {
    let configured = gated_sets_from_env();
    let as_of = gated_sets_as_of();
    let effective = effective_gated_sets(&configured, catalog, as_of);
    if !configured.is_empty() {
        let unlocked: Vec<&str> = configured
            .iter()
            .filter(|code| !effective.contains(*code))
            .map(String::as_str)
            .collect();
        if !unlocked.is_empty() {
            eprintln!(
                "Set gating: auto-unlocked {} set(s) past release date (as of {}-{:02}-{:02}): {}",
                unlocked.len(),
                as_of.year,
                as_of.month,
                as_of.day,
                unlocked.join(",")
            );
        }
    }
    effective
}

/// Test helper: build a minimal catalog entry for gating tests.
#[cfg(test)]
pub(crate) fn test_set_meta(
    code: &str,
    release_date: &str,
    set_type: &str,
) -> super::set_catalog::SetMeta {
    super::set_catalog::SetMeta {
        code: code.to_uppercase(),
        name: code.to_string(),
        release_date: super::set_catalog::ReleaseDate::parse(release_date),
        set_type: Some(set_type.to_string()),
        is_online_only: false,
        parent_code: None,
    }
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
    use crate::database::set_catalog::{ReleaseDate, SetCatalog};

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

    #[test]
    fn effective_gated_sets_auto_unlocks_past_release() {
        let configured = set(&["MSH", "MSC", "DOM"]);
        let mut catalog = SetCatalog::default();
        catalog.insert_test_meta(test_set_meta("MSH", "2026-06-26", "expansion"));
        catalog.insert_test_meta(test_set_meta("MSC", "2026-06-26", "commander"));
        catalog.insert_test_meta(test_set_meta("DOM", "2018-04-27", "expansion"));
        let as_of = ReleaseDate::parse("2026-06-30").unwrap();
        let effective = effective_gated_sets(&configured, &catalog, as_of);
        // MSH/MSC/DOM all released — nothing left gated.
        assert!(effective.is_empty());
    }

    #[test]
    fn effective_gated_sets_keeps_future_release() {
        let configured = set(&["MSH", "FUT"]);
        let mut catalog = SetCatalog::default();
        catalog.insert_test_meta(test_set_meta("MSH", "2026-06-26", "expansion"));
        catalog.insert_test_meta(test_set_meta("FUT", "2026-12-01", "expansion"));
        let as_of = ReleaseDate::parse("2026-06-30").unwrap();
        let effective = effective_gated_sets(&configured, &catalog, as_of);
        assert!(!effective.contains("MSH"));
        assert!(effective.contains("FUT"));
    }

    #[test]
    fn effective_gated_sets_unknown_set_stays_gated() {
        let configured = set(&["MYSTERY"]);
        let catalog = SetCatalog::default();
        let as_of = ReleaseDate::parse("2026-06-30").unwrap();
        let effective = effective_gated_sets(&configured, &catalog, as_of);
        assert!(effective.contains("MYSTERY"));
    }
}
