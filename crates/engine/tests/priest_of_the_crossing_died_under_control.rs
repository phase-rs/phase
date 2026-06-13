//! Discriminating regression test for **issue #1129**: a count phrased
//! "creatures that died **under your control** this turn" must count only the
//! source controller's deaths, not every player's.
//!
//! Priest of the Crossing's end-step trigger:
//!
//! > At the beginning of each end step, put X +1/+1 counters on each creature
//! > you control, where X is the number of creatures that died under your
//! > control this turn.
//!
//! Confirmed parse (`client/public/card-data.json` → "priest of the crossing"):
//! the trigger's `execute` is `Effect::PutCounterAll { counter_type: P1P1,
//! count: ZoneChangeCountThisTurn { from: Battlefield, to: Graveyard, filter:
//! Typed(Creature) }, target: Typed(Creature, controller: You) }`. Before the
//! fix the `count` filter's `controller` was `null`, so X counted EVERY
//! player's deaths. After the fix the parser stamps `controller: You` on the
//! count filter, and the runtime (`game::filter::zone_change_filter_inner`)
//! scopes the count to the source controller's deaths.
//!
//! Setup: P0 controls Priest plus a surviving creature; P0 and P1 each control
//! a creature that dies this turn. Resolving the trigger under P0 must place
//! exactly ONE +1/+1 counter on P0's surviving creature (only P0's death),
//! NOT two. With the fix reverted the count would be 2 and the assertion fails.
//!
//! CR 700.4: "dies" = put into a graveyard from the battlefield.
//! CR 109.5: "your"/"under your control" = the source's controller.
//! CR 122.1: +1/+1 counters placed by the resolving ability.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PRIEST_ORACLE: &str = "Flying\n\
At the beginning of each end step, put X +1/+1 counters on each creature you control, where X is the number of creatures that died under your control this turn.";

#[test]
fn priest_counts_only_creatures_that_died_under_your_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Priest of the Crossing under P0's control, parsed from real Oracle text.
    let priest = scenario
        .add_creature_from_oracle(P0, "Priest of the Crossing", 3, 3, PRIEST_ORACLE)
        .id();

    // A surviving P0 creature that will RECEIVE the +1/+1 counters.
    let survivor = scenario.add_creature(P0, "P0 Survivor", 2, 2).id();

    // One creature dies under P0's control (counts) and one under P1's control
    // (the negative control — must NOT count toward "under your control").
    let my_dead = scenario.add_creature(P0, "P0 Casualty", 1, 1).id();
    let their_dead = scenario.add_creature(P1, "P1 Casualty", 1, 1).id();

    let mut runner = scenario.build();

    // Drive both deaths through the production zone-change recorder so
    // `zone_changes_this_turn` is populated exactly as in a real game.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), my_dead, Zone::Graveyard, &mut events);
    move_to_zone(runner.state_mut(), their_dead, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().zone_changes_this_turn.len(),
        2,
        "sanity: both deaths must be recorded"
    );

    // Build the end-step trigger's execute ability (the count-and-place effect,
    // parsed standalone) and resolve it under P0 — same parser path the real
    // card uses, so the count's `controller` scope is exercised end-to-end.
    let execute_def = parse_effect_chain(
        "Put X +1/+1 counters on each creature you control, where X is the number \
         of creatures that died under your control this turn.",
        AbilityKind::Spell,
    );

    let ability = build_resolved_from_def(&execute_def, priest, P0);

    events.clear();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Priest end-step trigger must resolve");

    let counters = runner
        .state()
        .objects
        .get(&survivor)
        .expect("survivor still on battlefield")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);

    // DISCRIMINATOR (#1129): only P0's death is "under your control", so X = 1.
    // With the controller scope dropped (the bug), X would be 2.
    assert_eq!(
        counters, 1,
        "X must count only creatures that died under P0's control (1), not all \
         players' deaths (2); got {counters}"
    );
}
