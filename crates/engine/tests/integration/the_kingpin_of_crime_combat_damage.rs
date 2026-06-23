//! The Kingpin of Crime (MSH) — attack-triggered, duration-bound, filter-scoped
//! "assign combat damage by toughness" continuous effect.
//!
//! Oracle (relevant ability):
//!   Whenever you attack, you may pay 2 life. If you do, until end of turn,
//!   creatures you control with toughness greater than their power assign combat
//!   damage equal to their toughness rather than their power.
//!
//! The residual gap on main was the effect-side continuous clause:
//!   Unimplemented[creatures]: "creatures you control with toughness greater than
//!   their power assign combat damage equal to their toughness rather than their
//!   power"
//! The trigger shell (`YouAttack`, optional `PayLife { 2 }`, `IfYouDo`/
//! `OptionalEffectPerformed` gate, `UntilEndOfTurn` duration) already parsed; the
//! gap was the *plural* surface form ("assign … their toughness rather than their
//! power") of the Doran/Assault-Formation damage-by-toughness predicate, which the
//! singular-only `parse_continuous_modifications` arm did not recognize.
//!
//! These tests drive the real combat pipeline through `apply`:
//! declare-attackers → attack trigger on stack → optional `DecideOptionalEffect`
//! → layer system → combat damage. They assert the *life delta* on the defending
//! player, which only changes if the toughness-based assignment is actually
//! applied at damage time.
//!
//! CR 510.1a (a creature assigns combat damage equal to its power — the default),
//! CR 613.11 (the toughness-rather-than-power substitution is a continuous
//! rule-modifying effect), CR 611.2a (the effect lasts until end of turn),
//! CR 119.3 (paying 2 life). Filter axis: "toughness greater than their power".

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ContinuousModification, ControllerRef, Duration, Effect, FilterProp, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const KINGPIN_ATTACK_ABILITY: &str = "Whenever you attack, you may pay 2 life. If you do, until end of turn, creatures you control with toughness greater than their power assign combat damage equal to their toughness rather than their power.";

/// Declare `attacker` against P1, resolve the Kingpin attack trigger on the
/// stack, then accept (`pay = true`) or decline (`pay = false`) the optional
/// "pay 2 life" cost, and drive combat damage to completion (no blockers).
fn attack_and_decide(runner: &mut GameRunner, attacker: ObjectId, pay: bool) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    // CR 508.2: attack trigger is put on the stack; passing priority resolves it,
    // which raises the optional "pay 2 life" decision.
    runner.pass_both_players();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "Kingpin attack trigger must prompt to pay 2 life, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: pay })
        .expect("decide optional pay-2-life");

    // The continuous effect (if paid) is now registered; recompute layers so the
    // affected creatures pick up `assigns_damage_from_toughness` before damage.
    evaluate_layers(runner.state_mut());

    // Advance through the remaining priority passes to combat damage. No blockers.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare (no) blockers");
    }
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
}

/// Primary positive: a 2/4 (toughness > power) attacker, after paying 2 life,
/// assigns 4 (toughness) instead of 2 (power) → P1 falls to 16.
///
/// REVERT PROBE: with the plural-form fix reverted, the effect clause lowers to
/// `Effect::Unimplemented` (no continuous modification), so the 2/4 assigns its
/// power (2) and P1 is left at 18 — this assertion flips from 16 to 18.
#[test]
fn kingpin_paid_two_four_assigns_toughness_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Kingpin of Crime", 4, 4, KINGPIN_ATTACK_ABILITY);
    let attacker = scenario.add_creature(P0, "Defensive Brute", 2, 4).id();

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    attack_and_decide(&mut runner, attacker, true);

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 2,
        "paying the optional cost must deduct 2 life"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        16,
        "2/4 attacker (toughness > power) must assign 4 combat damage after Kingpin's effect"
    );
}

/// Filter axis: a 4/2 attacker (power > toughness) fails the
/// "toughness greater than their power" filter, so even after paying it assigns
/// its power (4), not its toughness (2) → P1 falls to 16 (4 damage).
///
/// This discriminates the *filter*: if the effect wrongly applied to all
/// creatures you control (dropping the P/T-comparison filter), the 4/2 would
/// assign 2 and P1 would be at 18. 16 proves the filter excluded it and normal
/// power-based assignment governs.
#[test]
fn kingpin_paid_four_two_fails_filter_assigns_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Kingpin of Crime", 4, 4, KINGPIN_ATTACK_ABILITY);
    let attacker = scenario.add_creature(P0, "Glass Cannon", 4, 2).id();

    let mut runner = scenario.build();
    attack_and_decide(&mut runner, attacker, true);

    let obj = runner.state().objects.get(&attacker).expect("attacker");
    assert!(
        !obj.assigns_damage_from_toughness,
        "4/2 (power > toughness) must NOT be granted toughness-based assignment"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        16,
        "4/2 attacker fails the toughness>power filter, so it assigns its power (4)"
    );
}

/// Optional-cost axis: declining the "pay 2 life" cost leaves normal power-based
/// assignment — the 2/4 assigns 2 (power) → P1 falls to 18, and P0 loses no life.
///
/// Discriminates the `IfYouDo` gate: if the continuous effect applied regardless
/// of payment, the 2/4 would deal 4 and P1 would be at 16.
#[test]
fn kingpin_declined_two_four_assigns_power_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Kingpin of Crime", 4, 4, KINGPIN_ATTACK_ABILITY);
    let attacker = scenario.add_creature(P0, "Defensive Brute", 2, 4).id();

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    attack_and_decide(&mut runner, attacker, false);

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "declining the optional cost must not deduct life"
    );
    let obj = runner.state().objects.get(&attacker).expect("attacker");
    assert!(
        !obj.assigns_damage_from_toughness,
        "declining the cost must not grant toughness-based assignment"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        18,
        "without paying, the 2/4 assigns its power (2)"
    );
}

/// Duration axis (CR 514.2 + CR 611.2a): the grant is `until end of turn`. The
/// 2/4 assigns from toughness during the turn it is paid, and the continuous
/// effect lapses at that turn's cleanup step.
///
/// Discriminates the duration: if the grant were permanent (or had no expiry),
/// `assigns_damage_from_toughness` would still be set after the turn rolls over;
/// this asserts it is cleared. Mirrors the Doran +X/+X-lapse test, which passes
/// priority until `turn_number` advances and then asserts the modification is
/// gone (no second combat needed, avoiding deck-out edge cases on a minimal
/// scenario library).
#[test]
fn kingpin_effect_lapses_after_end_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Kingpin of Crime", 4, 4, KINGPIN_ATTACK_ABILITY);
    let attacker = scenario.add_creature(P0, "Defensive Brute", 2, 4).id();

    let mut runner = scenario.build();
    let start_turn = runner.state().turn_number;

    // This turn: pay, deal 4 → P1 at 16, and the grant is live.
    attack_and_decide(&mut runner, attacker, true);
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        16,
        "the turn it is paid, the 2/4 assigns its toughness (4)"
    );
    assert!(
        runner.state().objects[&attacker].assigns_damage_from_toughness,
        "grant must be live during the turn it was paid"
    );

    // Advance one full turn by passing priority. The cleanup step (CR 514.2)
    // ends the until-end-of-turn effect.
    for _ in 0..400 {
        if runner.state().turn_number > start_turn {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        runner.state().turn_number > start_turn,
        "the game must advance past the turn the grant was created"
    );

    evaluate_layers(runner.state_mut());
    assert!(
        !runner.state().objects[&attacker].assigns_damage_from_toughness,
        "until-end-of-turn grant must have lapsed at cleanup — flag cleared next turn"
    );
}

/// Parser-level guard: the plural surface form of the damage-by-toughness
/// predicate now lowers to a `GenericEffect` carrying
/// `AssignDamageFromToughness` with a `controller=You` + toughness>power filter
/// and an `UntilEndOfTurn` duration — never `Effect::Unimplemented`.
///
/// This locks the exact gap that was closed; it is a shape test that backs the
/// runtime tests above (which carry the semantics).
#[test]
fn kingpin_clause_lowers_to_assign_damage_from_toughness() {
    let def = parse_effect_chain(
        "Until end of turn, creatures you control with toughness greater than their power assign combat damage equal to their toughness rather than their power.",
        AbilityKind::Activated,
    );
    let Effect::GenericEffect {
        static_abilities,
        duration,
        ..
    } = &*def.effect
    else {
        panic!("expected GenericEffect, got {:?}", def.effect);
    };
    assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    let stat = static_abilities
        .first()
        .expect("one continuous static ability");
    assert!(
        stat.modifications
            .contains(&ContinuousModification::AssignDamageFromToughness),
        "plural clause must carry AssignDamageFromToughness, got {:?}",
        stat.modifications
    );
    let affected = stat.affected.as_ref().expect("affected filter");
    let TargetFilter::Typed(tf) = affected else {
        panic!("expected a typed affected filter, got {affected:?}");
    };
    assert_eq!(
        tf.controller,
        Some(ControllerRef::You),
        "filter must be controller=You"
    );
    // CR 208.1: "their power" must lower to the self-referential `ToughnessGTPower`
    // prop (each creature's own toughness vs its own power) — NOT a source-scoped
    // `PtComparison` that would compare against the Kingpin's power.
    assert!(
        tf.properties.contains(&FilterProp::ToughnessGTPower),
        "filter must carry the self-referential ToughnessGTPower property, got {:?}",
        tf.properties
    );
}
