//! The "that many" count a resolution stamps is scoped to that one stack-object
//! resolution (CR 608.2c + CR 608.2h), not to the player action that happens to
//! resolve it.
//!
//! One `apply()` can resolve several stack objects in a row (passing priority
//! with auto-pass, or two players passing over a deep stack). A count stamped by
//! the first resolution (Quick Study's or The Arkenstone's draw) must not be read
//! as "damage prevented this way" by a later, unrelated resolution in the same
//! action — neither when Sacred Boon's spell creates its delayed trigger, nor when
//! that delayed trigger resolves at the end step.

use crate::reveal_until_that_many::pass_to_delayed_trigger;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{AutoPassRequest, PendingTriggerSummary, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SACRED_BOON: &str = "Prevent the next 3 damage that would be dealt to target creature this turn. At the beginning of the next end step, put a +0/+1 counter on that creature for each 1 damage prevented this way.";

/// Quick Study — its draw stamps a count of 2 when it resolves.
const QUICK_STUDY: &str = "Draw two cards.";

/// The Arkenstone — its end-step draw stamps a count of 1 when it resolves.
const THE_ARKENSTONE: &str =
    "Creatures you control get +1/+1.\nAt the beginning of your end step, draw a card.";

const PLUS_ZERO_PLUS_ONE: CounterType = CounterType::PowerToughness {
    power: 0,
    toughness: 1,
};

fn plus_zero_plus_one_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&PLUS_ZERO_PLUS_ONE)
        .copied()
        .unwrap_or(0)
}

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

fn stock_library(scenario: &mut GameScenario) {
    for index in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {index}"));
    }
}

fn add_sacred_boon(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Sacred Boon", true, SACRED_BOON)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn assert_stack_empty_priority(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "every spell and trigger must have resolved; waiting_for={}, stack={:?}",
        runner.waiting_for_kind(),
        runner.stack_names()
    );
}

/// The installed delayed trigger's "for each 1 damage prevented this way" count.
fn delayed_counter_count(runner: &GameRunner) -> QuantityExpr {
    let delayed = &runner.state().delayed_triggers;
    assert_eq!(
        delayed.len(),
        1,
        "reach guard: Sacred Boon installs exactly one delayed trigger"
    );
    let Effect::PutCounter { count, .. } = &delayed[0].ability.effect else {
        panic!(
            "Sacred Boon's delayed payload must put counters, got {:?}",
            delayed[0].ability.effect
        );
    };
    count.clone()
}

/// Advance to the end step and return the end-step triggers P0 must order.
fn advance_to_end_step_trigger_order(runner: &mut GameRunner) -> Vec<PendingTriggerSummary> {
    runner.advance_to_end_step();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => return triggers,
            // CR 508.1: no attack is declared on the way to the end step.
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the end step");
            }
            other => panic!("unexpected state while advancing to the end step: {other:?}"),
        }
    }
    panic!("the end-step triggers did not surface");
}

fn resolve_stack_in_one_action(runner: &mut GameRunner) {
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("resolve the whole stack in one player action");
    assert_stack_empty_priority(runner);
}

/// CR 608.2c + CR 608.2h + CR 603.7a: Quick Study sits ABOVE Sacred Boon, so one
/// player action resolves Quick Study first (stamping its draw count, 2) and then
/// Sacred Boon. Sacred Boon's resolution starts with no count of its own: no
/// damage has been prevented, so its delayed trigger must not freeze Quick
/// Study's 2 as "damage prevented this way". At the end step the creature gets
/// no +0/+1 counter.
#[test]
fn sacred_boon_does_not_freeze_a_count_stamped_by_an_earlier_resolution_in_the_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Boon Recipient", 2, 2).id();
    stock_library(&mut scenario);
    let boon = add_sacred_boon(&mut scenario);
    let quick_study = scenario
        .add_spell_to_hand_from_oracle(P0, "Quick Study", true, QUICK_STUDY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(boon).target_object(creature).commit();
    runner.cast(quick_study).commit();
    assert_eq!(
        runner.stack_names(),
        vec!["Sacred Boon".to_string(), "Quick Study".to_string()],
        "Quick Study must resolve first, above Sacred Boon"
    );
    let hand_before = hand_len(&runner);

    resolve_stack_in_one_action(&mut runner);

    // Reach guards: Quick Study's draw resolved in this action (stamping 2),
    // and Sacred Boon resolved after it, installing its delayed trigger.
    assert_eq!(
        hand_len(&runner),
        hand_before + 2,
        "Quick Study drew two cards in the same action"
    );
    assert_eq!(runner.state().objects[&boon].zone, Zone::Graveyard);
    // Focused check: Sacred Boon's resolution began with no stamped count, so
    // nothing was frozen — the amount stays the live "damage prevented" read.
    assert_eq!(
        delayed_counter_count(&runner),
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        "Quick Study's draw count must not be frozen into Sacred Boon's delayed trigger"
    );

    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);
    assert_eq!(runner.state().phase, Phase::End);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach guard: the delayed trigger fired"
    );

    assert_eq!(
        plus_zero_plus_one_counters(&runner, creature),
        0,
        "no damage was prevented, so the creature gets no +0/+1 counter"
    );
}

/// CR 608.2c + CR 608.2h + CR 603.7a: Sacred Boon resolves alone (no damage
/// prevented). At the end step The Arkenstone's draw trigger resolves first in
/// the SAME player action and stamps its own count (1); Sacred Boon's delayed
/// trigger resolves next and must not read that count as "damage prevented this
/// way".
#[test]
fn sacred_boon_end_step_trigger_does_not_read_a_count_stamped_earlier_in_the_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Boon Recipient", 2, 2).id();
    let arkenstone = scenario
        .add_artifact_from_oracle(P0, "The Arkenstone", THE_ARKENSTONE)
        .id();
    stock_library(&mut scenario);
    let boon = add_sacred_boon(&mut scenario);
    let mut runner = scenario.build();

    runner.cast(boon).target_object(creature).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        delayed_counter_count(&runner),
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        "reach guard: the delayed trigger reads the prevented amount live"
    );

    let triggers = advance_to_end_step_trigger_order(&mut runner);
    assert_eq!(
        triggers.len(),
        2,
        "Sacred Boon's delayed trigger and The Arkenstone"
    );
    // Index 0 is placed first (bottom): put the delayed trigger under The
    // Arkenstone's draw so the draw resolves first.
    let arkenstone_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == arkenstone)
        .expect("The Arkenstone's end-step trigger is pending");
    let mut order: Vec<usize> = (0..triggers.len())
        .filter(|&index| index != arkenstone_index)
        .collect();
    order.push(arkenstone_index);
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("order the end-step triggers");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "both triggers are on the stack"
    );
    let hand_before = hand_len(&runner);

    resolve_stack_in_one_action(&mut runner);
    assert_eq!(runner.state().phase, Phase::End);

    // Reach guards: The Arkenstone's draw resolved first in this action, and the
    // delayed trigger fired after it.
    assert_eq!(
        hand_len(&runner),
        hand_before + 1,
        "The Arkenstone's trigger drew a card"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach guard: the delayed trigger fired"
    );

    assert_eq!(
        plus_zero_plus_one_counters(&runner, creature),
        0,
        "no damage was prevented, so the creature gets no +0/+1 counter"
    );
}
