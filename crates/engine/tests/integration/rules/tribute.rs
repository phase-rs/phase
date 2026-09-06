//! Integration tests for the Tribute mechanic (CR 702.104).
//!
//! Covers:
//! - Chosen opponent paying tribute → source enters with N +1/+1 counters,
//!   `ChosenAttribute::TributeOutcome::Paid` persisted, "if tribute wasn't paid"
//!   trigger suppressed (CR 702.104a + CR 702.104b).
//! - Chosen opponent declining → no counters, `TributeOutcome::Declined` persisted,
//!   trigger fires (CR 702.104b).
//! - The controller first chooses the opponent (`NamedChoice` with `ChoiceType::Opponent`).

#![allow(unused_imports)]
use super::*;

use engine::types::ability::{ChoiceType, ChosenAttribute, TributeOutcome};
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;

/// Fanatic of Xenagos-class Oracle: Tribute 1 + "When this creature enters, if
/// tribute wasn't paid, it gets +1/+1 and gains haste until end of turn."
///
/// We drive the ETB sequence through a cast to observe the full replacement chain.
fn cast_tribute_creature(count: u32, paid: bool) -> GameRunner {
    let oracle = format!(
        "Tribute {count} (As this creature enters, an opponent of your choice may put {count} +1/+1 counters on it.)\n\
         When this creature enters, if tribute wasn't paid, this creature deals 2 damage to each opponent."
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Give P0 enough mana to cast and add the Tribute creature to hand.
    let mut hand_builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Tribute Tester", 2, 2, &oracle);
    let card_obj_id = hand_builder.id();
    hand_builder.with_mana_cost(engine::types::mana::ManaCost::generic(0));

    let mut runner = scenario.build();

    // Cast the Tribute creature.
    let card_id = runner.state().objects[&card_obj_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card_obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should succeed");

    // Pass priority so the spell resolves and the ETB replacement fires.
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.pass_both_players();
    }

    // Expect: the Choose-opponent prompt fires first (controller picks an opponent).
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(*player, P0, "controller should be choosing the opponent");
            assert_eq!(*choice_type, ChoiceType::opponent());
            assert!(
                options.contains(&P1.0.to_string()),
                "P1 must be a valid opponent choice, got {options:?}"
            );
        }
        other => panic!("expected NamedChoice (Opponent), got {other:?}"),
    }

    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose opponent should succeed");

    // Now the chosen opponent (P1) is prompted pay/decline.
    match &runner.state().waiting_for {
        WaitingFor::TributeChoice {
            player,
            count: prompt_count,
            ..
        } => {
            assert_eq!(*player, P1, "chosen opponent should be prompted");
            assert_eq!(*prompt_count, count);
        }
        other => panic!("expected TributeChoice, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: paid })
        .expect("tribute decision should succeed");

    // Drain any remaining stack work (ETB trigger, counter addition, etc.).
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.pass_both_players();
    }

    runner
}

/// Return the ObjectId of the just-entered Tribute creature on the battlefield.
fn find_tribute_creature(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .map(|obj| obj.name == "Tribute Tester")
                .unwrap_or(false)
        })
        .expect("Tribute Tester should be on the battlefield")
}

/// CR 702.104a: When the chosen opponent pays tribute, the creature enters with
/// N +1/+1 counters and the paid outcome is recorded.
#[test]
fn tribute_paid_applies_counters_and_records_outcome() {
    let runner = cast_tribute_creature(/* count */ 2, /* paid */ true);
    let id = find_tribute_creature(&runner);
    let obj = &runner.state().objects[&id];

    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "paid tribute should add +1/+1 counters equal to Tribute N"
    );
    assert!(
        obj.chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::TributeOutcome(TributeOutcome::Paid))),
        "paid tribute should persist TributeOutcome::Paid"
    );
}

/// CR 702.104b: When the chosen opponent declines, no counters are added and the
/// declined outcome is persisted so the "if tribute wasn't paid" trigger can fire.
#[test]
fn tribute_declined_records_outcome_without_counters() {
    let runner = cast_tribute_creature(/* count */ 2, /* paid */ false);
    let id = find_tribute_creature(&runner);
    let obj = &runner.state().objects[&id];

    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "declined tribute should not add counters"
    );
    assert!(
        obj.chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::TributeOutcome(TributeOutcome::Declined))),
        "declined tribute should persist TributeOutcome::Declined"
    );
}

/// CR 702.104a: The controller is the one who selects the chosen opponent — not
/// the spell's opponent. Verified by the initial NamedChoice prompt's player.
#[test]
fn tribute_controller_picks_chosen_opponent() {
    let runner = cast_tribute_creature(/* count */ 1, /* paid */ true);
    let id = find_tribute_creature(&runner);
    let obj = &runner.state().objects[&id];

    assert!(
        obj.chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::Player(p) if *p == P1)),
        "controller's opponent choice should be persisted on the source"
    );
}

/// CR 702.104b: Verify the outcome distinction between paid and declined is
/// fully observable through `ChosenAttribute::TributeOutcome` — the typed
/// discriminator the `TributeNotPaid` trigger condition evaluator reads from.
#[test]
fn tribute_outcome_persists_distinctly_for_paid_vs_declined() {
    let paid_runner = cast_tribute_creature(1, /* paid */ true);
    let paid_id = find_tribute_creature(&paid_runner);
    let paid_obj = &paid_runner.state().objects[&paid_id];

    let declined_runner = cast_tribute_creature(1, /* paid */ false);
    let declined_id = find_tribute_creature(&declined_runner);
    let declined_obj = &declined_runner.state().objects[&declined_id];

    let paid_outcome = paid_obj.chosen_attributes.iter().find_map(|a| match a {
        ChosenAttribute::TributeOutcome(o) => Some(*o),
        _ => None,
    });
    let declined_outcome = declined_obj.chosen_attributes.iter().find_map(|a| match a {
        ChosenAttribute::TributeOutcome(o) => Some(*o),
        _ => None,
    });

    assert_eq!(paid_outcome, Some(TributeOutcome::Paid));
    assert_eq!(declined_outcome, Some(TributeOutcome::Declined));
}

// ───────────────────── copy tokens of "as enters" permanents ─────────────────────
//
// CR 614.12a + CR 111.1 + CR 702.104a. A token that is a copy of a permanent with
// an "as this enters" replacement takes the LIMINAL copy seam
// (`token_copy::apply_copy_token_after_replacement_with_created_ids`): the entrant
// is reserved in `state.liminal_entries` and is deliberately absent from
// `state.objects` while its own entry chain runs.
//
// Two defects met there, and each row below fails on its own mutant:
//
//   1. `GameState::pending_liminal_entry_resume` was consumed ONLY by the
//      `CopyTargetChoice` answer handler, so an entry that paused on any other
//      prompt was never resumed and the token was never created at all. The
//      "token exists" rows discriminate this.
//   2. `Effect::Choose { persist: true }` binds its answer through
//      `NamedChoiceSource`, whose read (`named_choice_authority`) and write
//      (`source_mut_exact_for_resolution`) both looked the source up in
//      `state.objects` alone, so a liminal entrant persisted nothing. The
//      "chosen attribute / counters" rows discriminate this.
//
// CONTROL (`copy_token_of_vanilla_creature_enters`): the identical seam with no
// as-enters replacement on the copied creature. It must stay green under both
// mutants — a red control would mean these rows are measuring the copy seam
// itself rather than the as-enters chain.

/// The Fanatic-of-Xenagos-class Oracle, as a class fixture rather than one card.
const TRIBUTE_ORACLE: &str = "Tribute 2 (As this creature enters, an opponent of your choice may put 2 +1/+1 counters on it.)";
/// The Painter's-Servant-class "as enters, choose a named attribute" Oracle.
const CHOOSE_COLOR_ORACLE: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";
/// Kiki-Jiki-class copy line — the shared entry into every copy-token effect.
const COPY_TOKEN_ORACLE: &str = "Create a token that's a copy of target creature you control.";

/// Stage `source_oracle` on P0's battlefield and cast a copy-token sorcery at it,
/// stopping at the first prompt the entry chain raises.
fn copy_token_of(source_oracle: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Entry Tester", 2, 2, source_oracle)
        .id();
    let sorcery = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Copier", false, COPY_TOKEN_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.cast(sorcery).target_object(source).commit();
    drive_to_prompt(&mut runner);
    (runner, source)
}

/// Pass priority until the stack empties or a non-priority prompt appears.
fn drive_to_prompt(runner: &mut GameRunner) {
    for _ in 0..16 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            return;
        }
        runner.pass_both_players();
    }
}

/// The single copy token on the battlefield, or `None` when none was created.
///
/// The discriminating observable for defect 1: a stranded entry leaves
/// `state.liminal_entries` non-empty and produces no battlefield object at all.
fn copy_token(runner: &GameRunner) -> Option<ObjectId> {
    let state = runner.state();
    let tokens: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
        .copied()
        .collect();
    assert!(
        tokens.len() <= 1,
        "these fixtures create at most one copy token, found {tokens:?}"
    );
    tokens.first().copied()
}

/// CONTROL: the liminal copy seam itself, with no as-enters replacement in play.
/// Green under both mutants — it is what proves the rows below measure the
/// as-enters chain and not the copy seam.
#[test]
fn copy_token_of_vanilla_creature_enters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let source = scenario.add_creature(P0, "Bear", 2, 2).id();
    let sorcery = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Copier", false, COPY_TOKEN_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.cast(sorcery).target_object(source).commit();
    drive_to_prompt(&mut runner);

    assert!(
        copy_token(&runner).is_some(),
        "the liminal copy seam must create a token for a creature with no as-enters replacement"
    );
    assert!(
        runner.state().liminal_entries.is_empty(),
        "a committed entry must leave no liminal record"
    );
}

/// CR 702.104a: a token copy of a Tribute creature must run the full Tribute
/// chain — the controller chooses an opponent, that opponent is prompted
/// pay-or-decline — and the token must actually enter the battlefield.
#[test]
fn copy_token_of_tribute_creature_prompts_chosen_opponent_and_enters() {
    let (mut runner, _) = copy_token_of(TRIBUTE_ORACLE);

    // Stage 1 (CR 702.104a): the token's controller chooses the opponent.
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            ..
        } => {
            assert_eq!(*player, P0, "the copy's controller chooses the opponent");
            assert_eq!(*choice_type, engine::types::ability::ChoiceType::opponent());
        }
        other => panic!("expected the Tribute opponent choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose opponent");

    // Stage 2 (CR 702.104a): the CHOSEN opponent decides pay-or-decline. Before
    // the persist fix the answer above bound to nothing, `tribute::resolve` read
    // no `ChosenAttribute::Player`, and this prompt was silently skipped in
    // favour of the documented `TributeOutcome::Declined` fallback.
    let count = match &runner.state().waiting_for {
        WaitingFor::TributeChoice { player, count, .. } => {
            assert_eq!(*player, P1, "the chosen opponent decides pay-or-decline");
            *count
        }
        other => panic!("expected TributeChoice, got {other:?}"),
    };
    assert_eq!(count, 2, "Tribute N is copied with the creature (CR 707.2)");

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("pay tribute");
    drive_to_prompt(&mut runner);

    let token = copy_token(&runner).expect("the Tribute copy token must enter the battlefield");
    let obj = &runner.state().objects[&token];
    assert_eq!(
        obj.counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied(),
        Some(2),
        "CR 702.104a: paid tribute puts N +1/+1 counters on the entering token"
    );
    assert!(
        obj.chosen_attributes.iter().any(|a| matches!(
            a,
            engine::types::ability::ChosenAttribute::TributeOutcome(
                engine::types::ability::TributeOutcome::Paid
            )
        )),
        "CR 702.104b: the outcome must persist on the token for the companion trigger"
    );
    assert!(
        runner.state().liminal_entries.is_empty(),
        "a committed entry must leave no liminal record"
    );
}

/// CR 702.104b: declining on a copy token must record `Declined` because the
/// chosen opponent actually declined — not because the prompt was skipped.
#[test]
fn copy_token_of_tribute_creature_declined_records_outcome_and_enters() {
    let (mut runner, _) = copy_token_of(TRIBUTE_ORACLE);
    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose opponent");

    // Pin that the decline below is answering the CR 702.104a prompt, addressed
    // to the CHOSEN opponent — symmetric with the paid row. Without this the
    // `.expect` alone would let a future regression that skips the prompt fail
    // here as a harness error rather than as the rules defect it is.
    match &runner.state().waiting_for {
        WaitingFor::TributeChoice { player, count, .. } => {
            assert_eq!(*player, P1, "the chosen opponent decides pay-or-decline");
            assert_eq!(
                *count, 2,
                "Tribute N is copied with the creature (CR 707.2)"
            );
        }
        other => panic!("expected TributeChoice, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline tribute");
    drive_to_prompt(&mut runner);

    let token = copy_token(&runner).expect("the Tribute copy token must enter the battlefield");
    let obj = &runner.state().objects[&token];
    assert_eq!(
        obj.counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "CR 702.104a: a declined tribute adds no counters"
    );
    assert!(
        obj.chosen_attributes.iter().any(|a| matches!(
            a,
            engine::types::ability::ChosenAttribute::TributeOutcome(
                engine::types::ability::TributeOutcome::Declined
            )
        )),
        "CR 702.104b: the declined outcome must persist on the token"
    );
}

/// CR 614.12a: the general "as this ~ enters, choose a <named attribute>" class
/// (`parse_as_enters_choose`) — the same `Effect::Choose { persist: true }` chain
/// Tribute's first stage uses, on a card that has nothing to do with Tribute.
/// The token must enter AND carry the choice its controller made.
#[test]
fn copy_token_of_as_enters_choose_persists_choice_and_enters() {
    let (mut runner, _) = copy_token_of(CHOOSE_COLOR_ORACLE);

    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            ..
        } => {
            assert_eq!(*player, P0, "the copy's controller makes the entry choice");
            assert!(matches!(
                choice_type,
                engine::types::ability::ChoiceType::Color { .. }
            ));
        }
        other => panic!("expected the as-enters colour choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: "Blue".to_string(),
        })
        .expect("choose colour");
    drive_to_prompt(&mut runner);

    let token = copy_token(&runner).expect("the copy token must enter the battlefield");
    assert_eq!(
        runner.state().objects[&token].chosen_color(),
        Some(engine::types::mana::ManaColor::Blue),
        "CR 607.2d: the as-enters choice must persist on the entering token"
    );
    assert!(
        runner.state().liminal_entries.is_empty(),
        "a committed entry must leave no liminal record"
    );
}
