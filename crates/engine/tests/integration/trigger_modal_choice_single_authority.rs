//! CR 603.3c + CR 603.3d: a modal triggered ability's mode choice is announced
//! when the ability is put on the stack. This engine necessarily splits that one
//! announcement across a pause — `dispatch_pending_trigger_context` resolves the
//! legal choice and pushes the entry, then `begin_pending_trigger_target_selection`
//! raises the `AbilityModeChoice` prompt — and the two halves live in different
//! modules.
//!
//! These tests pin the contract that makes that split safe: the prompt the
//! controller actually sees is the answer of the ONE authority,
//! `ability_utils::resolve_legal_modal_choice`, evaluated in the triggering-event
//! window. Neither half may re-implement the mode-choice sequence (dynamic cap →
//! non-target unavailability → per-mode target legality → cross-mode assignment
//! cap → CR 603.3c no-legal-mode verdict), because a second copy is free to drift
//! from the announcement and it is the *prompt* the player is bound by.
//!
//! Both halves of the announcement are covered non-vacuously by real cards driven
//! through the production cast pipeline:
//!   * the CAP, via Riku's `EventContextSourceModesChosen` "choose up to X"
//!     (`max_choices` is event-context-dependent, so it also proves the prompt is
//!     built inside the trigger-event window);
//!   * the UNAVAILABLE-MODE SET, via Bumi's Earthbend mode, which requires a
//!     target land (CR 115.1) that does not exist on an empty board.
//!
//! Oracle text is the same engine-authoritative text used by
//! `riku_modal_modes_chosen_cap.rs`.

use engine::game::ability_utils::resolve_legal_modal_choice;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AnnouncedModalChoice, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const RIKU_ORACLE: &str = "Whenever you cast a modal spell, choose up to X, where X is the number of times you chose a mode for that spell —\n\u{2022} Exile the top card of your library. Until the end of your next turn, you may play it.\n\u{2022} Put a +1/+1 counter on Riku. It gains trample until end of turn.\n\u{2022} Create a 1/1 blue Bird creature token with flying.";

const ABRADE_ORACLE: &str =
    "Choose one \u{2014}\n\u{2022} Abrade deals 3 damage to target creature.\n\u{2022} Destroy target artifact.";

const BUMI_ORACLE: &str = "When Bumi enters, choose up to X, where X is the number of Lesson cards in your graveyard \u{2014}\n\u{2022} Put three +1/+1 counters on Bumi.\n\u{2022} Target player scries 3.\n\u{2022} Earthbend 3.";

/// What the paused `AbilityModeChoice` prompt offers the controller.
struct PromptedChoice {
    max_choices: usize,
    mode_count: usize,
    unavailable_modes: Vec<usize>,
}

/// Ask the single authority the same question the engine asked when it announced
/// the choice, using the paused `pending_trigger` as the input the announcement
/// used: the RAW modal header off the card, that trigger's own source/controller,
/// and its triggering event restored as the live event window (what
/// `push_trigger_event_context` does around both halves).
///
/// Panics if no modal pending trigger is parked — that would mean the prompt was
/// reached without the in-flight trigger the contract is about.
fn authority_answer(runner: &mut GameRunner) -> AnnouncedModalChoice {
    let pending = runner
        .state()
        .pending_trigger
        .as_ref()
        .expect("a modal pending trigger must still be parked while its mode prompt is outstanding")
        .clone();
    let modal = pending
        .modal
        .as_ref()
        .expect("the parked trigger must carry its raw modal header");

    // Restore the trigger-event window the announcement ran inside. The engine
    // restores it after each half, so the paused snapshot has no live event.
    let state = runner.state_mut();
    state.current_trigger_event = pending.trigger_event.clone();
    state.current_trigger_events = pending.trigger_event.iter().cloned().collect();
    state.current_trigger_match_count = pending.subject_match_count;

    let answer = resolve_legal_modal_choice(
        runner.state(),
        pending.source_id,
        pending.controller,
        modal,
        &pending.mode_abilities,
    )
    .expect("the authority must report a legal choice for a trigger that reached its mode prompt");

    let state = runner.state_mut();
    state.current_trigger_event = None;
    state.current_trigger_events = Vec::new();
    state.current_trigger_match_count = None;
    answer
}

/// Drive the pipeline until a triggered `AbilityModeChoice` is outstanding,
/// answering the cast's own mode/target windows on the way, and return what the
/// prompt offers. Panics if the run settles at `Priority` (the trigger never
/// fired) so a vacuous pass is impossible.
fn drive_to_mode_prompt(
    runner: &mut GameRunner,
    modes: &[usize],
    targets: &[ObjectId],
) -> PromptedChoice {
    let mut remaining_targets = targets.to_vec();
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice {
                modal,
                unavailable_modes,
                ..
            } => {
                return PromptedChoice {
                    max_choices: modal.max_choices,
                    mode_count: modal.mode_count,
                    unavailable_modes,
                }
            }
            WaitingFor::ModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: modes.to_vec(),
                    })
                    .expect("SelectModes must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                let target = remaining_targets.remove(0);
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("ChooseTarget must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
            }
            other => panic!(
                "unexpected WaitingFor while driving to the mode prompt: {}",
                other.variant_name()
            ),
        }
    }
    panic!("pipeline did not reach a triggered AbilityModeChoice within the step budget");
}

/// CAP HALF — CR 603.3d + CR 700.2b: Riku's triggered modal offers a "choose up
/// to X" cap that reads the triggering spell's chosen-mode count
/// (`EventContextSourceModesChosen`). The paused prompt's `max_choices` must be
/// exactly what `resolve_legal_modal_choice` reports, and must be the live
/// event-context value (1 for a one-mode Abrade) rather than 0 or Riku's own
/// `mode_count` — so this also proves the prompt is built with the
/// triggering-event window pushed, as the announcement was.
#[test]
fn prompt_cap_equals_modal_choice_authority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Riku, of Many Paths", 2, 4, RIKU_ORACLE);
    let dummy = scenario.add_creature(P1, "Target Dummy", 3, 3).id();
    let abrade = scenario
        .add_spell_to_hand_from_oracle(P0, "Abrade", true, ABRADE_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&abrade].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: abrade,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell must be accepted");

    let prompt = drive_to_mode_prompt(&mut runner, &[0], &[dummy]);

    // Non-vacuous: the event-context cap really resolved off the cast spell.
    assert_eq!(
        prompt.max_choices, 1,
        "Riku's cap must be the 1 mode chosen for Abrade (CR 700.2d), proving the \
         prompt resolved the dynamic cap inside the trigger-event window"
    );
    assert_eq!(prompt.mode_count, 3, "Riku's header must parse three modes");

    let announced = authority_answer(&mut runner);
    assert_eq!(
        prompt.max_choices, announced.modal.max_choices,
        "the prompt's cap must BE the single authority's answer, not an \
         independently derived one (CR 603.3c)"
    );
    assert_eq!(
        prompt.unavailable_modes, announced.unavailable_modes,
        "the prompt's unavailable-mode set must BE the single authority's answer"
    );
}

/// UNAVAILABLE HALF — CR 603.3c + CR 115.1: Bumi's ETB modal includes
/// "Earthbend 3", which requires a target land. With no land on the battlefield
/// that mode cannot be chosen, so the announcement marks it unavailable. The
/// paused prompt's unavailable set must be exactly the authority's — and must be
/// non-empty here, so the equality is not satisfied by two empty vectors.
#[test]
fn prompt_unavailable_modes_equal_modal_choice_authority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two Lessons in the graveyard so the dynamic cap resolves to 2 (< mode_count),
    // keeping the cap live rather than saturated at the clamp.
    for i in 0..2 {
        scenario
            .add_spell_to_graveyard(P0, &format!("Lesson {i}"), false)
            .with_subtypes(vec!["Lesson"]);
    }
    let bumi = scenario
        .add_creature_to_hand_from_oracle(P0, "Bumi, King of Three Trials", 4, 4, BUMI_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    // Reach-guard: the Earthbend mode is unavailable because NO land exists.
    assert!(
        !runner.state().objects.values().any(|obj| obj.zone
            == engine::types::zones::Zone::Battlefield
            && obj
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Land)),
        "the fixture must start with no land on the battlefield for Earthbend to be \
         target-unavailable"
    );

    let card_id = runner.state().objects[&bumi].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bumi,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Bumi must be accepted");

    let prompt = drive_to_mode_prompt(&mut runner, &[], &[]);

    // Non-vacuous: at least one mode really is unavailable.
    assert!(
        !prompt.unavailable_modes.is_empty(),
        "Earthbend needs a target land (CR 115.1); with no land its mode must be \
         announced unavailable, got {:?}",
        prompt.unavailable_modes
    );

    let announced = authority_answer(&mut runner);
    assert_eq!(
        prompt.unavailable_modes, announced.unavailable_modes,
        "the prompt's unavailable-mode set must BE the single authority's answer, not \
         an independently derived one (CR 603.3c)"
    );
    assert_eq!(
        prompt.max_choices, announced.modal.max_choices,
        "the prompt's cap must BE the single authority's answer"
    );
}
