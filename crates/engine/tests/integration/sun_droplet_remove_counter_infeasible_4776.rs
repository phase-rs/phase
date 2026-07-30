//! Issue #4776 — Sun Droplet: "you may remove a charge counter. If you do,
//! gain 1 life" must NOT be offered when the artifact has zero charge counters.
//!
//! Oracle (Scryfall-verified):
//!   "Whenever you're dealt damage, put that many charge counters on this
//!    artifact. At the beginning of each upkeep, you may remove a charge counter
//!    from this artifact. If you do, you gain 1 life."
//!
//! Root cause: the up-front optional-effect feasibility gate
//! (`optional_effect_is_infeasible`) only special-cased `PutChosenCounter`;
//! `RemoveCounter` fell through to "always feasible", so with zero counters the
//! player was still prompted, could accept, `OptionalEffectPerformed` was
//! recorded true, and the `GainLife` rider fired — life from nothing.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 122.1: counters; removing a counter that isn't present does nothing.
//! - CR 608.2d: a player can't choose an impossible option.
//! - CR 118.12: "If you do" gates the rider on the optional action being chosen;
//!   an impossible action is never offered, so the rider never fires.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const SUN_DROPLET: &str = "Whenever you're dealt damage, put that many charge counters on this artifact.\nAt the beginning of each upkeep, you may remove a charge counter from this artifact. If you do, you gain 1 life.";

fn charge() -> CounterType {
    CounterType::Generic("charge".to_string())
}

/// Zero charge counters: the upkeep trigger must NOT surface the "you may
/// remove a charge counter?" optional prompt, and P0's life must not change.
/// Reverting the fix re-exposes the prompt and (on accept) the phantom lifegain.
#[test]
fn sun_droplet_zero_counters_no_optional_prompt_no_lifegain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let droplet = {
        let mut b = scenario.add_creature_from_oracle(P0, "Sun Droplet", 0, 0, SUN_DROPLET);
        b.as_artifact();
        b.id()
    };
    // Library padding so advancing the turn doesn't deck anyone.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&droplet]
            .counters
            .get(&charge())
            .copied()
            .unwrap_or(0),
        0,
        "precondition: Sun Droplet has no charge counters"
    );
    let life_before = runner.life(P0);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    // No optional prompt was ever surfaced for the infeasible removal.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the infeasible 'remove a charge counter' must not prompt: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "no counter to remove means no life gained (CR 603.12 'if you do')"
    );
}

/// With a charge counter present the ability IS feasible: the optional prompt is
/// offered, and accepting removes the counter and gains 1 life. This is the
/// positive reach-guard proving the fix does not over-suppress a real choice.
#[test]
fn sun_droplet_with_counter_prompts_and_gains_life_on_accept() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let droplet = {
        let mut b = scenario.add_creature_from_oracle(P0, "Sun Droplet", 0, 0, SUN_DROPLET);
        b.as_artifact();
        b.id()
    };
    scenario.with_counter(droplet, charge(), 2);
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    runner.advance_to_upkeep();
    // Resolve the upkeep trigger; it must surface the optional prompt. Pass
    // priority until either the optional choice appears or the stack drains.
    for _ in 0..40 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        if runner.state().stack.is_empty()
            && !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. })
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "with a charge counter, the removal must be offered: {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the counter removal must be allowed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&droplet]
            .counters
            .get(&charge())
            .copied()
            .unwrap_or(0),
        1,
        "accepting must remove exactly one charge counter (2 -> 1)"
    );
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "removing a counter must gain 1 life"
    );
}

/// Review regression (matthewevans, #6022): a charge counter IS present but a
/// `CountersCantBeRemoved(charge)` static (Fear of Sleep Paralysis class) makes
/// its removal impossible. The optional prompt must STILL be suppressed — a
/// removal the rules forbid is an impossible option (CR 608.2d), so accepting it
/// must not fire the "if you do, gain 1 life" rider (CR 118.12). Reverting the
/// `counter_removal_blocked` check re-exposes the phantom lifegain.
#[test]
fn sun_droplet_counter_present_but_removal_blocked_no_prompt_no_lifegain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let droplet = {
        let mut b = scenario.add_creature_from_oracle(P0, "Sun Droplet", 0, 0, SUN_DROPLET);
        b.as_artifact();
        b.id()
    };
    scenario.with_counter(droplet, charge(), 2);
    // A separate permanent contributing a CountersCantBeRemoved(charge) static
    // that affects Sun Droplet (all permanents), freezing its charge counters.
    let warden = scenario.add_creature(P0, "Counter Warden", 2, 2).id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&warden)
        .unwrap()
        .static_definitions
        .push(
            StaticDefinition::new(StaticMode::CountersCantBeRemoved {
                counter_type: charge(),
            })
            .affected(TargetFilter::Typed(
                TypedFilter::permanent().controller(ControllerRef::You),
            )),
        );
    let life_before = runner.life(P0);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "a removal-blocked counter is an impossible option — no prompt: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "removal forbidden by a static means no life gained (CR 118.12)"
    );
    // The counter itself is untouched.
    assert_eq!(
        runner.state().objects[&droplet]
            .counters
            .get(&charge())
            .copied()
            .unwrap_or(0),
        2,
        "the frozen charge counters must remain"
    );
}
