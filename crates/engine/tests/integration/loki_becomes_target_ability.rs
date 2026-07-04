//! CR 115.1a + CR 602.2b + CR 603.2h — Loki, God of Mischief.
//!
//! "Whenever a player or permanent becomes the target of an ability you control,
//! draw a card. This ability triggers only once each turn."
//!
//! These integration tests drive the real activation pipeline (`activate(..)
//! .target_object(..).resolve()`): an ability P0 controls targets a battlefield
//! permanent, which emits `GameEvent::BecomesTarget` with the ability as source.
//! That matches Loki's `valid_source = StackAbility { controller: Some(You) }`
//! and `valid_card = Typed(Permanent ∧ InZone(Battlefield))`, so Loki draws — but
//! the `TriggerConstraint::OncePerTurn` limiter caps it at one draw per turn
//! (CR 603.2h) and resets at the turn boundary (turns.rs clears
//! `triggers_fired_this_turn`).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const LOKI_TRIGGER: &str = "Whenever a player or permanent becomes the target of an ability you \
     control, draw a card. This ability triggers only once each turn.";

/// A repeatable (no-tap), single-target activated ability P0 controls. Each
/// activation targets a creature → a `BecomesTarget` event sourced from an
/// ability P0 controls.
const PINGER_ABILITY: &str = "{G}: Target creature gets +1/+1 until end of turn.";

/// Seed `n` green mana into P0's floating pool immediately before an activation
/// (the pool empties at step boundaries, so it is topped up per activation).
fn add_green(runner: &mut GameRunner, n: usize) {
    for _ in 0..n {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    }
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn pinger_ability_index(runner: &GameRunner, pinger: ObjectId) -> usize {
    runner.state().objects[&pinger]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("pinger must expose its activated targeted ability")
}

/// §8.b PRIMARY — two qualifying targetings in one turn draw EXACTLY ONE card.
///
/// Discrimination: if the `OncePerTurn` constraint were not wired onto Loki's
/// trigger (e.g. the dispatch arm clobbered it), the second targeting would draw
/// a second card and the hand delta would be +2. The `== 1` assertion flips on
/// that regression, so the constraint wiring is live, not vacuous.
#[test]
fn loki_draws_once_per_turn_for_two_targetings() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _loki = scenario
        .add_creature_from_oracle(P0, "Loki, God of Mischief", 2, 2, LOKI_TRIGGER)
        .id();
    let pinger = scenario
        .add_creature_from_oracle(P0, "Mischief Pinger", 1, 1, PINGER_ABILITY)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);

    let mut runner = scenario.build();
    let idx = pinger_ability_index(&runner, pinger);

    let hand_before = hand_len(&runner, P0);

    add_green(&mut runner, 1);
    runner.activate(pinger, idx).target_object(bear).resolve();
    add_green(&mut runner, 1);
    runner.activate(pinger, idx).target_object(bear).resolve();

    assert_eq!(
        hand_len(&runner, P0) - hand_before,
        1,
        "two ability targetings in one turn must draw exactly one card (CR 603.2h once-per-turn cap)"
    );
}

/// §8.b RESET — after the turn boundary clears `triggers_fired_this_turn`, a new
/// targeting on P0's next turn draws again. Crosses the turn cycle through the
/// real engine pipeline (the same `legal_actions`-driven advance used by the
/// council-of-four per-turn-reset test) so the reset is exercised by production
/// turn machinery, not by poking state.
#[test]
fn loki_once_per_turn_limiter_resets_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _loki = scenario
        .add_creature_from_oracle(P0, "Loki, God of Mischief", 2, 2, LOKI_TRIGGER)
        .id();
    let pinger = scenario
        .add_creature_from_oracle(P0, "Mischief Pinger", 1, 1, PINGER_ABILITY)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    // Generous libraries so neither player decks out while the harness traverses
    // a full turn cycle (P1's turn + into P0's next turn) (CR 104.3c / 704.5b).
    let deck: Vec<&str> = vec!["Forest"; 30];
    scenario.with_library_top(P0, &deck);
    scenario.with_library_top(P1, &deck);

    let mut runner = scenario.build();
    let idx = pinger_ability_index(&runner, pinger);

    // Turn 1: fire once (draw +1), then a second targeting that the limiter caps.
    let t1_before = hand_len(&runner, P0);
    add_green(&mut runner, 1);
    runner.activate(pinger, idx).target_object(bear).resolve();
    add_green(&mut runner, 1);
    runner.activate(pinger, idx).target_object(bear).resolve();
    assert_eq!(
        hand_len(&runner, P0) - t1_before,
        1,
        "turn 1 must draw exactly one card (limiter caps the second targeting)"
    );

    // Advance the real pipeline until P0's next precombat main. Prefer passing
    // priority; submit the empty form of any forced decision so the turn keeps
    // moving. Never cast or tap — the per-turn limiter must reset purely from the
    // turn boundary (turns.rs clears `triggers_fired_this_turn`).
    let start_turn = runner.state().turn_number;
    let mut advanced = false;
    for _ in 0..2000 {
        if runner.state().turn_number > start_turn
            && runner.state().active_player == P0
            && runner.state().phase == Phase::PreCombatMain
        {
            advanced = true;
            break;
        }
        let actions = engine::ai_support::legal_actions(runner.state());
        let progress = actions
            .iter()
            .find(|a| matches!(a, GameAction::PassPriority))
            .or_else(|| {
                actions.iter().find(|a| {
                    matches!(
                        a,
                        GameAction::DeclareAttackers { .. }
                            | GameAction::DeclareBlockers { .. }
                            | GameAction::SelectCards { .. }
                            | GameAction::ChooseTarget { .. }
                    )
                })
            })
            .cloned();
        match progress {
            Some(action) => {
                if runner.act(action).is_err() {
                    break;
                }
            }
            None => break,
        }
    }
    assert!(
        advanced,
        "harness must reach P0's next precombat main; parked at turn {} player {:?} phase {:?} waiting {:?}",
        runner.state().turn_number,
        runner.state().active_player,
        runner.state().phase,
        runner.state().waiting_for,
    );

    // Turn 2: the limiter has reset, so a fresh targeting draws again. Measuring
    // the delta from AFTER the natural draw step isolates Loki's draw.
    let t2_before = hand_len(&runner, P0);
    add_green(&mut runner, 1);
    runner.activate(pinger, idx).target_object(bear).resolve();
    assert_eq!(
        hand_len(&runner, P0) - t2_before,
        1,
        "after the turn boundary clears the limiter, the new turn's targeting draws again"
    );
}
