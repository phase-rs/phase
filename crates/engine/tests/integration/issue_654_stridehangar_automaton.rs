//! GitHub issue #654 — Stridehangar Automaton must create an additional Thopter
//! when artifact tokens would be created under your control (CR 614.1a / CR 111.1).

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::phase::Phase;

const STRIDEHANGAR_REPLACEMENT: &str = "If one or more artifact tokens would be created under your control, those tokens plus an additional 1/1 colorless Thopter artifact creature token with flying are created instead.";
const TREASURE_MAKER_ETB: &str = "When this creature enters, create a Treasure token.";
const SQUIRREL_MAKER_ETB: &str =
    "When this creature enters, create a 1/1 green Squirrel creature token.";

fn tokens_with_subtype(runner: &engine::game::scenario::GameRunner, subtype: &str) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == subtype))
        })
        .count()
}

fn artifact_tokens(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|o| {
                o.is_token && o.card_types.core_types.contains(&CoreType::Artifact)
            })
        })
        .count()
}

#[test]
fn stridehangar_adds_thopter_when_artifact_token_would_be_created() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Stridehangar Automaton", 3, 3, STRIDEHANGAR_REPLACEMENT);
    let treasure_maker = scenario
        .add_creature_to_hand_from_oracle(P0, "Treasure Maker", 1, 1, TREASURE_MAKER_ETB)
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    assert_eq!(artifact_tokens(&runner), 0);

    runner.cast(treasure_maker).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        tokens_with_subtype(&runner, "Treasure"),
        1,
        "ETB must create the primary Treasure token"
    );
    assert_eq!(
        tokens_with_subtype(&runner, "Thopter"),
        1,
        "Stridehangar replacement must append a Thopter; waiting_for={:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        artifact_tokens(&runner),
        2,
        "Treasure plus appended Thopter must both be artifact tokens on the battlefield"
    );
}

#[test]
fn stridehangar_replacement_does_not_fire_for_non_artifact_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Stridehangar Automaton", 3, 3, STRIDEHANGAR_REPLACEMENT);
    let squirrel_maker = scenario
        .add_creature_to_hand_from_oracle(P0, "Squirrel Maker", 1, 1, SQUIRREL_MAKER_ETB)
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.cast(squirrel_maker).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(tokens_with_subtype(&runner, "Squirrel"), 1);
    assert_eq!(
        tokens_with_subtype(&runner, "Thopter"),
        0,
        "Non-artifact creature tokens must not trigger the artifact-token replacement"
    );
}
