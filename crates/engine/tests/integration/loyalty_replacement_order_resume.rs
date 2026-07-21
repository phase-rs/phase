//! CR 606.4 + CR 614.1a + CR 616.1: A loyalty activation whose loyalty-counter
//! cost is modified by TWO simultaneously-applicable counter replacements must
//! pause on the CR 616.1 ordering choice, then RESUME the activation once the
//! choice is settled — pushing the ability onto the stack exactly once, without
//! re-paying the loyalty cost.
//!
//! This is the discriminating test for the pause/resume plumbing (the new
//! `PendingCostMoveResume::LoyaltyActivation` continuation + the
//! `finalize_loyalty_activation` → `complete_loyalty_activation` split). If the
//! resume never fired, `waiting_for` would stay stuck on `ReplacementChoice` (or
//! the ability would never reach the stack); if it fired twice, the activation
//! would be double-counted.
//!
//! Order is material (CR 616.1): with a +3 activation, double-then-half =
//! floor(3*2/2) = 3, but half-then-double = floor(3/2)*2 = 2. The two orders
//! diverge (8 vs 7 final loyalty), proving the chosen order is honored.
//!
//! Both replacements are constructed test-locally (parser fix is inert until
//! card-data regen).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CounterReplacementSubject, Effect, QuantityExpr,
    QuantityModification, ReplacementDefinition, ReplacementPlayerScope, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

const START_LOYALTY: u32 = 5;

/// Build the scenario, activate the `+3` loyalty ability, and stop at the
/// CR 616.1 ordering prompt. Returns the runner, the planeswalker id, and the
/// candidate index of the doubling replacement (its source is the planeswalker
/// itself, via the SelfRef attachment).
fn activate_to_ordering_prompt() -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's planeswalker with a `+3` loyalty ability AND a self-scoped doubler
    // (Doubling-Season-class: valid_card SelfRef, default Recipient subject).
    let pw = scenario
        .add_creature(P0, "Order Walker", 0, 0)
        .as_planeswalker_with_loyalty("Test", START_LOYALTY)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Loyalty { amount: 3 })
            .sorcery_speed(),
        )
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::SelfRef)
                .quantity_modification(QuantityModification::DOUBLE)
                .description("double".to_string()),
        )
        .id();

    // P1 (opponent) controls Vorinclex halving (actor-scoped Opponent).
    let mut halving = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .valid_card(TargetFilter::Any)
        .quantity_modification(QuantityModification::Half)
        .counter_subject(CounterReplacementSubject::Actor)
        .description("halve".to_string());
    halving.valid_player = Some(ReplacementPlayerScope::Opponent);
    scenario
        .add_creature(P1, "Vorinclex, Monstrous Raider", 6, 6)
        .with_replacement_definition(halving);

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: pw,
            ability_index: 0,
        })
        .expect("activate the +3 loyalty ability");

    // Reach-guard (non-vacuous): the CR 616.1 ordering prompt must surface for
    // P0, with exactly the two competing replacements.
    let doubler_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            candidates,
        } => {
            assert_eq!(*player, P0, "the affected permanent's controller chooses");
            assert_eq!(*candidate_count, 2, "doubler + halver compete");
            candidates
                .iter()
                .position(|c| c.source_id == pw)
                .expect("the doubler candidate is sourced by the planeswalker (SelfRef)")
        }
        other => panic!("expected a CR 616.1 ordering prompt, got {other:?}"),
    };

    (runner, pw, doubler_index)
}

fn assert_activation_completed(runner: &GameRunner, pw: ObjectId) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resume must return to Priority once the ordering choice settles: {:?}",
        state.waiting_for
    );
    assert!(
        matches!(
            state.stack.back().map(|e| &e.kind),
            Some(StackEntryKind::ActivatedAbility { .. })
        ),
        "the loyalty ability must be pushed onto the stack by the resumed tail"
    );
    assert_eq!(
        state.objects[&pw].loyalty_activations_this_turn, 1,
        "the activation tail must run exactly once (not zero, not twice)"
    );
    assert_eq!(
        state
            .loyalty_abilities_activated_this_turn
            .get(&P0)
            .copied(),
        Some(1),
        "the per-player activation counter must be recorded exactly once"
    );
}

/// Choosing the doubler first: double-then-half = floor(3*2/2) = 3 → 5 + 3 = 8.
#[test]
fn double_then_half_resumes_with_plus_three() {
    let (mut runner, pw, doubler_index) = activate_to_ordering_prompt();

    runner
        .act(GameAction::ChooseReplacement {
            index: doubler_index,
        })
        .expect("apply the doubler first");

    assert_activation_completed(&runner, pw);
    assert_eq!(
        runner.state().objects[&pw].loyalty,
        Some(START_LOYALTY + 3),
        "double-then-half must net +3 loyalty (5 → 8)"
    );
}

/// Choosing the halver first: half-then-double = floor(3/2)*2 = 2 → 5 + 2 = 7.
/// Diverges from the double-first order (8), proving the choice is material.
#[test]
fn half_then_double_resumes_with_plus_two() {
    let (mut runner, pw, doubler_index) = activate_to_ordering_prompt();
    let halver_index = 1 - doubler_index;

    runner
        .act(GameAction::ChooseReplacement {
            index: halver_index,
        })
        .expect("apply the halver first");

    assert_activation_completed(&runner, pw);
    assert_eq!(
        runner.state().objects[&pw].loyalty,
        Some(START_LOYALTY + 2),
        "half-then-double must net +2 loyalty (5 → 7)"
    );
}
