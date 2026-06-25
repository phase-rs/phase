//! Regression for GitHub issue #3997 — Sliver Gravemother grants encore equal to
//! each Sliver's mana value; activating Sliver Legion's encore must cost {5}
//! generic mana, must not offer ChooseX at 0, and must be unpayable without
//! sufficient mana.
//!
//! https://github.com/phase-rs/phase/issues/3997

use engine::game::casting::{
    activated_ability_definitions, can_activate_ability_now, handle_activate_ability,
};
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, Effect};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const GRAVEMOTHER_ORACLE: &str = "The \"legend rule\" doesn't apply to Slivers you control.\n\
Each Sliver creature card in your graveyard has encore {X}, where X is its mana value.\n\
Encore {5} ({5}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";

fn legion_mana_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![
            ManaCostShard::White,
            ManaCostShard::Blue,
            ManaCostShard::Black,
            ManaCostShard::Red,
            ManaCostShard::Green,
        ],
    }
}

fn colorless_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn encore_ability_index(state: &engine::types::game_state::GameState, source: ObjectId) -> usize {
    activated_ability_definitions(state, source)
        .into_iter()
        .find(|(_, ability)| matches!(&*ability.effect, Effect::Encore))
        .map(|(index, _)| index)
        .expect("granted encore ability")
}

#[test]
fn sliver_legion_encore_costs_mana_value_not_choose_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Sliver Gravemother", 6, 6, GRAVEMOTHER_ORACLE);

    let legion_id = scenario
        .add_creature_to_graveyard(P0, "Sliver Legion", 7, 7)
        .with_mana_cost(legion_mana_cost())
        .with_subtypes(vec!["Sliver"])
        .id();

    scenario.with_mana_pool(P0, colorless_mana(5));

    let runner = scenario.build();
    let encore_idx = encore_ability_index(runner.state(), legion_id);

    let abilities = activated_ability_definitions(runner.state(), legion_id);
    let (_, encore) = abilities
        .iter()
        .find(|(_, ability)| matches!(&*ability.effect, Effect::Encore))
        .expect("granted encore");
    let Some(AbilityCost::Composite { costs }) = &encore.cost else {
        panic!("encore cost must be composite, got {:?}", encore.cost);
    };
    assert!(
        costs
            .iter()
            .any(|c| { matches!(c, AbilityCost::Mana { cost } if *cost == ManaCost::generic(5)) }),
        "encore mana sub-cost must equal Legion's mana value (5), got {costs:?}"
    );

    assert!(
        can_activate_ability_now(runner.state(), P0, legion_id, encore_idx),
        "encore must be legal with five mana available"
    );

    let mut state = runner.state().clone();
    let waiting = handle_activate_ability(
        &mut state,
        P0,
        legion_id,
        encore_idx,
        &mut Vec::<GameEvent>::new(),
    )
    .expect("activate encore");

    assert!(
        !matches!(waiting, WaitingFor::ChooseXValue { .. }),
        "encore bound to mana value must not enter ChooseXValue, got {waiting:?}"
    );
}

#[test]
fn sliver_legion_encore_unpayable_without_mana_value_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Sliver Gravemother", 6, 6, GRAVEMOTHER_ORACLE);

    let legion_id = scenario
        .add_creature_to_graveyard(P0, "Sliver Legion", 7, 7)
        .with_mana_cost(legion_mana_cost())
        .with_subtypes(vec!["Sliver"])
        .id();

    let runner = scenario.build();
    let encore_idx = encore_ability_index(runner.state(), legion_id);

    assert!(
        !can_activate_ability_now(runner.state(), P0, legion_id, encore_idx),
        "encore must be illegal without mana to pay Legion's mana value"
    );
}

#[test]
fn sliver_legion_encore_illegal_with_partial_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Sliver Gravemother", 6, 6, GRAVEMOTHER_ORACLE);

    let legion_id = scenario
        .add_creature_to_graveyard(P0, "Sliver Legion", 7, 7)
        .with_mana_cost(legion_mana_cost())
        .with_subtypes(vec!["Sliver"])
        .id();

    scenario.with_mana_pool(P0, colorless_mana(4));

    let runner = scenario.build();
    let encore_idx = encore_ability_index(runner.state(), legion_id);

    assert!(
        !can_activate_ability_now(runner.state(), P0, legion_id, encore_idx),
        "encore must be illegal when the player cannot pay the full mana value"
    );
}
