//! Issue #4365 — Marvel Super Heroes release-gating must auto-unlock after the
//! set's MTGJSON `releaseDate`, without fabricating format legalities locally.

use engine::database::legality::LegalityStatus;
use engine::database::set_catalog::{ReleaseDate, SetCatalog, SetMeta};
use engine::database::set_gating::{
    all_formats_banned, effective_gated_sets, is_card_gated, resolve_gated_sets,
};
use std::collections::HashSet;

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
    assert!(!is_card_gated(&["MSH".to_string()], &effective));
}

#[test]
fn msh_stays_gated_before_release_date() {
    let catalog = msh_catalog();
    let configured: HashSet<String> = ["MSH", "MSC", "TMSH"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let as_of = ReleaseDate::parse("2026-06-25").unwrap();
    let effective = effective_gated_sets(&configured, &catalog, as_of);
    assert_eq!(effective, configured);
    assert!(is_card_gated(&["MSH".to_string()], &effective));
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
fn resolve_gated_sets_unlocks_msh_when_env_still_lists_it() {
    std::env::set_var("GATED_SETS", "MSH,MSC,TMSH");
    std::env::set_var("GATED_SETS_AS_OF", "2026-06-30");

    let effective = resolve_gated_sets(&msh_catalog());
    assert!(
        effective.is_empty(),
        "resolve_gated_sets must drop released sets even when GATED_SETS is stale"
    );

    std::env::remove_var("GATED_SETS");
    std::env::remove_var("GATED_SETS_AS_OF");
}

#[test]
fn unknown_set_stays_gated_when_configured() {
    let configured: HashSet<String> = ["MYSTERY"].iter().map(|s| s.to_string()).collect();
    let as_of = ReleaseDate::parse("2026-06-30").unwrap();
    let effective = effective_gated_sets(&configured, &SetCatalog::default(), as_of);
    assert!(effective.contains("MYSTERY"));
}
