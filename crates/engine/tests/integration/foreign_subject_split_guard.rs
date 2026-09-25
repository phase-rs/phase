//! A compound subject ("Insects and Spiders you control …") parses to one static, not a split.

use engine::game::keywords::has_keyword_kind;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, ControllerRef, StaticCondition, TargetFilter, TypeFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::phase::Phase;

/// The Swarmweaver's printed Oracle text.
const THE_SWARMWEAVER: &str = "When The Swarmweaver enters, create two 1/1 black and green Insect creature tokens with flying.\nDelirium — As long as there are four or more card types among cards in your graveyard, Insects and Spiders you control get +1/+1 and have deathtouch.";

/// The compound subject "Insects and Spiders you control" must resolve as ONE
/// `Or` filter, not be split by `try_split_and_foreign_subject_grant` into a
/// false SelfRef primary plus a Spider-only companion.
#[test]
fn the_swarmweaver_compound_subject_anthem_is_not_split() {
    let parsed = parse_oracle_text(
        THE_SWARMWEAVER,
        "The Swarmweaver",
        &["Delirium".to_string()],
        &["Artifact".to_string(), "Creature".to_string()],
        &["Scarecrow".to_string()],
    );
    assert_eq!(
        parsed.statics.len(),
        1,
        "the Delirium anthem must stay one static, got {:?}",
        parsed.statics
    );
    let anthem = &parsed.statics[0];
    match &anthem.affected {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(
                filters.len(),
                2,
                "expected Insect-you-control + Spider-you-control conjuncts, got {filters:?}"
            );
            for subtype in ["Insect", "Spider"] {
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&TypeFilter::Creature)
                                && tf.type_filters.contains(&TypeFilter::Subtype(subtype.to_string()))
                                && tf.controller == Some(ControllerRef::You)
                    )),
                    "expected a Creature+Subtype({subtype:?}) filter scoped to You, got {filters:?}"
                );
            }
        }
        other => {
            panic!("affected must be Or(Insect You, Spider You) subtype filters, got {other:?}")
        }
    }
    assert_eq!(
        anthem.modifications,
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Deathtouch
            },
        ],
        "expected exactly [+1/+1, Deathtouch], got {:?}",
        anthem.modifications
    );
    assert!(
        !matches!(anthem.condition, Some(StaticCondition::Unrecognized { .. })),
        "the Delirium condition must not fall back to Unrecognized, got {:?}",
        anthem.condition
    );
}

/// Recompute layers and read whether `id` currently has a keyword of `kind`.
/// Mirrors `has_kw` in `angelic_field_marshal_lieutenant_2885.rs`, keyed on
/// `KeywordKind` rather than a full `Keyword` value so a Ward mana-cost payload
/// need not be reconstructed exactly to check for its presence.
fn has_kw_kind(
    runner: &mut engine::game::scenario::GameRunner,
    id: ObjectId,
    kind: KeywordKind,
) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword_kind(&runner.state().objects[&id], kind)
}

/// Thorin Oakenshield's printed Oracle text.
const THORIN_OAKENSHIELD: &str = "Trample\nStoried (If you control three or more artifacts, legendaries, and/or Sagas, you have an enduring story for the rest of the game.)\nAs long as you have an enduring story, artifacts and creatures you control have ward {1}.";

/// The compound subject "artifacts and creatures you control" must reach
/// artifacts you control (not only creatures you control), and only while you
/// have an enduring story.
#[test]
fn thorin_oakenshield_ward_reaches_artifacts_and_creatures_with_story() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thorin = scenario
        .add_creature_from_oracle(P0, "Thorin Oakenshield", 3, 2, THORIN_OAKENSHIELD)
        .with_subtypes(vec!["Dwarf", "Noble"])
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Fodder Artifact", "")
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let opp_bear = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();

    // No enduring story: none of P0's permanents have Ward.
    assert!(!has_kw_kind(&mut runner, thorin, KeywordKind::Ward));
    assert!(!has_kw_kind(&mut runner, artifact, KeywordKind::Ward));
    assert!(!has_kw_kind(&mut runner, bear, KeywordKind::Ward));

    // CR 702.195a + CR 702.21a: with an enduring story, both the artifact and
    // the creature conjuncts gain Ward — the opponent's creature does not.
    runner.state_mut().enduring_story.insert(P0);
    assert!(has_kw_kind(&mut runner, thorin, KeywordKind::Ward));
    assert!(has_kw_kind(&mut runner, artifact, KeywordKind::Ward));
    assert!(has_kw_kind(&mut runner, bear, KeywordKind::Ward));
    assert!(!has_kw_kind(&mut runner, opp_bear, KeywordKind::Ward));
}
