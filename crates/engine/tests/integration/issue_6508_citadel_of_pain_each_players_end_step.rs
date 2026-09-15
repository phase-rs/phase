//! Issue #6508 — Citadel of Pain damages each player at their own end step by
//! the number of untapped lands THAT player controls.
//!
//! > At the beginning of each player's end step, this enchantment deals X damage
//! > to that player, where X is the number of untapped lands they control.
//!
//! Reported as "damages its controller instead of opponents". The board gives
//! the two players different untapped-land counts, so the opponent's end step
//! separates the three readings: the correct one (the opponent takes THEIR
//! count), a count bound to the enchantment's controller (the opponent takes
//! the CONTROLLER's count), and a trigger that fires only on the controller's
//! own end step (the opponent takes nothing).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ControllerRef, Effect, QuantityExpr, QuantityRef, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

// Verbatim Oracle text (Scryfall, 2026-09-15).
const CITADEL_OF_PAIN: &str = "At the beginning of each player's end step, this enchantment deals X damage to that player, where X is the number of untapped lands they control.";

const CONTROLLER_LANDS: i32 = 5;
const OPPONENT_LANDS: i32 = 2;

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// Pass priority on the real action path until `player`'s end step has begun.
///
/// `GameRunner::advance_to_phase` re-enters the turn interpreter for the step it
/// is already in, which re-runs that step's beginning-of-step triggers when it is
/// called with priority open. Walking with `PassPriority` is what a client does,
/// so each end step fires Citadel of Pain exactly once.
fn pass_priority_into_end_step_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..60 {
        let state = runner.state();
        if state.active_player == player && state.phase == Phase::End {
            return;
        }
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "unexpected prompt while advancing: {:?}",
            state.waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be accepted");
    }
    panic!(
        "did not reach {player:?}'s end step; stopped at {:?} of {:?}'s turn",
        runner.state().phase,
        runner.state().active_player
    );
}

#[test]
fn citadel_of_pain_hits_each_player_for_their_own_untapped_lands() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &player in &[P0, P1] {
        scenario.with_library_top(player, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    let citadel = scenario
        .add_creature(P0, "Citadel of Pain", 0, 0)
        .as_enchantment()
        .from_oracle_text(CITADEL_OF_PAIN)
        .id();
    for _ in 0..CONTROLLER_LANDS {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    for _ in 0..OPPONENT_LANDS {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    let mut runner = scenario.build();

    assert!(
        runner.state().objects[&citadel]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| {
                entry.definition.mode == TriggerMode::Phase
                    && entry.definition.phase == Some(Phase::End)
            }),
        "reach-guard: Citadel of Pain's Oracle text must provide its end-step trigger"
    );
    let (p0_start, p1_start) = (life(&runner, P0), life(&runner, P1));

    // CR 603.2: the end-step trigger fires on the controller's own end step.
    runner.advance_to_end_step();
    assert!(
        runner.state().active_player == P0 && runner.state().phase == Phase::End,
        "must reach P0's end step; stopped at {:?} of {:?}'s turn on {:?}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        life(&runner, P0),
        p0_start - CONTROLLER_LANDS,
        "on P0's end step, P0 takes damage equal to the untapped lands P0 controls"
    );
    assert_eq!(
        life(&runner, P1),
        p1_start,
        "P1 takes nothing on P0's end step"
    );

    pass_priority_into_end_step_of(&mut runner, P1);
    assert!(
        runner.state().active_player == P1 && runner.state().phase == Phase::End,
        "must reach P1's end step; stopped at {:?} of {:?}'s turn on {:?}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
    // CR 603.2: it fires again on the opponent's end step. The ability is controlled by P0, but
    // "that player" and "they" are the player whose end step it is.
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == citadel && entry.controller == P0),
        "Citadel's P0-controlled trigger must be on the stack at P1's end step; stack={:?}",
        runner.state().stack
    );
    runner.advance_until_stack_empty();

    // CR 120.3a: damage to a player causes that much life loss.
    assert_eq!(
        life(&runner, P1),
        p1_start - OPPONENT_LANDS,
        "on P1's end step, P1 takes damage equal to the untapped lands P1 controls \
         ({OPPONENT_LANDS}), not the lands its controller controls ({CONTROLLER_LANDS})"
    );
    assert_eq!(
        life(&runner, P0),
        p0_start - CONTROLLER_LANDS,
        "the controller takes nothing more on the opponent's end step"
    );
}

/// Lowered-count controller of a phase trigger's `DealDamage` amount.
fn end_step_damage_count_controller(oracle: &str) -> Option<ControllerRef> {
    let parsed = parse_oracle_text(
        oracle,
        "Citadel of Pain",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::Phase && trigger.phase == Some(Phase::End))?;
    match trigger
        .execute
        .as_deref()
        .map(|ability| ability.effect.as_ref())
    {
        Some(Effect::DealDamage {
            amount:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(typed),
                        },
                },
            ..
        }) => typed.controller.clone(),
        _ => None,
    }
}

/// SHAPE. CR 107.3c: the where-X clause defines X. Its third-person "they"
/// names the player whose step it is, while a printed "you" still names the
/// controller. Each case is the other's reach-guard: both must reach the
/// `DealDamage` count, so neither can pass by the parse failing.
#[test]
fn where_x_count_binds_they_to_the_scoped_player_and_keeps_you() {
    assert_eq!(
        end_step_damage_count_controller(CITADEL_OF_PAIN),
        Some(ControllerRef::ScopedPlayer),
        "\"they control\" in the where-X count must name the end-step player"
    );
    let you_variant = CITADEL_OF_PAIN.replace("they control", "you control");
    assert_ne!(
        you_variant, CITADEL_OF_PAIN,
        "reach-guard: the variant must differ"
    );
    assert_eq!(
        end_step_damage_count_controller(&you_variant),
        Some(ControllerRef::You),
        "a printed \"you control\" in the where-X count must stay the controller"
    );
}
