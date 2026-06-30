//! Issue #4365 — Marvel Super Heroes cards must not remain Banned / not-legal
//! after the set's release date when `GATED_SETS` still lists MSH or MTGJSON
//! has not yet populated AtomicCards legalities.

use engine::database::legality::{LegalityFormat, LegalityStatus};
use engine::database::legality_inference::{self, InferenceContext};
use engine::database::set_catalog::{ReleaseDate, SetCatalog, SetMeta};
use engine::database::set_gating::{all_formats_banned, effective_gated_sets, is_card_gated};
use engine::types::card::Rarity;
use std::collections::{BTreeMap, BTreeSet, HashSet};

fn msh_catalog() -> SetCatalog {
    let mut catalog = SetCatalog::default();
    for (code, set_type) in [
        ("MSH", "expansion"),
        ("MSC", "commander"),
        ("TMSH", "expansion"),
    ] {
        catalog.insert_test_meta(SetMeta {
            code: code.into(),
            name: code.into(),
            release_date: ReleaseDate::parse("2026-06-26"),
            set_type: Some(set_type.into()),
            is_online_only: false,
            parent_code: None,
        });
    }
    catalog
}

#[test]
fn msh_auto_unlocks_from_gated_sets_after_release_date() {
    std::env::set_var("GATED_SETS", "MSH,MSC,TMSH");
    std::env::set_var("GATED_SETS_AS_OF", "2026-06-30");

    let catalog = msh_catalog();
    let configured: HashSet<String> = ["MSH", "MSC", "TMSH"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let as_of = ReleaseDate::parse("2026-06-30").unwrap();
    let effective = effective_gated_sets(&configured, &catalog, as_of);
    assert!(
        effective.is_empty(),
        "MSH/MSC/TMSH must auto-unlock after 2026-06-26 release"
    );

    let gated = effective;
    assert!(!is_card_gated(&["MSH".to_string()], &gated));

    std::env::remove_var("GATED_SETS");
    std::env::remove_var("GATED_SETS_AS_OF");
}

#[test]
fn msh_cards_are_marked_banned_only_while_gate_is_active() {
    let printings = vec!["MSH".to_string()];
    let pre_release_gated: HashSet<String> = ["MSH"].iter().map(|s| s.to_string()).collect();
    assert!(is_card_gated(&printings, &pre_release_gated));
    let banned = all_formats_banned();
    assert!(banned
        .values()
        .all(|status| *status == LegalityStatus::Banned));
}

#[test]
fn msh_expansion_cards_infer_standard_legal_when_mtgjson_legalities_empty() {
    std::env::set_var("GATED_SETS_AS_OF", "2026-06-30");
    let catalog = msh_catalog();
    let printings = vec!["MSH".to_string()];
    let rarities = BTreeSet::from([Rarity::Mythic]);
    let inferred = legality_inference::infer_missing_legalities(&InferenceContext {
        printings: &printings,
        catalog: &catalog,
        leadership_skills: None,
        type_line: Some("Legendary Creature — Human Hero"),
        rarities: &rarities,
    });

    assert_eq!(
        inferred.get(&LegalityFormat::Standard),
        Some(&LegalityStatus::Legal),
        "MSH expansion cards must be Standard-legal after release"
    );
    assert_eq!(
        inferred.get(&LegalityFormat::Commander),
        Some(&LegalityStatus::Legal)
    );
    assert_ne!(
        inferred.get(&LegalityFormat::Premodern),
        Some(&LegalityStatus::Legal)
    );

    let export: BTreeMap<_, _> = inferred
        .iter()
        .map(|(format, status)| {
            (
                format.as_key().to_string(),
                status.as_export_str().to_string(),
            )
        })
        .collect();
    assert_eq!(export.get("standard"), Some(&"legal".to_string()));
    assert_eq!(export.get("banned"), None);

    std::env::remove_var("GATED_SETS_AS_OF");
}
