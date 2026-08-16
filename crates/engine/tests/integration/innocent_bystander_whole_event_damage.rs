//! CR 120.4b: whole-event damage aggregation for a received-damage amount
//! threshold — Innocent Bystander, "Whenever this creature is dealt 3 or more
//! damage, investigate."
//!
//! Damage dealt simultaneously is dealt in ONE damage event (CR 120.4 processes
//! damage in a single four-part sequence, and the worked example printed under
//! CR 120.4d states a multi-source event as one bracketed entry), and CR 120.4b
//! puts damage triggers at that granularity. So two blockers each dealing 2 to
//! the Bystander is a single 4-damage event that fires the `>= 3` threshold
//! ONCE — not two 2-damage events that each fail it, and not two firings.
//! The card's own ruling says the same: it triggers "only if it's dealt 3 or
//! more damage all at once."
//!
//! The Bystander is the ATTACKER and the blockers are the damage sources, which
//! is the only combat shape that produces a multi-source simultaneous batch on
//! one recipient. Every row drives the standard shared spine
//! (`rules::run_combat` → `advance_until_stack_empty`); `GameRunner::combat_damage`
//! cannot be used, because two blockers raise `WaitingFor::AssignCombatDamage`
//! (`blocker_count >= 2`) and that pump breaks on any non-`Priority` state,
//! delivering no combat damage at all.

use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use super::rules::{run_combat, GameScenario, P0, P1};

/// Verbatim Oracle text, reminder text included — `/card-test` requires the
/// printed text, and the parser strips reminder text upstream.
const BYSTANDER: &str = "Whenever this creature is dealt 3 or more damage, investigate. (Create a Clue token. It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")";

/// Case-insensitive Clue count, copied from the in-repo investigate precedent
/// `issue_5159_attacks_alone_investigate.rs`.
fn count_clues_on_battlefield(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|obj| obj.name.eq_ignore_ascii_case("Clue"))
        })
        .count()
}

#[test]
fn two_sources_two_each_fires_once() {
    // V4 — two blockers each deal 2 in ONE event: 2 + 2 = 4 >= 3 fires once.
    // Revert-failing: with the old per-event check neither 2-damage event
    // clears the threshold and the result is 0 Clues (under-fire).
    let mut scenario = GameScenario::new();
    // `run_combat` assumes two priority passes reach DeclareAttackers;
    // `GameScenario::new()` starts at `Phase::Untap`, which does not satisfy it.
    scenario.at_phase(Phase::PreCombatMain);
    let bystander = scenario
        .add_creature_from_oracle(P0, "Innocent Bystander", 2, 1, BYSTANDER)
        .id();
    let blocker_a = scenario.add_creature(P1, "Blocker A", 2, 2).id();
    let blocker_b = scenario.add_creature(P1, "Blocker B", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![bystander],
        vec![(blocker_a, bystander), (blocker_b, bystander)],
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues_on_battlefield(&runner),
        1,
        "2 + 2 damage in one event is a single 4-damage event and must fire the >= 3 threshold once"
    );
}

#[test]
fn two_sources_three_each_fires_once_not_twice() {
    // V5 — both sources individually clear the threshold, so the once-per-batch
    // dedup is what keeps this at one firing (CR 603.2c).
    // Revert-failing: put the batch-dedup skip back on `trig_def.batched`
    // (which this class deliberately never sets) and the result is 2 Clues.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bystander = scenario
        .add_creature_from_oracle(P0, "Innocent Bystander", 2, 1, BYSTANDER)
        .id();
    let blocker_a = scenario.add_creature(P1, "Blocker A", 3, 3).id();
    let blocker_b = scenario.add_creature(P1, "Blocker B", 3, 3).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![bystander],
        vec![(blocker_a, bystander), (blocker_b, bystander)],
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues_on_battlefield(&runner),
        1,
        "one simultaneous event fires the trigger once even when each source alone clears it"
    );
}

#[test]
fn single_source_three_fires_once() {
    // Positive reach-guard for the whole file: proves the trigger is reachable
    // at all through this spine, so a 0-asserting row elsewhere cannot pass
    // merely because nothing ever fires.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bystander = scenario
        .add_creature_from_oracle(P0, "Innocent Bystander", 2, 1, BYSTANDER)
        .id();
    let blocker = scenario.add_creature(P1, "Lone Blocker", 3, 3).id();
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![bystander], vec![(blocker, bystander)]);
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues_on_battlefield(&runner),
        1,
        "a single 3-damage source clears the threshold on its own"
    );
}

#[test]
fn damage_at_different_times_does_not_fire() {
    // V6 — CR 506.1 / CR 702.7b: a first-striking blocker deals its 2 in the
    // first-strike sub-step and the other deals its 2 in the regular sub-step.
    // Those are two SEPARATE damage events, so neither reaches 3 and the
    // trigger must not fire. This is an invariance guard rather than a
    // revert-failing row: 0 Clues holds under both poles of the scope axis.
    // What it discriminates is a fold keyed on ACCUMULATED state instead of on
    // the batch — summing the recipient's `damage_marked` rather than each
    // candidate event's amount would return 1 Clue here while the control below
    // still returns 1.
    //
    // Indestructible (CR 702.12b) is load-bearing: a 2/1 with 2 damage marked
    // would be destroyed by the CR 704.5g state-based action at the next
    // priority (CR 704.3), and the second instance would never land — 0 Clues
    // would then be correct for a reason unrelated to batch scoping.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bystander = scenario
        .add_creature_from_oracle(P0, "Innocent Bystander", 2, 1, BYSTANDER)
        .with_keyword(Keyword::Indestructible)
        .id();
    let first_striker = scenario
        .add_creature(P1, "First Striker", 2, 2)
        .with_keyword(Keyword::FirstStrike)
        .id();
    let regular = scenario.add_creature(P1, "Regular Blocker", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![bystander],
        vec![(first_striker, bystander), (regular, bystander)],
    );
    runner.advance_until_stack_empty();

    // Reachability, asserted rather than argued. Both known failure modes for
    // this row (recipient death, and a driver that stalls at
    // `AssignCombatDamage`) leave 2 marked damage, not 4.
    assert!(
        runner
            .battlefield_names()
            .iter()
            .any(|name| name == "Innocent Bystander"),
        "indestructible recipient must survive both damage sub-steps"
    );
    assert_eq!(
        runner.state().objects[&bystander].damage_marked, 4,
        "both 2-damage instances must actually land — this is what makes the 0 below non-vacuous"
    );

    assert_eq!(
        count_clues_on_battlefield(&runner),
        0,
        "damage dealt in two different sub-steps is two events; neither reaches the >= 3 threshold"
    );
}

#[test]
fn damage_at_different_times_control_same_event_does_fire() {
    // V6b — the A/B control for the row above. Same indestructible Bystander,
    // same two 2-power blockers, same 4 total damage; ONLY the timing differs
    // (no first strike ⇒ one event). If this ever returns 0, the row above is
    // vacuous.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bystander = scenario
        .add_creature_from_oracle(P0, "Innocent Bystander", 2, 1, BYSTANDER)
        .with_keyword(Keyword::Indestructible)
        .id();
    let blocker_a = scenario.add_creature(P1, "Blocker A", 2, 2).id();
    let blocker_b = scenario.add_creature(P1, "Blocker B", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![bystander],
        vec![(blocker_a, bystander), (blocker_b, bystander)],
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bystander].damage_marked, 4,
        "same total damage as the different-times row"
    );
    assert_eq!(
        count_clues_on_battlefield(&runner),
        1,
        "the same creatures and the same total damage in ONE event do fire the trigger"
    );
}

#[test]
fn two_bystanders_each_fire_once() {
    // H5 — grouping is keyed on the RECIPIENT (`TargetRef`), while the
    // once-per-batch dedup is keyed on the trigger SOURCE. Two distinct
    // Bystanders therefore hold distinct dedup keys and neither suppresses the
    // other. Two same-controller triggers raise the CR 603.3b ordering prompt,
    // which only `advance_until_stack_empty` drains — `run_combat` exits on it.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bystander_a = scenario
        .add_creature_from_oracle(P0, "Innocent Bystander", 2, 1, BYSTANDER)
        .with_keyword(Keyword::Indestructible)
        .id();
    let bystander_b = scenario
        .add_creature_from_oracle(P0, "Innocent Bystander", 2, 1, BYSTANDER)
        .with_keyword(Keyword::Indestructible)
        .id();
    let a1 = scenario.add_creature(P1, "Blocker A1", 2, 2).id();
    let a2 = scenario.add_creature(P1, "Blocker A2", 2, 2).id();
    let b1 = scenario.add_creature(P1, "Blocker B1", 2, 2).id();
    let b2 = scenario.add_creature(P1, "Blocker B2", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![bystander_a, bystander_b],
        vec![
            (a1, bystander_a),
            (a2, bystander_a),
            (b1, bystander_b),
            (b2, bystander_b),
        ],
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bystander_a].damage_marked, 4,
        "first Bystander must take its whole batch"
    );
    assert_eq!(
        runner.state().objects[&bystander_b].damage_marked, 4,
        "second Bystander must take its whole batch"
    );
    assert_eq!(
        count_clues_on_battlefield(&runner),
        2,
        "each Bystander fires exactly once — the dedup key is the trigger source, not the batch"
    );
}
