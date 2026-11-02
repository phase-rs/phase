//! CR 603.3c + CR 700.2b: a modal triggered ability whose controller chooses no
//! mode ("choose up to one" / "choose any number" answered with zero modes) is
//! removed from the stack. It must not stay there as a no-op entry that players
//! can respond to.
//!
//! Oracle (Scryfall, Hullbreaker Horror):
//!   Flash
//!   This spell can't be countered.
//!   Whenever you cast a spell, choose up to one —
//!   • Return target spell you don't control to its owner's hand.
//!   • Return target nonland permanent to its owner's hand.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HULLBREAKER_ORACLE: &str = "Flash\n\
This spell can't be countered.\n\
Whenever you cast a spell, choose up to one \u{2014}\n\
\u{2022} Return target spell you don't control to its owner's hand.\n\
\u{2022} Return target nonland permanent to its owner's hand.";

const DIVINATION_ORACLE: &str = "Draw two cards.";

struct Board {
    runner: GameRunner,
    horrors: Vec<ObjectId>,
    spell: ObjectId,
    bear: ObjectId,
}

fn board_in(mut scenario: GameScenario, horror_count: usize) -> Board {
    scenario.at_phase(Phase::PreCombatMain);
    let horrors = (0..horror_count)
        .map(|_| {
            scenario
                .add_creature(P0, "Hullbreaker Horror", 7, 8)
                .from_oracle_text_with_keywords(&["Flash"], HULLBREAKER_ORACLE)
                .id()
        })
        .collect();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }
    Board {
        runner: scenario.build(),
        horrors,
        spell,
        bear,
    }
}

fn board(horror_count: usize) -> Board {
    board_in(GameScenario::new(), horror_count)
}

fn triggers_from(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|e| {
            e.source_id == source && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

fn spell_on_stack(state: &GameState, spell: ObjectId) -> bool {
    state
        .stack
        .iter()
        .any(|e| e.id == spell || e.source_id == spell)
}

fn hand_size(state: &GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize].hand.len()
}

fn cast_divination(b: &mut Board) {
    let card_id = b.runner.state().objects[&b.spell].card_id;
    b.runner
        .act(GameAction::CastSpell {
            object_id: b.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Divination must be accepted");
}

/// Answer any trigger-ordering prompt and stop at the next Hullbreaker mode
/// prompt, asserting it offers zero modes (min 0, max 1). Returns its source.
fn advance_to_mode_prompt(b: &mut Board) -> ObjectId {
    for _ in 0..8 {
        match b.runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                b.runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering the Hullbreaker triggers must be accepted");
            }
            WaitingFor::AbilityModeChoice {
                source_id,
                modal,
                is_activated,
                ..
            } => {
                assert!(
                    b.horrors.contains(&source_id),
                    "reach: mode prompt must be a Hullbreaker's"
                );
                assert!(
                    !is_activated,
                    "reach: the prompt is for a triggered ability"
                );
                assert_eq!(modal.min_choices, 0, "reach: 'up to one' lowers to min 0");
                assert_eq!(modal.max_choices, 1, "reach: 'up to one' lowers to max 1");
                return source_id;
            }
            other => panic!(
                "reach: expected a Hullbreaker mode prompt, got {}",
                other.variant_name()
            ),
        }
    }
    panic!("reach: no Hullbreaker mode prompt surfaced");
}

fn choose_no_mode(b: &mut Board) {
    b.runner
        .act(GameAction::SelectModes { indices: vec![] })
        .expect("choosing zero modes must be accepted (min_choices 0)");
}

/// Choose the "return target nonland permanent" mode and target the Bear.
fn choose_bounce_bear(b: &mut Board) {
    b.runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choosing mode 2 must be accepted");
    while let WaitingFor::TriggerTargetSelection { .. } = b.runner.state().waiting_for {
        b.runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(b.bear)),
            })
            .expect("targeting the opponent's Bear must be accepted");
    }
}

fn resolve_stack(b: &mut Board) {
    for _ in 0..40 {
        match b.runner.state().waiting_for {
            WaitingFor::Priority { .. } if b.runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                b.runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
            }
            ref other => panic!("unexpected wait while resolving: {}", other.variant_name()),
        }
    }
    panic!("the stack did not empty");
}

/// Assert the state right after a lone Hullbreaker's zero-mode answer: only
/// Divination is on the stack and its caster holds priority.
fn assert_trigger_removed(state: &GameState, b: &Board) {
    assert!(
        spell_on_stack(state, b.spell),
        "reach: Divination must still be on the stack"
    );
    assert_eq!(
        triggers_from(state, b.horrors[0]),
        0,
        "CR 603.3c + CR 700.2b: with no mode chosen the trigger is removed from the stack"
    );
    assert_eq!(state.stack.len(), 1, "only Divination remains on the stack");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the caster receives priority with only Divination on the stack, got {}",
        state.waiting_for.variant_name()
    );
    assert!(
        state.pending_trigger.is_none() && state.pending_trigger_entry.is_none(),
        "no construction cursor survives the removal"
    );
}

#[test]
fn hullbreaker_zero_modes_removes_trigger_from_stack() {
    let mut b = board(1);
    cast_divination(&mut b);
    advance_to_mode_prompt(&mut b);
    choose_no_mode(&mut b);

    assert_trigger_removed(b.runner.state(), &b);

    let hand_before = hand_size(b.runner.state(), P0);
    resolve_stack(&mut b);
    assert_eq!(
        hand_size(b.runner.state(), P0),
        hand_before + 2,
        "Divination resolves normally once the removed trigger is gone"
    );
    assert_eq!(
        b.runner.state().objects[&b.bear].zone,
        Zone::Battlefield,
        "no mode was chosen, so nothing is bounced"
    );
}

/// Control: choosing mode 2 bounces the opponent's creature, proving the
/// trigger and mode path are live.
#[test]
fn hullbreaker_mode_two_bounces_nonland_permanent_control() {
    let mut b = board(1);
    cast_divination(&mut b);
    advance_to_mode_prompt(&mut b);
    choose_bounce_bear(&mut b);
    assert_eq!(
        triggers_from(b.runner.state(), b.horrors[0]),
        1,
        "control: with a mode chosen the trigger stays on the stack"
    );
    resolve_stack(&mut b);
    assert_eq!(
        b.runner.state().objects[&b.bear].zone,
        Zone::Hand,
        "control: mode 2 must return the Bear to its owner's hand"
    );
}

/// CR 603.3b + CR 603.3c: two Hullbreakers trigger on the same cast. Choosing no
/// mode for the first removes only that trigger; the second is still announced
/// and keeps its chosen mode.
#[test]
fn zero_modes_on_first_of_a_batch_still_announces_the_sibling() {
    let mut b = board(2);
    cast_divination(&mut b);

    let first = advance_to_mode_prompt(&mut b);
    choose_no_mode(&mut b);
    assert_eq!(
        triggers_from(b.runner.state(), first),
        0,
        "the first trigger is removed with no mode chosen"
    );

    let second = advance_to_mode_prompt(&mut b);
    assert_ne!(
        first, second,
        "reach: the sibling Hullbreaker's trigger is announced next"
    );
    choose_bounce_bear(&mut b);

    let state = b.runner.state();
    assert!(
        spell_on_stack(state, b.spell),
        "reach: Divination is on the stack"
    );
    assert_eq!(
        triggers_from(state, first),
        0,
        "the removed trigger stays gone"
    );
    assert_eq!(
        triggers_from(state, second),
        1,
        "the sibling trigger reaches the stack"
    );
    assert_eq!(state.stack.len(), 2, "Divination plus the sibling trigger");

    resolve_stack(&mut b);
    assert_eq!(
        b.runner.state().objects[&b.bear].zone,
        Zone::Hand,
        "the sibling trigger's chosen mode resolves"
    );
}

/// Both triggers in one batch answered with no mode: both are removed.
#[test]
fn zero_modes_on_every_trigger_of_a_batch_removes_them_all() {
    let mut b = board(2);
    cast_divination(&mut b);
    let first = advance_to_mode_prompt(&mut b);
    choose_no_mode(&mut b);
    let second = advance_to_mode_prompt(&mut b);
    assert_ne!(first, second, "reach: both Hullbreakers prompt");
    choose_no_mode(&mut b);

    let state = b.runner.state();
    assert!(
        spell_on_stack(state, b.spell),
        "reach: Divination is on the stack"
    );
    assert_eq!(state.stack.len(), 1, "both triggers are removed");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the caster receives priority, got {}",
        state.waiting_for.variant_name()
    );
}

/// The AI's legal-action set offers the zero-mode answer, and taking it goes
/// through the same removal.
#[test]
fn ai_zero_mode_candidate_removes_the_trigger() {
    let mut b = board(1);
    cast_divination(&mut b);
    advance_to_mode_prompt(&mut b);

    let zero_mode = GameAction::SelectModes { indices: vec![] };
    assert!(
        engine::ai_support::legal_actions(b.runner.state()).contains(&zero_mode),
        "reach: the AI may choose no mode for a 'choose up to one' trigger"
    );
    b.runner
        .act(zero_mode)
        .expect("the AI's zero-mode answer must be accepted");
    assert_trigger_removed(b.runner.state(), &b);
}

/// Save/restore while the mode prompt is open: the restored game removes the
/// trigger the same way.
#[test]
fn zero_modes_after_save_restore_at_the_mode_prompt() {
    let mut b = board(1);
    cast_divination(&mut b);
    advance_to_mode_prompt(&mut b);

    let saved = serde_json::to_string(b.runner.state()).expect("save at the mode prompt");
    b.runner = GameRunner::from_state(serde_json::from_str(&saved).expect("restore"));
    assert!(
        matches!(
            b.runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "reach: the restored game is still at the mode prompt"
    );
    assert_eq!(
        triggers_from(b.runner.state(), b.horrors[0]),
        1,
        "reach: the in-construction entry survives the round trip"
    );

    choose_no_mode(&mut b);
    assert_trigger_removed(b.runner.state(), &b);
}

/// CR 800.4a: an opponent leaving while the mode prompt is open does not disturb
/// the removal.
#[test]
fn zero_modes_after_an_opponent_leaves_mid_prompt() {
    let mut b = board_in(GameScenario::new_n_player(3, 7), 1);
    cast_divination(&mut b);
    advance_to_mode_prompt(&mut b);

    let leaver = PlayerId(2);
    b.runner
        .act(GameAction::Concede { player_id: leaver })
        .expect("conceding mid-prompt must be accepted");
    assert!(
        matches!(
            b.runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { player, .. } if player == P0
        ),
        "reach: the mode prompt survives an opponent leaving, got {}",
        b.runner.state().waiting_for.variant_name()
    );

    choose_no_mode(&mut b);
    assert_trigger_removed(b.runner.state(), &b);
}
