//! GitHub issue #3296 — Hordewing Skaab discards the entire hand instead of
//! discarding only as many cards as were drawn.
//!
//! Oracle: "Whenever one or more Zombies you control deal combat damage to one
//! or more of your opponents, you may draw cards equal to the number of
//! opponents dealt damage this way. If you do, discard that many cards."

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::run_combat;

const HORDEWING_ORACLE: &str = "Flying\n\
Other Zombies you control have flying.\n\
Whenever one or more Zombies you control deal combat damage to one or more of your opponents, \
you may draw cards equal to the number of opponents dealt damage this way. If you do, discard that many cards.";

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

fn accept_optional_effect(runner: &mut GameRunner) {
    loop {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("optional effect choice must succeed");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.advance_until_stack_empty();
                break;
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();
}

#[test]
fn hordewing_skaab_discards_only_as_many_as_drawn_not_entire_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Hordewing Skaab", 3, 3, HORDEWING_ORACLE)
        .flying()
        .with_subtypes(vec!["Zombie", "Horror"]);
    let zombie = scenario
        .add_creature(P0, "Zombie Attacker", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();

    for i in 0..7 {
        scenario.add_creature_to_hand(P0, &format!("Hand Card {i}"), 0, 0);
    }
    for name in ["Library A", "Library B", "Library C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    assert_eq!(hand_before, 7, "precondition: seven cards in hand");

    run_combat(&mut runner, vec![zombie], vec![]);
    accept_optional_effect(&mut runner);

    let hand_after = hand_len(&runner, P0);
    assert_eq!(
        hand_after, hand_before,
        "one opponent was damaged: draw 1, then discard 1 — net hand size unchanged"
    );
}
