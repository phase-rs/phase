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
//!
//! The ETB targets one opponent as it is put on the stack (CR 115.1d); only that
//! opponent decides. The dilemma bounce is covered in `face_a_dilemma.rs`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::PlayerActionKind;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const TELL_ME: &str = "When this enchantment enters, target opponent may have you draw a card. If they don't, they lose 4 life and you surveil 1.\nWhenever you face a dilemma, you may return this enchantment to its owner's hand. (You face a dilemma as you choose one or more modes for a spell or ability.)";

const LEYLINE: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou have hexproof. (You can't be the target of spells or abilities your opponents control.)";

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
    scenario_with_leylines(player_count, &[])
}

/// `scenario`, with Leyline of Sanctity (CR 702.11c: its controller has
/// hexproof) on the battlefield under each player in `leylines`.
fn scenario_with_leylines(player_count: u8, leylines: &[PlayerId]) -> (GameRunner, ObjectId) {
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
    for &player in leylines {
        scenario.add_enchantment_from_oracle(player, "Leyline of Sanctity", LEYLINE);
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
    cast_and_resolve_with(runner, card, target, accept, |_| {})
}

/// `cast_and_resolve`, running `after_target` once the enters trigger's target
/// has been chosen and the trigger waits on the stack.
fn cast_and_resolve_with(
    runner: &mut GameRunner,
    card: ObjectId,
    target: PlayerId,
    accept: bool,
    mut after_target: impl FnMut(&mut GameRunner),
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
                after_target(runner);
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

/// M1 — CR 115.1 + CR 603.3d: "target opponent" never offers the controller as
/// a legal target for the enters trigger.
#[test]
fn enters_trigger_offers_only_opponents_as_targets() {
    let (mut runner, card) = scenario(2);
    let p1_life = runner.life(P1);
    let record = cast_and_resolve(&mut runner, card, P1, false);
    eprintln!(
        "M1 record: {record:?}; life P0 {} P1 {}",
        runner.life(P0),
        runner.life(P1)
    );
    match record.target_prompts.as_slice() {
        [] => {
            // CR 115.1: with no prompt the only legal target was auto-chosen;
            // it must be the opponent, who then lost 4 life on the decline.
            assert_eq!(runner.life(P1), p1_life - 4);
        }
        [legal] => {
            assert!(legal.contains(&TargetRef::Player(P1)));
            assert!(
                !legal.contains(&TargetRef::Player(P0)),
                "CR 115.1: the controller is not a legal \"target opponent\": {legal:?}"
            );
        }
        prompts => panic!("expected at most one target prompt, got {prompts:?}"),
    }
}

/// M2 — CR 115.1 + CR 608.2d: with three players only the announced target
/// (the second opponent, P2, so seat order cannot satisfy it) is asked; the
/// non-targeted opponent is never asked and loses nothing.
#[test]
fn three_players_only_the_targeted_opponent_decides() {
    const P2: PlayerId = PlayerId(2);
    let (mut runner, card) = scenario(3);
    let p0_life = runner.life(P0);
    let p1_life = runner.life(P1);
    let p2_life = runner.life(P2);
    let record = cast_and_resolve(&mut runner, card, P2, false);
    eprintln!(
        "M2 record: {record:?}; life P0 {} P1 {} P2 {}",
        runner.life(P0),
        runner.life(P1),
        runner.life(P2)
    );
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(runner.life(P0), p0_life);
    assert_eq!(runner.life(P1), p1_life);
    assert_eq!(runner.life(P2), p2_life - 4);
    assert_eq!(record.surveil_actors, vec![P0]);
}

/// M3 — CR 115.1d + CR 118.12: with three players the enters trigger raises
/// exactly one target prompt over the opponents only; P2 is chosen and accepts,
/// so P0 draws one and nobody loses life or surveils.
#[test]
fn three_players_accept_targets_one_opponent_and_controller_draws() {
    const P2: PlayerId = PlayerId(2);
    let (mut runner, card) = scenario(3);
    let lives = [runner.life(P0), runner.life(P1), runner.life(P2)];
    let p0_hand_after_cast = hand_len(&runner, P0) - 1;
    let record = cast_and_resolve(&mut runner, card, P2, true);
    let [legal] = record.target_prompts.as_slice() else {
        panic!("expected exactly one target prompt: {record:?}");
    };
    assert!(legal.contains(&TargetRef::Player(P1)));
    assert!(legal.contains(&TargetRef::Player(P2)));
    assert!(!legal.contains(&TargetRef::Player(P0)), "{legal:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(hand_len(&runner, P0), p0_hand_after_cast + 1);
    assert_eq!([runner.life(P0), runner.life(P1), runner.life(P2)], lives);
    assert!(record.surveil_actors.is_empty());
}

/// L-TMWYS — CR 702.11c + CR 603.3d: with every opponent hexproof the enters
/// trigger has no legal target and is removed; nobody is asked, nothing
/// happens. Paired positive: with only P1 hexproof, P2 is the sole legal target
/// and is chosen without a prompt; P2 declines and the decline result lands.
#[test]
fn no_legal_target_removes_the_enters_trigger() {
    const P2: PlayerId = PlayerId(2);
    let (mut runner, card) = scenario_with_leylines(3, &[P1, P2]);
    let lives = [runner.life(P0), runner.life(P1), runner.life(P2)];
    let p0_hand_after_cast = hand_len(&runner, P0) - 1;
    let record = cast_and_resolve(&mut runner, card, P2, false);
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert!(record.optional_actors.is_empty(), "{record:?}");
    assert!(record.surveil_actors.is_empty(), "{record:?}");
    assert_eq!(hand_len(&runner, P0), p0_hand_after_cast);
    assert_eq!([runner.life(P0), runner.life(P1), runner.life(P2)], lives);

    let (mut runner, card) = scenario_with_leylines(3, &[P1]);
    let lives = [runner.life(P0), runner.life(P1), runner.life(P2)];
    let record = cast_and_resolve(&mut runner, card, P2, false);
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(
        [runner.life(P0), runner.life(P1), runner.life(P2)],
        [lives[0], lives[1], lives[2] - 4]
    );
    assert_eq!(record.surveil_actors, vec![P0]);
}

/// F-TMWYS — CR 608.2b: the declared opponent gains hexproof while the
/// trigger waits on the stack, so its only target is illegal and it does not
/// resolve: nobody is asked, nobody loses life, nobody surveils or draws.
/// Positive: M2 (the same setup without the grant).
#[test]
fn target_illegal_on_resolution_does_nothing() {
    const P2: PlayerId = PlayerId(2);
    let (mut runner, card) = scenario(3);
    let lives = [runner.life(P0), runner.life(P1), runner.life(P2)];
    let p0_hand_after_cast = hand_len(&runner, P0) - 1;
    let record = cast_and_resolve_with(&mut runner, card, P2, false, |runner| {
        runner.state_mut().add_transient_continuous_effect(
            ObjectId(0),
            P2,
            Duration::Permanent,
            TargetFilter::SpecificPlayer { id: P2 },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }],
            None,
        );
    });
    assert_eq!(record.target_prompts.len(), 1, "REACH: {record:?}");
    assert!(record.optional_actors.is_empty(), "{record:?}");
    assert!(record.surveil_actors.is_empty(), "{record:?}");
    assert_eq!(hand_len(&runner, P0), p0_hand_after_cast);
    assert_eq!([runner.life(P0), runner.life(P1), runner.life(P2)], lives);
}
