//! CR 702.94a + CR 113.6b: Aminatou, Veil Piercer grants miracle to
//! enchantment cards in the controller's hand ("Each enchantment card in
//! your hand has miracle. Its miracle cost is equal to its mana cost reduced
//! by {4}."). The granted keyword must offer a reveal when its controller
//! draws it as the first card of THEIR OWN turn, exactly like a printed
//! miracle keyword does — reported as not firing on the controller's own
//! turn (only appearing to work on an opponent's turn) in issue #6006.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const AMINATOU: &str = "Each enchantment card in your hand has miracle. Its miracle cost is equal to its mana cost reduced by {4}.";

/// Action budget for a single-turn scenario (draw step through main phase).
const MAX_ACTIONS_SINGLE_TURN: usize = 400;
/// Action budget for a scenario spanning an intervening opponent's turn.
const MAX_ACTIONS_MULTI_TURN: usize = 2000;

fn advance_turn_action(runner: &mut GameRunner) {
    let action = match &runner.state().waiting_for {
        WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => GameAction::PassPriority,
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        other => panic!("unexpected turn-action prompt: {other:?}"),
    };
    runner.act(action).expect("advance turn action");
}

/// CR 702.94a: The controller's own mandatory draw-step draw is their first
/// draw of their own turn, so an Aminatou-granted miracle must offer a
/// reveal for it exactly as a printed miracle keyword would.
#[test]
fn aminatou_grant_offers_miracle_on_controllers_own_draw_step() {
    let mut scenario = GameScenario::new();
    // `at_phase` sets turn 2, avoiding CR 103.8a's skipped initial draw step.
    scenario.at_phase(Phase::Upkeep);
    scenario
        .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
        .from_oracle_text(AMINATOU);
    let drawn = scenario
        .add_spell_to_library_top(P0, "SixEnchant", false)
        .as_enchantment()
        .id();
    let mut runner = scenario.build();

    for _ in 0..MAX_ACTIONS_SINGLE_TURN {
        match &runner.state().waiting_for {
            WaitingFor::MiracleReveal {
                player, object_id, ..
            } => {
                assert_eq!(*player, P0, "the drawing player must receive the offer");
                assert_eq!(
                    *object_id, drawn,
                    "the draw-step offer must identify the card just drawn"
                );
                return;
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::PreCombatMain => {
                panic!(
                    "Aminatou's granted miracle did not offer a reveal for the \
                     controller's own draw-step draw (issue #6006)"
                );
            }
            _ => advance_turn_action(&mut runner),
        }
    }

    panic!("did not reach a MiracleReveal prompt within {MAX_ACTIONS_SINGLE_TURN} actions");
}

/// CR 702.94a: The grant must keep working on the controller's SECOND own
/// draw step, after a full opponent's turn has passed in between — the exact
/// "works on opponent's turn but not mine" shape reported in issue #6006.
#[test]
fn aminatou_grant_offers_miracle_after_an_intervening_opponent_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario
        .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
        .from_oracle_text(AMINATOU);
    // Library top-to-bottom for P0: ordinary card (drawn turn 2), then the
    // miracle-eligible enchantment (drawn turn 4, after P1's turn 3).
    let miracle_card = scenario
        .add_spell_to_library_top(P0, "SixEnchant", false)
        .as_enchantment()
        .id();
    scenario.add_card_to_library_top(P0, "Ordinary Draw");
    // Stock P1's library so their intervening turn's mandatory draw doesn't
    // deck them out (CR 104.3c) before P0's second draw step is reached.
    for i in 0..10 {
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
    }
    let mut runner = scenario.build();

    let mut seen_miracle_reveal = false;
    for _ in 0..MAX_ACTIONS_MULTI_TURN {
        match &runner.state().waiting_for {
            WaitingFor::MiracleReveal { object_id, .. } if *object_id == miracle_card => {
                seen_miracle_reveal = true;
                break;
            }
            WaitingFor::MiracleReveal { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline unrelated miracle offer");
            }
            WaitingFor::Priority { .. }
                if runner.state().phase == Phase::PreCombatMain
                    && runner.state().active_player == P0
                    && runner.state().turn_number >= 4 =>
            {
                break;
            }
            _ => advance_turn_action(&mut runner),
        }
    }

    assert!(
        seen_miracle_reveal,
        "Aminatou's granted miracle did not offer a reveal on the controller's \
         own draw step after an opponent's turn passed (issue #6006); ended at \
         turn {} phase {:?} active_player {:?} waiting_for {:?}",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
}
