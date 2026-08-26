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

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .unwrap_or(0)
}

/// Drive the trigger to the point where its "you may draw" decision is live and
/// accept it.
///
/// The `Priority`-arm-then-`break` shape this replaces exited before ever
/// dispatching `DecideOptionalEffect`: after `run_combat` the trigger is on the
/// stack under `WaitingFor::Priority`, so the very first iteration took the
/// priority arm, ran `advance_until_stack_empty` (which stops as soon as
/// `PassPriority` is rejected under `OptionalEffectChoice`) and broke out. The
/// draw therefore never happened and the "net hand size unchanged" assertion
/// held vacuously (issue #6858). Keep advancing and accepting until neither is
/// possible so the optional draw is genuinely taken.
fn accept_optional_effect(runner: &mut GameRunner) {
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("optional effect choice must succeed");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.advance_until_stack_empty();
            }
            _ => return,
        }
    }
    panic!(
        "optional draw never settled; stuck on {:?}",
        runner.state().waiting_for
    );
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
    let library_before = library_len(&runner, P0);
    assert_eq!(hand_before, 7, "precondition: seven cards in hand");

    run_combat(&mut runner, vec![zombie], vec![]);
    accept_optional_effect(&mut runner);

    // Reach-guard (issue #6858): the net-hand-size assertion below is satisfied
    // just as well by a trigger that drew nothing and discarded nothing, so it
    // cannot stand alone. Pin the draw against the library and the discard
    // against the live prompt before reading hand size.
    assert_eq!(
        library_len(&runner, P0),
        library_before - 1,
        "one opponent was damaged: the optional draw must have taken a card"
    );
    let WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "\"If you do, discard that many cards\" must prompt for one discard, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(count, 1, "discard exactly as many as were drawn");

    runner
        .act(GameAction::SelectCards {
            cards: cards.iter().copied().take(1).collect(),
        })
        .expect("submitting the discard selection must succeed");
    accept_optional_effect(&mut runner);

    assert_eq!(
        hand_len(&runner, P0),
        hand_before,
        "one opponent was damaged: draw 1, then discard 1 — net hand size unchanged"
    );
}
