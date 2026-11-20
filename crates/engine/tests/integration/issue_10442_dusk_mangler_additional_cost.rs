//! CR 601.2b + CR 601.2h regression for issue #10442.
//!
//! Dusk Mangler's additional cost is a three-way choice — "sacrifice a creature,
//! discard a card, or pay 4 life". `AdditionalCost::Choice` carries the whole
//! printed list, every option is enumerated as a `DecideOptionalCost` index, and
//! the caster may pick any option they can actually pay. A caster with no
//! creatures and less than 4 life must still be able to discard a card.

use engine::game::life_safety::{preview_candidate_life_safety, CandidateLifeSafety};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, AdditionalCost, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

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

/// Adds a creature the caster could sacrifice, so every one of Dusk Mangler's
/// three alternatives is payable and the prompt is a real three-way election.
fn add_sacrifice_fodder(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_creature(P0, "Grizzly Bears", 2, 2).id()
}

/// Asserts the cast committed: Dusk Mangler is on the stack, priority is open,
/// and all seven Swamps paid its {5}{B}{B}.
fn assert_cast_committed(runner: &engine::game::scenario::GameRunner, mangler: ObjectId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast must complete, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&mangler].zone, Zone::Stack);
    let swamps: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].name == "Swamp")
        .collect();
    assert_eq!(swamps.len(), 7, "presence guard: the seven Swamps");
    assert!(
        swamps.iter().all(|id| runner.state().objects[id].tapped),
        "the printed {{5}}{{B}}{{B}} must be paid in full"
    );
}

/// CR 601.2b + CR 601.2h: index 0 sacrifices a creature and pays nothing else.
#[test]
fn sacrifice_option_is_the_first_index() {
    let (mut scenario, mangler, fodder) = dusk_mangler_board(8);
    let bear = add_sacrifice_fodder(&mut scenario);
    let mut runner = scenario.build();
    cast_mangler(&mut runner, mangler);

    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("sacrificing a creature must be a selectable option");
    if let WaitingFor::PayCost { kind, choices, .. } = &runner.state().waiting_for {
        assert!(
            matches!(kind, PayCostKind::Sacrifice),
            "index 0 must ask for a sacrifice, got {kind:?}"
        );
        assert_eq!(choices, &vec![bear], "only the creature can be sacrificed");
        runner
            .act(GameAction::SelectCards { cards: vec![bear] })
            .expect("sacrificing the creature must pay the cost");
    }

    assert_cast_committed(&runner, mangler);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "the sacrificed creature is the cost paid"
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Hand,
        "index 0 discards nothing"
    );
    assert_eq!(runner.state().players[0].life, 8, "index 0 pays no life");
}

/// CR 601.2b + CR 601.2h: index 1 discards a card through the prompt — the
/// option the two-way parse used to drop — and leaves the creature and life
/// untouched.
#[test]
fn discard_option_is_the_second_index() {
    let (mut scenario, mangler, fodder) = dusk_mangler_board(8);
    let bear = add_sacrifice_fodder(&mut scenario);
    let keeper = scenario.add_card_to_hand(P0, "Second Hand Card");
    let mut runner = scenario.build();
    cast_mangler(&mut runner, mangler);

    runner
        .act(GameAction::DecideOptionalCost { index: 1 })
        .expect("discarding a card must be a selectable option");
    let WaitingFor::PayCost { kind, choices, .. } = &runner.state().waiting_for else {
        panic!(
            "index 1 must ask which card to discard, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(kind, PayCostKind::Discard),
        "index 1 must ask for a discard, got {kind:?}"
    );
    assert!(
        choices.contains(&fodder) && choices.contains(&keeper),
        "both other cards in hand must be discardable, got {choices:?}"
    );
    assert!(
        !choices.contains(&mangler),
        "the spell being cast is not in hand to discard"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("discarding the chosen card must pay the cost");

    assert_cast_committed(&runner, mangler);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&keeper].zone,
        Zone::Hand,
        "only the chosen card is discarded"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "index 1 sacrifices nothing"
    );
    assert_eq!(runner.state().players[0].life, 8, "index 1 pays no life");
}

/// CR 118.11 + CR 119.4: with no creature and no other card in hand, paying 4
/// life is Dusk Mangler's only payable branch — and it is still prompted, so the
/// branch is committed by the caster's answer rather than automatically.
#[test]
fn sole_payable_life_option_is_still_prompted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..7 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    scenario.with_life(P0, 8);
    let mangler = scenario
        .add_creature_to_hand_from_oracle(P0, "Dusk Mangler", 5, 4, DUSK_MANGLER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 5,
        })
        .id();
    let mut runner = scenario.build();
    cast_mangler(&mut runner, mangler);
    assert_eq!(
        runner.state().players[0].life,
        8,
        "no life is paid before the caster answers"
    );

    runner
        .act(GameAction::DecideOptionalCost { index: 2 })
        .expect("paying 4 life is the payable option");
    assert_cast_committed(&runner, mangler);
    assert_eq!(runner.state().players[0].life, 4);
}

// --- Final Payment: a sole payable life branch and the lethal-life veto ---

/// Verbatim Oracle text (Scryfall, 2026-11-16).
const FINAL_PAYMENT_ORACLE: &str = "As an additional cost to cast this spell, pay 5 life or sacrifice a creature or enchantment.\nDestroy target creature.";
/// Verbatim Oracle text (Scryfall, 2026-11-16).
const BLOODLETTER_ORACLE: &str = "Flying\nIf an opponent would lose life during your turn, they lose twice that much life instead. (Damage causes loss of life.)";

/// Whose turn Final Payment is cast in. Bloodletter of Aclazotz (controlled by
/// P1) doubles P0's life loss only during P1's turn.
#[derive(Clone, Copy)]
enum Turn {
    Caster,
    BloodletterController,
}

/// P0 has a Plains, a Swamp and Final Payment in hand, and controls no creature
/// or enchantment, so paying 5 life is the only payable branch. P1 controls
/// Bloodletter of Aclazotz. Casts Final Payment and returns at its
/// additional-cost prompt.
fn cast_final_payment(
    life: i32,
    turn: Turn,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, life);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Black);
    let bloodletter = scenario
        .add_creature(P1, "Bloodletter of Aclazotz", 2, 4)
        .from_oracle_text_with_keywords(&["Flying"], BLOODLETTER_ORACLE)
        .id();
    let final_payment = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Payment", true, FINAL_PAYMENT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    if let Turn::BloodletterController = turn {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let card_id = runner.state().objects[&final_payment].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: final_payment,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announcing Final Payment must succeed");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bloodletter)],
            })
            .expect("Bloodletter is a legal target");
    }

    let WaitingFor::OptionalCostChoice {
        cost: AdditionalCost::Choice(costs),
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the sole payable life branch must be prompted, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(
            costs.as_slice(),
            [AbilityCost::PayLife { .. }, AbilityCost::Sacrifice(_)]
        ),
        "Final Payment's printed branches: pay 5 life or sacrifice, got {costs:?}"
    );
    assert_eq!(
        runner.state().players[0].life,
        life,
        "no life is paid before the caster answers"
    );
    (runner, final_payment, bloodletter)
}

/// The generated "pay 5 life" answer and its life-safety verdict.
fn pay_life_verdict(runner: &engine::game::scenario::GameRunner) -> CandidateLifeSafety {
    let candidate = engine::ai_support::candidate_actions(runner.state())
        .into_iter()
        .find(|candidate| candidate.action == GameAction::DecideOptionalCost { index: 0 })
        .expect("the prompt must generate the pay-life answer");
    preview_candidate_life_safety(runner.state(), &candidate)
}

/// CR 119.4 + CR 704.5a: paying 5 life at 5 life is legal but leaves the caster
/// at 0, so the veto judges the answer lethal.
#[test]
fn final_payment_nominally_lethal_life_payment_is_vetoed() {
    let (runner, _, _) = cast_final_payment(5, Turn::Caster);
    assert_eq!(
        pay_life_verdict(&runner),
        CandidateLifeSafety::Unsafe {
            before: 5,
            after: 0,
            committed: 5,
        }
    );
}

/// CR 118.11 + CR 614.1a + CR 704.5a: at 8 life the nominal 5 looks safe, but
/// during P1's turn Bloodletter of Aclazotz doubles the loss to 10. The branch
/// must still reach the prompt, and the veto judges the post-replacement loss.
#[test]
fn final_payment_replacement_amplified_life_payment_is_vetoed() {
    let (runner, _, _) = cast_final_payment(8, Turn::BloodletterController);
    assert_eq!(
        pay_life_verdict(&runner),
        CandidateLifeSafety::Unsafe {
            before: 8,
            after: -2,
            committed: 10,
        }
    );

    // Reach guard: the replacement really doubles the payment.
    let mut paid = runner;
    paid.act(GameAction::DecideOptionalCost { index: 0 })
        .expect("paying 5 life is legal at 8 life");
    assert_eq!(paid.state().players[0].life, -2);
}

/// Control: at 8 life during the caster's own turn Bloodletter does not apply,
/// so paying 5 life is not lethal, stays available, and completes the cast.
#[test]
fn final_payment_nonlethal_life_payment_stays_available() {
    let (mut runner, final_payment, bloodletter) = cast_final_payment(8, Turn::Caster);
    assert_eq!(pay_life_verdict(&runner), CandidateLifeSafety::NotUnsafe);

    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("paying 5 life is the payable branch");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bloodletter)],
            })
            .expect("Bloodletter is a legal target");
    }
    assert_eq!(runner.state().players[0].life, 3, "exactly 5 life is paid");
    assert_eq!(runner.state().objects[&final_payment].zone, Zone::Stack);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast must complete, got {:?}",
        runner.state().waiting_for
    );
}
