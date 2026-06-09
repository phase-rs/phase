//! Regression for issue #2415: Rottenmouth Viper must offer its optional
//! sacrifice additional cost and reduce the spell's mana cost by {1} per
//! permanent sacrificed.
//!
//! https://github.com/phase-rs/phase/issues/2415

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AdditionalCost, AdditionalCostRepeatability, StaticCondition,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};

const ROTTENMOUTH_ORACLE: &str = concat!(
    "As an additional cost to cast this spell, you may sacrifice any number of nonland permanents. ",
    "This spell costs {1} less to cast for each permanent sacrificed this way.\n",
    "Whenever this creature enters or attacks, put a blight counter on it. ",
    "Then for each blight counter on it, each opponent loses 4 life unless that player ",
    "sacrifices a nonland permanent of their choice or discards a card."
);

fn build_scenario() -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
    Vec<engine::types::identifiers::ObjectId>,
) {
    let parsed = parse_oracle_text(
        ROTTENMOUTH_ORACLE,
        "Rottenmouth Viper",
        &[],
        &["Creature".to_string()],
        &[],
    );

    let additional_cost = parsed
        .additional_cost
        .clone()
        .expect("Rottenmouth must parse an additional cost");
    let reduce_static = parsed
        .statics
        .iter()
        .find(|s| {
            matches!(
                s.mode,
                StaticMode::ModifyCost {
                    mode: CostModifyMode::Reduce,
                    ..
                }
            )
        })
        .cloned()
        .expect("Rottenmouth must parse a self-spell cost-reduction static");
    assert_eq!(
        reduce_static.condition,
        Some(StaticCondition::AdditionalCostPaid)
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Six lands — enough to pay {5}{B} at full cost or {3}{B} after sacrificing two.
    let lands: Vec<_> = (0..6)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();

    let viper = scenario
        .add_creature_to_hand(P0, "Rottenmouth Viper", 5, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 5,
        })
        .with_additional_cost(additional_cost)
        .with_static_definition(reduce_static)
        .id();

    // Two sac fodder nonland permanents.
    let fodder: Vec<_> = (0..2)
        .map(|i| scenario.add_creature(P0, &format!("Fodder {i}"), 1, 1).id())
        .collect();

    (scenario.build(), viper, lands, fodder)
}

use engine::types::mana::ManaCostShard;

fn tapped_land_count(
    runner: &engine::game::scenario::GameRunner,
    lands: &[engine::types::identifiers::ObjectId],
) -> usize {
    lands
        .iter()
        .filter(|id| runner.state().objects[id].tapped)
        .count()
}

#[test]
fn rottenmouth_viper_parses_sacrifice_additional_cost_and_reduction() {
    let parsed = parse_oracle_text(
        ROTTENMOUTH_ORACLE,
        "Rottenmouth Viper",
        &[],
        &["Creature".to_string()],
        &[],
    );
    match parsed.additional_cost {
        Some(AdditionalCost::Optional {
            cost: AbilityCost::Sacrifice {
                count: u32::MAX, ..
            },
            repeatability: AdditionalCostRepeatability::Once,
        }) => {}
        other => panic!("expected optional any-number sacrifice cost, got {other:?}"),
    }
    assert!(
        parsed.statics.iter().any(|s| {
            matches!(
                s.mode,
                StaticMode::ModifyCost {
                    mode: CostModifyMode::Reduce,
                    amount: ManaCost::Cost { generic: 1, .. },
                    dynamic_count: Some(
                        engine::types::ability::QuantityRef::TrackedSetSize
                            | engine::types::ability::QuantityRef::FilteredTrackedSetSize { .. }
                    ),
                    ..
                }
            )
        }),
        "expected sacrificed-this-way cost reduction static, got statics: {:?}",
        parsed.statics
    );
}

#[test]
fn rottenmouth_viper_sacrifice_two_reduces_cost_by_two() {
    let (mut runner, viper, lands, fodder) = build_scenario();
    let card_id = runner.state().objects[&viper].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: viper,
            card_id,
            targets: vec![],
        })
        .expect("begin Rottenmouth Viper cast");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "expected optional sacrifice prompt, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept optional sacrifice cost");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { max: 2, .. }
        ),
        "expected ChooseXValue for sacrifice count, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("choose to sacrifice two permanents");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "expected sacrifice PayCost prompt, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: fodder.clone(),
        })
        .expect("sacrifice two nonland permanents");

    // {5}{B} minus {2} → {3}{B}: three swamps for generic plus one for {B}.
    assert_eq!(
        tapped_land_count(&runner, &lands),
        4,
        "sacrificing two permanents should reduce the cast to {{3}}{{B}} (4 swamps total)"
    );
}
