//! Issue #8388: Dragon Whelp was sacrificed without reaching its threshold.
//!
//! "{R}: This creature gets +1/+0 until end of turn. If this ability has been
//! activated four or more times this turn, sacrifice this creature at the
//! beginning of the next end step." — the threshold clause reached no condition
//! parser at all, so the sacrifice rider ran unconditionally and the Whelp died
//! after a single activation. The gap was invisible to coverage because
//! `line_has_condition_text` carried "this ability has been activated" on its
//! structural-exemption list.
//!
//! Fix: `AbilityCondition::AbilityUseCountThisTurn { tally: Activated, .. }`
//! reads the CR 602.2a announcement ledger
//! (`GameState::activated_abilities_this_turn`, keyed by the same
//! `(source_id, ability_index)` the stamp in `push_ability_entry` uses).
//!
//! These tests drive the real production path — `GameAction::ActivateAbility`
//! through the stack, then the end step — rather than calling the condition
//! evaluator directly, because the defect was that the condition never reached
//! the ability at all. A parser-shape or direct-evaluator test passes on the
//! broken build.
//!
//! Discriminators (flip when the fix is reverted): pre-fix the Whelp is in the
//! graveyard after ONE activation, so tests 1 and 2 both fail.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall, verified 2026-09-09). Nalathni Dragon,
// Farrelite Priest and Initiates of the Ebon Hand print the same second
// sentence; only the first clause differs.
const DRAGON_WHELP_ORACLE: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn. If this ability has been activated four or more times this turn, sacrifice this creature at the beginning of the next end step.";

fn whelp_scenario(activations: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // One red mana per planned activation — the {R} cost is what makes each
    // `ActivateAbility` legal, so an under-funded pool would fail the
    // activation rather than test the threshold.
    scenario.with_mana_pool(
        P0,
        (0..activations)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let whelp = scenario
        .add_creature_from_oracle(P0, "Dragon Whelp", 2, 3, DRAGON_WHELP_ORACLE)
        .id();
    (scenario.build(), whelp)
}

fn activate(runner: &mut GameRunner, whelp: ObjectId, nth: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: whelp,
            ability_index: 0,
        })
        .unwrap_or_else(|e| panic!("activation {nth} must be payable: {e:?}"));
    runner.resolve_top();
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Cross combat and the end step, resolving whatever fires.
///
/// `advance_to_phase` stops at any non-priority wait, and the harness surfaces
/// `WaitingFor::DeclareAttackers` as a turn-based action that plain priority
/// passes cannot answer — so combat is crossed explicitly. The
/// `advance_until_stack_empty` is what actually RESOLVES a delayed trigger that
/// fired at the end step; without it the Whelp survives no matter what the
/// condition decided, and both assertions below would pass vacuously.
fn cross_to_resolved_end_step(runner: &mut GameRunner) {
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the delayed sacrifice is timed to the next end step (CR 603.7a)"
    );
}

fn activation_count(runner: &GameRunner, whelp: ObjectId) -> u32 {
    runner
        .state()
        .activated_abilities_this_turn
        .get(&(whelp, 0))
        .copied()
        .unwrap_or(0)
}

/// Test 1 (the reported bug, discriminating): three activations are BELOW the
/// printed threshold of four, so the rider must not arm and the Whelp must
/// survive its end step.
///
/// The `delayed_triggers` assertion is the load-bearing one. Checking only the
/// final zone would pass on a build where the rider armed correctly but nothing
/// resolved it — the trigger has to be shown ABSENT, not merely inconsequential.
#[test]
fn three_activations_do_not_arm_the_sacrifice() {
    let (mut runner, whelp) = whelp_scenario(3);

    for nth in 1..=3 {
        activate(&mut runner, whelp, nth);
    }

    // Reach-guard: the ability really resolved three times.
    assert_eq!(
        activation_count(&runner, whelp),
        3,
        "three activations must be recorded on the CR 602.2a ledger"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "three activations is below the printed four-or-more threshold, so the \
         delayed sacrifice must never be installed; got {:?}",
        runner.state().delayed_triggers
    );

    cross_to_resolved_end_step(&mut runner);

    assert_eq!(
        zone_of(&runner, whelp),
        Zone::Battlefield,
        "the Whelp must survive below its threshold (pre-fix the ungated rider \
         sacrificed it after the very first activation — issue #8388)"
    );
}

/// Test 2 (the other side of the boundary): the fourth activation arms the
/// delayed trigger, which sacrifices the Whelp at the next end step.
///
/// Paired with test 1 deliberately — a fix that simply never fired the rider
/// would satisfy test 1 on its own, and that is precisely the regression this
/// pairing is here to catch.
#[test]
fn four_activations_sacrifice_the_whelp_at_the_next_end_step() {
    let (mut runner, whelp) = whelp_scenario(4);

    for nth in 1..=4 {
        activate(&mut runner, whelp, nth);
    }

    assert_eq!(
        activation_count(&runner, whelp),
        4,
        "four activations must be recorded on the CR 602.2a ledger"
    );
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "the fourth activation must install the delayed end-step sacrifice"
    );
    // CR 603.7a: the sacrifice is delayed, so the Whelp is still around now.
    assert_eq!(
        zone_of(&runner, whelp),
        Zone::Battlefield,
        "the rider is delayed to the next end step, not immediate"
    );

    cross_to_resolved_end_step(&mut runner);

    assert_eq!(
        zone_of(&runner, whelp),
        Zone::Graveyard,
        "the armed delayed trigger must sacrifice the Whelp at the end step"
    );
}
