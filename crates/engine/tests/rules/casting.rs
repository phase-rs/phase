#![allow(unused_imports)]
use super::*;

use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, DamageAmount, Effect,
    TargetFilter, TargetRef,
};
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;

/// Helper: advance past TargetSelection if present, return the resulting WaitingFor.
fn handle_target_selection(runner: &mut engine::game::scenario::GameRunner, result: &ActionResult) {
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target selection should succeed");
    }
}

/// Extract `additional_cost_paid` from the top stack entry (assumes it's a Spell).
fn top_stack_cost_paid(runner: &engine::game::scenario::GameRunner) -> bool {
    let entry = runner
        .state()
        .stack
        .last()
        .expect("stack should not be empty");
    match &entry.kind {
        StackEntryKind::Spell { ability, .. } => ability.context.additional_cost_paid,
        other => panic!("expected Spell on stack, got {:?}", other),
    }
}

/// Cast a spell with an Optional additional cost, choose to pay.
/// Verifies the casting pipeline enters OptionalCostChoice and
/// sets additional_cost_paid = true on the stack entry when paid.
#[test]
fn optional_cost_paid_sets_flag() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: DamageAmount::Fixed(3),
            target: TargetFilter::Any,
        })
        .with_additional_cost(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // Should now be at OptionalCostChoice
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "expected OptionalCostChoice, got {:?}",
        runner.state().waiting_for,
    );

    // Pay the additional cost
    let result3 = runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("decide optional cost should succeed");

    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after paying, got {:?}",
        result3.waiting_for,
    );

    assert!(
        top_stack_cost_paid(&runner),
        "additional_cost_paid should be true when cost is paid"
    );
}

/// Cast a spell with an Optional additional cost, choose to skip.
/// Verifies additional_cost_paid = false on the stack entry.
#[test]
fn optional_cost_skipped_clears_flag() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: DamageAmount::Fixed(3),
            target: TargetFilter::Any,
        })
        .with_additional_cost(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // Skip the additional cost
    let result3 = runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("skip optional cost should succeed");

    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after skipping, got {:?}",
        result3.waiting_for,
    );

    assert!(
        !top_stack_cost_paid(&runner),
        "additional_cost_paid should be false when cost is skipped"
    );
}

/// Cast a spell without an additional cost -- should skip OptionalCostChoice entirely.
#[test]
fn no_additional_cost_skips_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);

    let spell_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // Should go to target selection or directly to priority -- never OptionalCostChoice
    assert!(
        !matches!(result.waiting_for, WaitingFor::OptionalCostChoice { .. }),
        "should not enter OptionalCostChoice for spells without additional costs"
    );
}

/// Cancel cast while at OptionalCostChoice returns the spell to hand.
#[test]
fn cancel_cast_at_optional_cost_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: DamageAmount::Fixed(3),
            target: TargetFilter::Any,
        })
        .with_additional_cost(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // Cancel the cast
    let result3 = runner
        .act(GameAction::CancelCast)
        .expect("cancel should succeed");

    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after cancel, got {:?}",
        result3.waiting_for,
    );

    assert!(
        runner.state().stack.is_empty(),
        "stack should be empty after cancel"
    );
    assert_eq!(
        runner.state().objects[&spell_id].zone,
        Zone::Hand,
        "spell should return to hand after cancel"
    );
}
