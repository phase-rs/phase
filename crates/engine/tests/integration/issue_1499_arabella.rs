//! Issue #1499 — Arabella, Abandoned Doll attack trigger must deal X damage to
//! each opponent and you gain X life, where X is creatures you control with
//! power 2 or less.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, FilterProp, PlayerFilter,
    PtStat, PtValueScope, QuantityExpr, QuantityRef, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::{run_combat, WaitingFor};

fn arabella_attack_trigger() -> TriggerDefinition {
    let x = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 2 },
                }],
            }),
        },
    };
    TriggerDefinition::new(TriggerMode::Attacks)
        .execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DamageEachPlayer {
                    amount: x.clone(),
                    player_filter: PlayerFilter::Opponent,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: x,
                    player: TargetFilter::Controller,
                },
            )),
        )
        .valid_card(TargetFilter::SelfRef)
}

#[test]
fn issue_1499_arabella_attack_deals_x_and_gains_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let arabella = scenario
        .add_creature(P0, "Arabella, Abandoned Doll", 1, 3)
        .with_trigger_definition(arabella_attack_trigger())
        .id();
    scenario.add_creature(P0, "Small A", 1, 1);
    scenario.add_creature(P0, "Small B", 1, 1);
    scenario.add_creature(P0, "Large", 3, 3);

    let mut runner = scenario.build();

    let p1_life_before = runner.life(P1);
    let p0_life_before = runner.life(P0);

    run_combat(&mut runner, vec![arabella], vec![]);
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "combat should finish at priority, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "DamageEachPlayer must deal 3 to each opponent"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before + 3,
        "Arabella's second sentence must gain the controller 3 life"
    );
}
