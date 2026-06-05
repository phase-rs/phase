//! Regression for GitHub issue #766 — Thoughtweft Lieutenant.
//!
//! Oracle: "When Thoughtweft Lieutenant or another Kithkin you control enters,
//! target creature you control gets +1/+1 and gains trample until end of turn."
//!
//! The Discord report: a creature buffed by Thoughtweft Lieutenant (granted
//! +1/+1 AND trample until end of turn) did not trample its buffed excess over
//! a blocker. The fix is ALREADY on current main: the layer system applies the
//! +1/+1 and the granted Trample keyword, and combat damage reads the LAYERED
//! power/toughness (not the printed base) plus the granted keyword. This file is
//! a regression GUARD that pins the full combat pipeline so the behavior cannot
//! silently re-regress; it is NOT a fail-first test.
//!
//! Attacker-buff path: the **transient continuous effect** path. The test
//! installs the exact `TransientContinuousEffect` the parsed Thoughtweft trigger
//! produces — `AddPower(1) + AddToughness(1) + AddKeyword(Trample)` with
//! `Duration::UntilEndOfTurn`, scoped to the attacker via
//! `TargetFilter::SpecificObject` — then runs it through `evaluate_layers` and
//! real combat. This isolates the layered-P/T + granted-keyword combat behavior
//! the bug report exercised without depending on card-data being loaded.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, Duration, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// Install the same continuous effect Thoughtweft Lieutenant's trigger creates
/// on its chosen target: +1/+1 and trample until end of turn, scoped to a single
/// creature. Mirrors `add_transient_continuous_effect` usage in the engine
/// (e.g. Threaten-style control effects in `database/synthesis.rs`).
fn buff_attacker_like_thoughtweft(runner: &mut GameRunner, attacker: ObjectId) {
    let controller = runner
        .state()
        .objects
        .get(&attacker)
        .expect("attacker on battlefield")
        .controller;
    // CR 611.2: the trigger creates a continuous effect lasting until end of
    // turn that modifies P/T (layer 7c) and grants a keyword (layer 6).
    runner.state_mut().add_transient_continuous_effect(
        attacker,
        controller,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: attacker },
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            },
        ],
        None,
    );
    evaluate_layers(runner.state_mut());
}

/// CR 702.19b + CR 510.1c: A 2/2 base creature buffed to 3/3 with GRANTED
/// trample, blocked by a 2/2, must assign lethal (2) to the blocker and trample
/// the buffed excess (1) over to the defending player.
///
/// This is the full-pipeline guard for issue #766: it proves combat damage reads
/// the LAYERED power (3, not the printed 2) via `combat_damage_amount`, the
/// LAYERED toughness of the blocker via `lethal_damage_needed`, and the GRANTED
/// `Keyword::Trample` via `has_keyword`.
#[test]
fn thoughtweft_buffed_trample_excess_reaches_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's base 2/2 attacker — printed stats, no inherent trample.
    let attacker = scenario.add_creature(P0, "Buffed Soldier", 2, 2).id();
    // P1's 2/2 blocker.
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();

    let mut runner = scenario.build();

    // Pre-buff sanity: base 2/2, no trample.
    {
        let obj = runner.state().objects.get(&attacker).unwrap();
        assert_eq!(obj.power, Some(2), "precondition: base power is 2");
        assert_eq!(obj.toughness, Some(2), "precondition: base toughness is 2");
        assert!(
            !obj.has_keyword(&Keyword::Trample),
            "precondition: attacker has no inherent trample"
        );
    }

    buff_attacker_like_thoughtweft(&mut runner, attacker);

    // CR 613.4c (layer 7c) + CR 702.19a: layers show the attacker at 3/3 with the
    // granted trample keyword before combat damage is assigned.
    {
        let obj = runner.state().objects.get(&attacker).unwrap();
        assert_eq!(
            obj.power,
            Some(3),
            "evaluate_layers must surface buffed power 3 (2 base + 1)"
        );
        assert_eq!(
            obj.toughness,
            Some(3),
            "evaluate_layers must surface buffed toughness 3 (2 base + 1)"
        );
        assert!(
            obj.has_keyword(&Keyword::Trample),
            "evaluate_layers must surface the granted Trample keyword"
        );
    }

    let attacker_life_before = runner.life(P1);
    assert_eq!(attacker_life_before, 20, "precondition: P1 starts at 20");

    // Declare the 3/3 trampler as attacker; P1 blocks with the 2/2.
    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);

    // CR 510.1c: the trample attacker assigns lethal (2 = the blocker's
    // toughness) to its single blocker before any excess is assigned. This is the
    // discriminating signal that the 3-power trampler split its damage correctly:
    // it marked the blocker with exactly lethal and pushed the remainder onward.
    let blocker_damage = runner
        .state()
        .objects
        .get(&blocker)
        .map(|b| b.damage_marked)
        .expect("blocker present immediately after the combat damage step");
    assert_eq!(
        blocker_damage, 2,
        "CR 510.1c: exactly lethal (2 = blocker toughness) is assigned to the 2/2 blocker"
    );

    // CR 702.19b: the buffed excess (3 power − 2 lethal = 1) tramples over to the
    // defending player. 20 → 19.
    assert_eq!(
        runner.life(P1),
        attacker_life_before - 1,
        "CR 702.19b: 1 trample excess from the buffed 3/3 must reach P1 (20 -> 19)"
    );

    // CR 704.5g: after SBAs run, the 2/2 blocker with 2 marked damage is
    // destroyed and moves to its owner's graveyard. The object record persists in
    // `state.objects` with an updated zone, so check the zone rather than key
    // presence.
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects.get(&blocker).map(|b| b.zone),
        Some(Zone::Graveyard),
        "CR 704.5g: the 2/2 blocker took 2 lethal damage and should be in the graveyard"
    );
}
