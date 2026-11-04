//! Tell Me What You See — CR 118.12 + CR 109.5: the targeted opponent decides
//! the draw; on decline they lose 4 life and the controller surveils 1.
//!
//! "When this enchantment enters, target opponent may have you draw a card. If
//! they don't, they lose 4 life and you surveil 1." Both "they lose 4 life" and
//! "you surveil 1" are the effect of "If they don't" (CR 118.12), and "you" is
//! the enchantment's controller (CR 109.5), never the deciding opponent.
//!
//! The tests cast the enchantment from hand so the enters trigger comes from the
//! real zone change, then drive every prompt through `GameAction`s, recording
//! each prompt's actor. Every seat's library is seeded so a surveil by any seat
//! raises a `SurveilChoice` (CR 701.25c: surveil 0 would be no surveil event).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::PlayerActionKind;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const TELL_ME: &str = "When this enchantment enters, target opponent may have you draw a card. If they don't, they lose 4 life and you surveil 1.\nWhenever you face a dilemma, you may return this enchantment to its owner's hand. (You face a dilemma as you choose one or more modes for a spell or ability.)";

/// Every prompt the trigger raised, in order, with its actor.
#[derive(Debug, Default)]
struct Record {
    /// `legal_targets` of each target prompt, in prompt order.
    target_prompts: Vec<Vec<TargetRef>>,
    /// The player asked each `OptionalEffectChoice`.
    optional_actors: Vec<PlayerId>,
    /// The player of each `SurveilChoice`.
    surveil_actors: Vec<PlayerId>,
}

fn scenario(player_count: u8) -> (GameRunner, ObjectId) {
    let mut scenario = if player_count == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(player_count, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..player_count {
        let player = PlayerId(seat);
        for index in 0..3 {
            scenario.add_spell_to_library_top(player, &format!("Library {seat}-{index}"), false);
        }
    }
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Tell Me What You See", false, TELL_ME)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero())
        .id();
    (scenario.build(), card)
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn surveil_ledger(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(actor, kind)| *actor == player && *kind == PlayerActionKind::Surveil)
        .count()
}

/// Cast Tell Me What You See, target `target` with its enters trigger, answer
/// every optional prompt with `accept`, and resolve to an empty stack.
fn cast_and_resolve(
    runner: &mut GameRunner,
    card: ObjectId,
    target: PlayerId,
    accept: bool,
) -> Record {
    runner.cast(card).commit();
    let mut record = Record::default();
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                record
                    .target_prompts
                    .push(target_slots[selection.current_slot].legal_targets.clone());
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(target)),
                    })
                    .expect("choose the enters trigger's target opponent");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                record.optional_actors.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the draw choice");
            }
            WaitingFor::SurveilChoice { player, cards } => {
                record.surveil_actors.push(player);
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("keep the surveilled card on top");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return record;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while Tell Me What You See resolves");
            }
            other => panic!("unexpected Tell Me What You See prompt: {other:?}"),
        }
    }
    panic!("Tell Me What You See did not resolve: {record:?}");
}

/// R1 — CR 118.12 + CR 109.5 + CR 608.2c: on decline the targeted opponent
/// loses 4 life and then the CONTROLLER surveils 1; the deciding opponent never
/// surveils and the controller does not draw.
#[test]
fn decline_opponent_loses_four_and_controller_surveils_one() {
    let (mut runner, card) = scenario(2);
    let p0_life = runner.life(P0);
    let p1_life = runner.life(P1);
    let p0_hand_after_cast = hand_len(&runner, P0) - 1;

    let record = cast_and_resolve(&mut runner, card, P1, false);
    eprintln!(
        "R1 record: {record:?}; life P0 {} P1 {}",
        runner.life(P0),
        runner.life(P1)
    );

    // CR 118.12: only the targeted opponent decides whether P0 draws.
    assert_eq!(record.optional_actors, vec![P1]);
    // CR 118.12: the decline branch ran — the opponent lost 4 life.
    assert_eq!(runner.life(P1), p1_life - 4);
    assert_eq!(runner.life(P0), p0_life);
    // CR 109.5 + CR 701.25a: "you surveil 1" is the controller's surveil.
    assert_eq!(record.surveil_actors, vec![P0]);
    assert_eq!(surveil_ledger(&runner, P0), 1);
    assert_eq!(surveil_ledger(&runner, P1), 0);
    // CR 118.12: declining the draw means P0 draws nothing.
    assert_eq!(hand_len(&runner, P0), p0_hand_after_cast);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// R2 — CR 118.12: on accept the controller draws 1 and nothing else happens:
/// no life loss and no surveil by anyone, because "they lose 4 life and you
/// surveil 1" is the "If they don't" effect.
#[test]
fn accept_controller_draws_one_and_nobody_surveils() {
    let (mut runner, card) = scenario(2);
    let p0_life = runner.life(P0);
    let p1_life = runner.life(P1);
    let p0_hand_after_cast = hand_len(&runner, P0) - 1;

    let record = cast_and_resolve(&mut runner, card, P1, true);
    eprintln!(
        "R2 record: {record:?}; life P0 {} P1 {}",
        runner.life(P0),
        runner.life(P1)
    );

    // CR 118.12: the accept branch ran — P1 decided and P0 drew one card.
    assert_eq!(record.optional_actors, vec![P1]);
    assert_eq!(hand_len(&runner, P0), p0_hand_after_cast + 1);
    assert_eq!(runner.life(P1), p1_life);
    assert_eq!(runner.life(P0), p0_life);
    // CR 118.12: the decline effect, surveil included, does not happen.
    assert!(record.surveil_actors.is_empty());
    assert_eq!(surveil_ledger(&runner, P0), 0);
    assert_eq!(surveil_ledger(&runner, P1), 0);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
