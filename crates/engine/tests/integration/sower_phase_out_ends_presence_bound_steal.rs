//! CR 702.26f: a "for as long as ~ remains on the battlefield" steal ends when
//! the stealing permanent phases out — and does NOT come back when it phases
//! back in — while the CR 702.26d event-deadline class ("until ~ leaves the
//! battlefield", Banisher Priest's exile) keeps running across the same
//! phase-out.
//!
//! Sower of Temptation's steal lowers to a `WhileHostOnBattlefield` transient
//! continuous effect. Before the wording split the duration lowered to the
//! `UntilHostLeavesPlay` event deadline, `transient_effect_is_live` asked only
//! the zone question, and a phased-out Sower kept its stolen creature —
//! against CR 702.26f: "effects with 'for as long as' durations that track
//! that permanent (see rule 611.2b) end when that permanent phases out because
//! they can no longer see it."
//!
//! Both tests are discriminating, in opposite directions (each of the three
//! mutations in the first bullet measured as a full `--lib` +
//! `--test integration` pair):
//!   * Revert the parser split (map "remains on the battlefield" back onto
//!     `UntilHostLeavesPlay`) or drop the `WhileHostOnBattlefield` filter leg
//!     from `transient_effect_is_live` → the steal visibly survives the
//!     phase-out → the first test fails at its control assert. Drop only the
//!     presence arm of `prune_lapsed_host_bound_effects` → the filter still
//!     refuses the steal but the effect stays in the list → the first test
//!     fails at its ENDED assert instead.
//!   * Ask the phasing question of EVERY host-bound duration (the over-reach a
//!     previous review round shipped and reverted) → Banisher Priest's exile
//!     would end at the phase-out and the exiled creature return early → the
//!     second test fails.
//!
//! The phase-out goes through the production entry point,
//! `game::phasing::phase_out_object` — the same call
//! `effects::phase_out::resolve` makes for Clever Concealment and Teferi's
//! Protection — so the test proves the engine's own phase-out reaches the
//! pruning seam, not merely that a predicate reads a hand-set field.

use engine::game::game_object::PhaseOutCause;
use engine::game::layers::evaluate_layers;
use engine::game::phasing::{phase_in_object, phase_out_object};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SOWER_ORACLE: &str = "Flying\nWhen this creature enters, gain control of \
    target creature for as long as this creature remains on the battlefield.";

const BANISHER_PRIEST_ORACLE: &str = "When this creature enters, exile target \
    creature an opponent controls until this creature leaves the battlefield.";

const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
     creature. It gains haste until end of turn. (It can attack and {T} this turn.)";

#[test]
fn sowers_steal_ends_on_phase_out_and_does_not_revive_on_phase_in() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sower = scenario
        .add_creature_to_hand_from_oracle(P0, "Sower of Temptation", 2, 2, SOWER_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(sower).target_object(bear).resolve();
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().controller,
        P0,
        "reach-guard: the resolved ETB steal must be in force before the phase-out"
    );

    let mut events = Vec::new();
    phase_out_object(
        runner.state_mut(),
        sower,
        PhaseOutCause::Directly,
        &mut events,
    );
    assert!(
        !runner.state().objects.get(&sower).unwrap().is_phased_in(),
        "reach-guard: the production phase-out must actually phase Sower out"
    );
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().controller,
        P1,
        "CR 702.26f: the presence-bound steal ends when Sower phases out"
    );
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .all(|e| e.source_id != sower),
        "the steal must be ENDED (removed at the pruning seam), not merely suppressed"
    );

    phase_in_object(runner.state_mut(), sower, &mut events);
    assert!(
        runner.state().objects.get(&sower).unwrap().is_phased_in(),
        "reach-guard: Sower phased back in"
    );
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().controller,
        P1,
        "CR 702.26f: the duration ENDED at the phase-out — phasing back in must not revive the steal"
    );
    assert_eq!(
        runner.state().objects.get(&sower).unwrap().zone,
        Zone::Battlefield,
        "CR 702.26d: phasing is not a zone change, so Sower never left the battlefield"
    );
}

#[test]
fn banisher_priests_event_deadline_exile_survives_its_phase_out() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let priest = scenario
        .add_creature_to_hand_from_oracle(P0, "Banisher Priest", 2, 2, BANISHER_PRIEST_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();

    runner.cast(priest).target_object(bear).resolve();
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().zone,
        Zone::Exile,
        "reach-guard: the ETB exile must have resolved"
    );

    let mut events = Vec::new();
    phase_out_object(
        runner.state_mut(),
        priest,
        PhaseOutCause::Directly,
        &mut events,
    );
    assert!(
        !runner.state().objects.get(&priest).unwrap().is_phased_in(),
        "reach-guard: the production phase-out must actually phase the Priest out"
    );
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().zone,
        Zone::Exile,
        "CR 702.26d: a phase-out is not the Priest leaving the battlefield, so \
         the exiled creature must NOT return"
    );

    phase_in_object(runner.state_mut(), priest, &mut events);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().zone,
        Zone::Exile,
        "still exiled after the phase-in — the deadline never fired"
    );

    // Positive reach-guard for the pair machinery itself: when the Priest
    // ACTUALLY leaves the battlefield, the deadline fires and the creature
    // returns — proving the two phase-out assertions above were not vacuously
    // green on a dead exile/return link.
    runner.cast(bolt).target_object(priest).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().zone,
        Zone::Battlefield,
        "the Priest died, so \"until this creature leaves the battlefield\" ended \
         and the exiled creature returns"
    );
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().controller,
        P1,
        "the returned creature is back under its owner's control"
    );
}

/// CR 611.2b, the counter-direction of the split this PR turns on: a control
/// change of the HOST must NOT end a presence-bound effect.
///
/// The whole reason `Duration::WhileControllingHost` may wear
/// `ReplacementCondition::ControllerControlsSource` — and the two other host
/// wordings may not — is that the control gate ends on a control change while
/// the presence reading survives it. Every other presence test in the tree
/// measures a PHASE-OUT or a battlefield exit, both of which the control gate
/// would also end; only a control change discriminates the two readings. So
/// without this test, re-pointing the `WhileHostOnBattlefield` arm at
/// `controller_controls_source_gate` stays green.
///
/// The authority is CR 611.2a — an effect lasts as long as STATED, and the
/// stated duration here is a presence condition, not a control one, so a control
/// change does not reach it. (CR 702.26d is not in play: nothing phases.
/// CR 611.2c is a different question — it fixes WHICH objects the effect
/// affects, not when it ends — so it is not cited as the reason.) Gaining
/// control of Sower therefore does not hand over the creature it took.
///
/// Revert-probe: pointing the presence arm of the host-bound lapse pass at the
/// control gate reds the final assertion (the bear returns to P1 the moment
/// P0 stops controlling Sower). The pre-steal reach-guard rules out the vacuous
/// opposite (no steal in force at all).
#[test]
fn sowers_steal_survives_a_control_change_of_its_own_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sower = scenario
        .add_creature_to_hand_from_oracle(P0, "Sower of Temptation", 2, 2, SOWER_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Treason", false, ACT_OF_TREASON)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    // Both libraries must be non-empty: an empty-library draw is its own
    // state-based ending and would confound the measurement.
    scenario.with_library_top(P0, &["Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest"]);
    let mut runner = scenario.build();

    runner.cast(sower).target_object(bear).resolve();
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().controller,
        P0,
        "reach-guard: the resolved ETB steal must be in force before the control change"
    );

    // Act of Treason is a sorcery, so P1 needs their own main phase.
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().controller,
        P0,
        "reach-guard: the steal must survive the turn boundary too — otherwise a \
         turn-scoped expiry, not the control change, is what the final assertion \
         would be measuring"
    );

    runner.cast(steal).target_object(sower).resolve();
    assert_eq!(
        runner.state().objects.get(&sower).unwrap().controller,
        P1,
        "reach-guard: Act of Treason must actually move control of Sower"
    );
    assert_eq!(
        runner.state().objects.get(&sower).unwrap().zone,
        Zone::Battlefield,
        "reach-guard: and Sower must still be on the battlefield — that is the \
         whole difference from the phase-out sibling"
    );

    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects.get(&bear).unwrap().controller,
        P0,
        "CR 611.2a: 'for as long as ~ remains on the battlefield' is a PRESENCE \
         condition. Sower changed controller but never left, so the duration has \
         not ended and the stolen creature stays with the effect's controller"
    );
}
