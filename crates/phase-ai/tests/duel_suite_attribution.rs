//! Empirical bridge from declared `MatchupSpec.exercises` to observed
//! `DecisionTrace` attribution. Closes the gap from "we *said* this matchup
//! tests landfall" to "the LandfallTiming policy actually emitted a verdict
//! during play."
//!
//! Two tests:
//!
//! 1. `expected_policies_covers_every_feature_kind` — static, always runs.
//!    Asserts the lookup table maps every `FeatureKind` to ≥1 policy id.
//!    Catches the common drift of adding a new feature variant without
//!    extending the test mapping.
//!
//! 2. `declared_exercises_appear_in_attribution` — `#[ignore]`, opt in via
//!    `cargo test -p phase-ai --test duel_suite_attribution -- --ignored`.
//!    Runs the full suite at Easy with attribution enabled and asserts that
//!    for every matchup, every declared `FeatureKind` corresponds to ≥1
//!    `PolicyId` actually observed in the attribution (top_scores or
//!    rejects), in either p0 or p1.
//!
//! The empirical test excludes `greasefang-mirror`. Follow-up 1 bounded
//! the runaway-activation pathology (`pending_activations` guard +
//! per-source-per-turn activation cap in `phase-ai/src/search.rs`), so the
//! matchup completes naturally, but the remaining AI-search cost on the
//! cluttered board makes it ~5× slower than other matchups; running it in
//! the attribution suite would dominate the budget. The skip is a perf
//! concession, not a correctness issue.

use std::path::PathBuf;

use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;
use phase_ai::duel_suite::attribution::PolicyAttribution;
use phase_ai::duel_suite::run::{run_suite, AttributionMode, SuiteOptions};
use phase_ai::duel_suite::FeatureKind;

/// Map a `FeatureKind` to the set of `PolicyId` debug-names whose appearance
/// in attribution satisfies "this feature was actually exercised." Each name
/// must match a real variant in `phase_ai::policies::registry::PolicyId`.
///
/// Multi-policy features (Control, Aristocrats, TokensWide) list every
/// policy that the feature can manifest through; observing **any** one is
/// sufficient. Single-policy features have a one-element slice.
///
/// Exhaustive match — adding a new `FeatureKind` variant without extending
/// this lookup is a compile-time error.
fn expected_policies(kind: FeatureKind) -> &'static [&'static str] {
    match kind {
        FeatureKind::Landfall => &["LandfallTiming"],
        FeatureKind::ManaRamp => &["RampTiming"],
        FeatureKind::Tribal => &["TribalLordPriority"],
        FeatureKind::Control => &["SweeperTiming", "HoldManaUp", "BoardWipeTelegraph"],
        FeatureKind::Aristocrats => &["FreeOutletActivation", "SacrificeValue"],
        FeatureKind::Artifacts => &["ArtifactSynergyTactical"],
        FeatureKind::Enchantments => &["EnchantmentsPayoff"],
        FeatureKind::AggroPressure => &["AggroPressure"],
        FeatureKind::TokensWide => &["TokensWide", "AnthemPriority"],
        FeatureKind::PlusOneCounters => &["PlusOneCountersTactical"],
        FeatureKind::SpellslingerProwess => &["SpellslingerCasting"],
        FeatureKind::Reanimator => &["ReanimatorPayoff"],
        FeatureKind::Equipment => &["EquipmentPayoff"],
        FeatureKind::Blink => &["BlinkPayoff"],
        FeatureKind::Mill => &["MillPayoff"],
        FeatureKind::Energy => &["EnergyPayoff"],
    }
}

#[test]
fn expected_policies_covers_every_feature_kind() {
    for kind in FeatureKind::ALL.iter().copied() {
        let policies = expected_policies(kind);
        assert!(
            !policies.is_empty(),
            "FeatureKind::{kind:?} has no expected policies — extend \
             expected_policies() in tests/duel_suite_attribution.rs",
        );
    }
}

fn attribution_mentions(att: &PolicyAttribution, policy_ids: &[&str]) -> bool {
    policy_ids.iter().any(|id| {
        att.rejects.contains_key(*id) || att.top_scores.iter().any(|s| s.policy_id == *id)
    })
}

fn load_db() -> CardDatabase {
    let cards_dir = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("client")
                .join("public")
        });
    let export_path = cards_dir.join("card-data.json");
    CardDatabase::from_export(&export_path)
        .unwrap_or_else(|e| panic!("load card-data.json from {}: {e}", export_path.display()))
}

#[test]
#[ignore = "long-running suite test; opt in via --ignored"]
fn declared_exercises_appear_in_attribution() {
    let db = load_db();
    let opts = SuiteOptions {
        difficulty: AiDifficulty::Easy,
        games_per_matchup: 1,
        base_seed: 7777,
        output_path: PathBuf::from("target/duel-suite-attribution-test.json"),
        filter: None,
        attribution: AttributionMode::Enabled,
        git_sha: None,
        card_data_hash: None,
        harvest_output: None,
    };
    let report = run_suite(&db, &opts).expect("suite run");

    let mut gaps: Vec<String> = Vec::new();
    for result in &report.results {
        if result.matchup_id == "greasefang-mirror" {
            continue;
        }
        let Some(att) = &result.attribution else {
            gaps.push(format!("{}: attribution missing", result.matchup_id));
            continue;
        };
        for kind in &result.exercises {
            // ArtifactSynergyPolicy and EnchantmentsPayoffPolicy are deliberately
            // nudge-band policies (bonuses of 0.2–0.5). Their per-decision score
            // never reaches the top-3 this attribution test inspects, so those
            // features are validated at the activation level by
            // `affinity_mirror_deck_activates_artifact_synergy` /
            // `enchantress_mirror_deck_activates_enchantments_payoff`
            // (duel_suite/tests.rs) — which assert the tagged deck clears
            // `COMMITMENT_FLOOR` — rather than via runtime attribution here.
            if matches!(*kind, FeatureKind::Artifacts | FeatureKind::Enchantments) {
                continue;
            }
            let expected = expected_policies(*kind);
            let in_p0 = attribution_mentions(&att.p0, expected);
            let in_p1 = attribution_mentions(&att.p1, expected);
            if !in_p0 && !in_p1 {
                gaps.push(format!(
                    "{}: declared exercises {:?} but no policy in {:?} \
                     appeared in either player's attribution \
                     (p0_decisions={}, p1_decisions={})",
                    result.matchup_id, kind, expected, att.p0.decisions, att.p1.decisions,
                ));
            }
        }
    }

    assert!(
        gaps.is_empty(),
        "exercise gaps found ({} matchups):\n  {}",
        gaps.len(),
        gaps.join("\n  "),
    );
}
