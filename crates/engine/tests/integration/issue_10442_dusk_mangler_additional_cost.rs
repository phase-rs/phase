//! CR 601.2b + CR 601.2h regression for issue #10442.
//!
//! Dusk Mangler's additional cost is a three-way choice — "sacrifice a creature,
//! discard a card, or pay 4 life". `AdditionalCost::Choice` carries the whole
//! printed list, every option is enumerated as a `DecideOptionalCost` index, and
//! the caster may pick any option they can actually pay. A caster with no
//! creatures and less than 4 life must still be able to discard a card.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

/// Verbatim Oracle text (Scryfall, 2026-11-15).
const DUSK_MANGLER_ORACLE: &str = "As an additional cost to cast this spell, sacrifice a creature, discard a card, or pay 4 life.\nWhen this creature enters, each opponent sacrifices a creature of their choice, discards a card, and loses 4 life.";

/// Builds the issue's board: seven Swamps, Dusk Mangler plus one discard fodder
/// in hand, no creatures on the battlefield, and the requested life total.
fn dusk_mangler_board(life: i32) -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..7 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    scenario.with_life(P0, life);
    let fodder = scenario.add_card_to_hand(P0, "Discard Fodder");
    let mangler = scenario
        .add_creature_to_hand_from_oracle(P0, "Dusk Mangler", 5, 4, DUSK_MANGLER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 5,
        })
        .id();
    (scenario, mangler, fodder)
}

/// Asserts the prompt is Dusk Mangler's three-way list in printed order and
/// returns `true` when the prompt was reached.
fn assert_three_way_choice(waiting_for: &WaitingFor) {
    let WaitingFor::OptionalCostChoice {
        cost: AdditionalCost::Choice(costs),
        ..
    } = waiting_for
    else {
        panic!("expected Dusk Mangler's three-way additional-cost prompt, got {waiting_for:?}");
    };
    assert_eq!(
        costs.len(),
        3,
        "the discard option must not be dropped: {costs:?}"
    );
    assert!(
        matches!(costs[0], AbilityCost::Sacrifice(_)),
        "option 0 is the printed sacrifice, got {:?}",
        costs[0]
    );
    assert!(
        matches!(costs[1], AbilityCost::Discard { .. }),
        "option 1 is the printed discard, got {:?}",
        costs[1]
    );
    assert!(
        matches!(costs[2], AbilityCost::PayLife { .. }),
        "option 2 is the printed pay 4 life, got {:?}",
        costs[2]
    );
}

fn cast_mangler(runner: &mut engine::game::scenario::GameRunner, mangler: ObjectId) {
    let card_id = runner.state().objects[&mangler].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: mangler,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announcing Dusk Mangler must reach its additional-cost prompt");
    assert_three_way_choice(&runner.state().waiting_for);
}

/// CR 601.2h: the discard branch is a real, payable option even with no
/// creatures and less than 4 life — the exact shape that used to be lost.
#[test]
fn discard_option_casts_mangler_without_creatures_or_enough_life() {
    let (scenario, mangler, fodder) = dusk_mangler_board(3);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&mangler].card_id;
    // CR 601.2b + CR 601.2h: with no creature to sacrifice and less than 4 life,
    // discarding is the only payable alternative, so the engine applies it
    // without asking — and the cast proceeds instead of failing.
    runner
        .act(GameAction::CastSpell {
            object_id: mangler,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announcing Dusk Mangler must reach its payable alternative");

    let WaitingFor::PayCost { kind, choices, .. } = &runner.state().waiting_for else {
        panic!(
            "selecting the discard branch must prompt for the card to discard, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(kind, PayCostKind::Discard),
        "the discard branch must ask for a discard, got {kind:?}"
    );
    assert!(
        choices.contains(&fodder),
        "the discard fodder must be selectable, got {choices:?}"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("paying the discard cost must complete the declaration");

    assert_eq!(
        runner.state().players[0].life,
        3,
        "the discard branch pays no life"
    );
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == mangler),
        "paying the discard branch must put Dusk Mangler on the stack, got {:?}",
        runner.state().waiting_for
    );
}

/// The same board with enough life can pay the printed life option instead.
#[test]
fn pay_life_option_is_the_third_index() {
    let (scenario, mangler, _fodder) = dusk_mangler_board(8);
    let mut runner = scenario.build();
    cast_mangler(&mut runner, mangler);

    runner
        .act(GameAction::DecideOptionalCost { index: 2 })
        .expect("paying 4 life must be a selectable option");

    assert_eq!(
        runner.state().players[0].life,
        4,
        "the third option pays exactly 4 life"
    );
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == mangler),
        "paying the life branch must put Dusk Mangler on the stack, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 601.2h + CR 601.2b: a client-supplied index outside the prompt's option
/// list is a typed refusal, never an out-of-bounds panic, and leaves the prompt
/// untouched so the caster can answer it correctly afterwards.
#[test]
fn out_of_range_index_is_rejected_without_mutating_the_prompt() {
    let (scenario, mangler, _fodder) = dusk_mangler_board(8);
    let mut runner = scenario.build();
    cast_mangler(&mut runner, mangler);

    let rejected = runner.act(GameAction::DecideOptionalCost { index: 9 });
    assert!(
        rejected.is_err(),
        "an index outside the option list must be rejected"
    );
    assert_three_way_choice(&runner.state().waiting_for);

    // The prompt is still answerable.
    runner
        .act(GameAction::DecideOptionalCost { index: 2 })
        .expect("the prompt must remain answerable after a rejected index");
}
