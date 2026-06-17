//! Registry + snapshot invariants. These run under `cargo test -p phase-ai`
//! and gate any change that adds/renames matchups or features.

use super::snapshots::{load_snapshot_at, snapshot_path};
use super::{all_matchups, find_matchup, resolve_deck_ref, DeckRef, FeatureKind};

#[test]
fn every_feature_kind_is_exercised() {
    let matchups = all_matchups();
    for kind in FeatureKind::ALL {
        let exercised = matchups
            .iter()
            .any(|m| m.exercises.iter().any(|k| k == kind));
        assert!(
            exercised,
            "FeatureKind::{kind:?} is not exercised by any MatchupSpec — add a matchup that \
             includes it in `exercises`, or remove the variant if the feature is gone."
        );
    }
}

/// Cross-check against the gate-covered `DeckFeatures` axes: landfall,
/// mana_ramp, tribal, control, aristocrats, artifacts, enchantments,
/// aggro_pressure, tokens_wide, plus_one_counters, spellslinger_prowess — 11
/// axes, each with a dedicated `MatchupSpec`. When a new gate-covered axis is
/// added, this assertion fails until `FeatureKind::ALL` is updated to match.
#[test]
fn feature_kind_matches_deck_features_field_count() {
    assert_eq!(
        FeatureKind::ALL.len(),
        11,
        "FeatureKind::ALL is out of sync with DeckFeatures — add the new variant."
    );
}

#[test]
fn every_snapshot_loads() {
    for spec in all_matchups() {
        for (label, deck) in [("p0", &spec.p0), ("p1", &spec.p1)] {
            if let Some(path) = snapshot_path(deck) {
                let snap = load_snapshot_at(&path).unwrap_or_else(|e| {
                    panic!(
                        "matchup `{}` {label} snapshot at {} failed to load: {e}",
                        spec.id,
                        path.display()
                    )
                });
                assert!(
                    !snap.cards.is_empty(),
                    "matchup `{}` {label} snapshot at {} has zero cards",
                    spec.id,
                    path.display()
                );
                assert!(
                    snap.cards.len() >= 40,
                    "matchup `{}` {label} snapshot at {} has only {} cards — below the \
                     playable-floor of 40",
                    spec.id,
                    path.display(),
                    snap.cards.len()
                );
            }
        }
    }
}

#[test]
fn all_matchup_ids_unique() {
    let ids: Vec<&str> = all_matchups().iter().map(|m| m.id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        ids.len(),
        sorted.len(),
        "duplicate matchup IDs in registry: {ids:?}"
    );
}

/// The pre-refactor `ai_duel.rs` hardcoded 16 matchup IDs in a match block.
/// Every one of those IDs MUST remain resolvable by `find_matchup` so existing
/// CLI invocations (`cargo run --bin ai-duel -- --matchup prowess-mirror`)
/// keep working.
#[test]
fn matchup_ids_preserved() {
    const LEGACY_IDS: &[&str] = &[
        "red-vs-green",
        "blue-vs-green",
        "red-vs-blue",
        "black-vs-green",
        "white-vs-red",
        "black-vs-blue",
        "red-mirror",
        "green-mirror",
        "blue-mirror",
        "azorius-vs-prowess",
        "azorius-vs-gruul",
        "delver-vs-prowess",
        "azorius-vs-green",
        "delver-vs-green",
        "prowess-vs-green",
        "prowess-mirror",
    ];
    for id in LEGACY_IDS {
        assert!(
            find_matchup(id).is_some(),
            "legacy matchup id `{id}` no longer resolves via find_matchup"
        );
    }
}

/// Guards the deck-pool gap that `ArtifactSynergyPolicy`'s merge gate depends
/// on: the `affinity-mirror` matchup is tagged `FeatureKind::Artifacts`, which
/// is only honest if the deck actually clears `artifacts::COMMITMENT_FLOOR`
/// (otherwise `ArtifactSynergyPolicy::activation` returns `None` and the gate
/// runs the policy dormant — the exact failure mode flagged in review). This
/// resolves the real snapshot through the card database and asserts the
/// artifacts feature both detects payoffs and crosses the activation floor, so
/// the gate's artifacts coverage cannot silently regress to a non-artifact deck.
///
/// `#[ignore]` because it needs the full `card-data.json` export, which the
/// other suite invariants intentionally avoid. Run with the card data present:
///   `cargo test -p phase-ai -- --ignored affinity_mirror_deck_activates_artifact_synergy`
/// honoring `PHASE_CARDS_PATH` (defaults to the workspace `data/` directory).
#[test]
#[ignore = "needs full card-data.json export; run with --ignored"]
fn affinity_mirror_deck_activates_artifact_synergy() {
    use crate::features::artifacts::{detect, COMMITMENT_FLOOR};
    use engine::database::CardDatabase;
    use engine::game::{resolve_player_deck_list, PlayerDeckList};
    use std::path::PathBuf;

    let data_root = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data")));
    let db_path = data_root.join("card-data.json");
    let db = CardDatabase::from_export(&db_path)
        .unwrap_or_else(|e| panic!("failed to load card database from {db_path:?}: {e}"));

    let spec = find_matchup("affinity-mirror").expect("affinity-mirror matchup must resolve");
    assert!(
        spec.exercises.contains(&FeatureKind::Artifacts),
        "affinity-mirror must claim to exercise FeatureKind::Artifacts"
    );

    let names = resolve_deck_ref(&spec.p0).expect("affinity-mirror p0 snapshot must resolve");
    let payload = resolve_player_deck_list(
        &db,
        &PlayerDeckList {
            main_deck: names,
            ..Default::default()
        },
    );
    let feature = detect(&payload.main_deck);

    assert!(
        feature.payoff_count >= 1,
        "affinity deck must contain at least one artifact-cost payoff (affinity/improvise/count), \
         got payoff_count={} artifact_count={}",
        feature.payoff_count,
        feature.artifact_count
    );
    assert!(
        feature.commitment >= COMMITMENT_FLOOR,
        "affinity deck must clear COMMITMENT_FLOOR ({COMMITMENT_FLOOR}) so ArtifactSynergyPolicy \
         activates during the gate; got commitment={} (payoff={}, enabler={}, artifacts={})",
        feature.commitment,
        feature.payoff_count,
        feature.enabler_count,
        feature.artifact_count
    );
}

/// Enchantments sibling of `affinity_mirror_deck_activates_artifact_synergy`:
/// guards that the `enchantress-mirror` matchup tagged `FeatureKind::Enchantments`
/// actually clears `enchantments::COMMITMENT_FLOOR`, so the required gate runs
/// `EnchantmentsPayoffPolicy` active (not dormant — the review blocker). Resolves
/// the real snapshot through the card database and asserts the feature detects
/// enchantress/constellation payoffs and crosses the activation floor.
///
/// `#[ignore]` because it needs the full `card-data.json` export. Run with:
///   `cargo test -p phase-ai -- --ignored enchantress_mirror_deck_activates_enchantments_payoff`
#[test]
#[ignore = "needs full card-data.json export; run with --ignored"]
fn enchantress_mirror_deck_activates_enchantments_payoff() {
    use crate::features::enchantments::{detect, COMMITMENT_FLOOR};
    use engine::database::CardDatabase;
    use engine::game::{resolve_player_deck_list, PlayerDeckList};
    use std::path::PathBuf;

    let data_root = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data")));
    let db_path = data_root.join("card-data.json");
    let db = CardDatabase::from_export(&db_path)
        .unwrap_or_else(|e| panic!("failed to load card database from {db_path:?}: {e}"));

    let spec = find_matchup("enchantress-mirror").expect("enchantress-mirror matchup must resolve");
    assert!(
        spec.exercises.contains(&FeatureKind::Enchantments),
        "enchantress-mirror must claim to exercise FeatureKind::Enchantments"
    );

    let names = resolve_deck_ref(&spec.p0).expect("enchantress-mirror p0 snapshot must resolve");
    let payload = resolve_player_deck_list(
        &db,
        &PlayerDeckList {
            main_deck: names,
            ..Default::default()
        },
    );
    let feature = detect(&payload.main_deck);

    assert!(
        feature.payoff_count >= 1,
        "enchantress deck must contain at least one enchantress/constellation payoff, \
         got payoff_count={} enchantment_count={}",
        feature.payoff_count,
        feature.enchantment_count
    );
    assert!(
        feature.commitment >= COMMITMENT_FLOOR,
        "enchantress deck must clear COMMITMENT_FLOOR ({COMMITMENT_FLOOR}) so \
         EnchantmentsPayoffPolicy activates during the gate; got commitment={} \
         (payoff={}, enchantments={})",
        feature.commitment,
        feature.payoff_count,
        feature.enchantment_count
    );
}

#[test]
fn inline_decks_resolve_to_60_cards() {
    for spec in all_matchups() {
        for (label, deck) in [("p0", &spec.p0), ("p1", &spec.p1)] {
            if let DeckRef::Inline { build, .. } = deck {
                let cards = build();
                assert_eq!(
                    cards.len(),
                    60,
                    "matchup `{}` {label} inline deck resolves to {} cards (expected 60)",
                    spec.id,
                    cards.len()
                );
            }
        }
    }
}
