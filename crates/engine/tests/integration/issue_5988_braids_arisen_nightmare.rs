//! Issue #5988 — Braids, Arisen Nightmare: when an opponent does not sacrifice a
//! matching permanent, that opponent loses 2 life and Braids' controller draws.
//!
//! Drives the real end-step trigger through `apply()` (not a hand-built execute
//! chain) to catch wiring defects in trigger dispatch / continuation resume.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const BRAIDS_ORACLE: &str = "At the beginning of your end step, you may sacrifice an artifact, \
creature, enchantment, land, or planeswalker. If you do, each opponent may sacrifice a permanent \
of their choice that shares a card type with it. For each opponent who doesn't, that player loses \
2 life and you draw a card.";

fn life(state: &engine::types::game_state::GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player")
        .life
}

fn hand_len(state: &engine::types::game_state::GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player")
        .hand
        .len()
}

#[test]
fn braids_end_step_opponent_without_match_loses_two_and_controller_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario
        .add_creature(P0, "Braids, Arisen Nightmare", 3, 3)
        .from_oracle_text(BRAIDS_ORACLE);
    let p0_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // P1 controls only a land — nothing shares the sacrificed creature type.
    scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    scenario.add_card_to_library_top(P0, "Library Card");

    let mut runner = scenario.build();
    let p0_life_before = life(runner.state(), P0);
    let p1_life_before = life(runner.state(), P1);
    let p0_hand_before = hand_len(runner.state(), P0);

    runner.advance_to_end_step();

    for _ in 0..80 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order end-step triggers");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                let accept = player == P0;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional effect decision");
            }
            WaitingFor::EffectZoneChoice { player, .. } if player == P0 => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![p0_creature],
                    })
                    .expect("controller sacrifice");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while stack resolving");
            }
            _ => runner.pass_both_players(),
        }
    }

    assert_eq!(
        life(runner.state(), P1),
        p1_life_before - 2,
        "opponent who could not sacrifice a matching permanent loses 2 life"
    );
    assert_eq!(
        life(runner.state(), P0),
        p0_life_before,
        "Braids' controller does not lose life"
    );
    assert_eq!(
        hand_len(runner.state(), P0),
        p0_hand_before + 1,
        "Braids' controller draws one card for the declining opponent"
    );
}

#[test]
fn braids_end_step_opponent_declines_matching_sacrifice_loses_two_and_controller_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario
        .add_creature(P0, "Braids, Arisen Nightmare", 3, 3)
        .from_oracle_text(BRAIDS_ORACLE);
    let p0_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.add_creature(P1, "Bear Cub", 2, 2);
    scenario.add_card_to_library_top(P0, "Library Card");

    let mut runner = scenario.build();
    let p0_life_before = life(runner.state(), P0);
    let p1_life_before = life(runner.state(), P1);
    let p0_hand_before = hand_len(runner.state(), P0);

    runner.advance_to_end_step();

    for _ in 0..80 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order end-step triggers");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                let accept = player == P0;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional effect decision");
            }
            WaitingFor::EffectZoneChoice { player, .. } if player == P0 => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![p0_creature],
                    })
                    .expect("controller sacrifice");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while stack resolving");
            }
            _ => runner.pass_both_players(),
        }
    }

    assert_eq!(
        life(runner.state(), P1),
        p1_life_before - 2,
        "opponent who declines to sacrifice loses 2 life"
    );
    assert_eq!(life(runner.state(), P0), p0_life_before);
    assert_eq!(
        hand_len(runner.state(), P0),
        p0_hand_before + 1,
        "controller draws for the declining opponent"
    );
}
