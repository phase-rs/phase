//! Issue #5328: the "attacks alone" mechanic must be evaluated relative to the
//! creature that attacked, not the ability's source. When the observer (here
//! "Agent 13, Sharon Carter") stays back and a DIFFERENT creature you control
//! attacks alone, the trigger must still fire; when two creatures attack, it
//! must not.
//!
//! https://github.com/phase-rs/phase/issues/5328
//!
//! Root cause: `TriggerCondition::MinCoAttackers` excluded the ability SOURCE
//! from the co-attacker tally. For a non-attacking observer, `source_id` is not
//! in combat, so the lone attacker stayed counted and `Not(MinCoAttackers{1})`
//! evaluated false — the "attacks alone" ability silently failed to fire.
//! CR 506.5 + CR 702.83b: a creature attacks alone iff it is the only creature
//! declared as an attacker.

use engine::types::phase::Phase;

use super::rules::{run_combat, GameRunner, GameScenario, P0};

const ORACLE: &str = "Whenever a creature you control attacks alone, investigate.";

fn count_clues(runner: &GameRunner) -> usize {
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

/// Clues produced with the observer on the battlefield and `num_others`
/// non-observer attackers (the observer never attacks).
fn clues_when_others_attack(num_others: usize) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The observer stays home; only the "other" creatures attack.
    scenario.add_creature_from_oracle(P0, "Agent 13, Sharon Carter", 2, 2, ORACLE);
    let attackers: Vec<_> = (0..num_others)
        .map(|i| scenario.add_creature(P0, &format!("Bear{i}"), 2, 2).id())
        .collect();
    let mut runner = scenario.build();

    run_combat(&mut runner, attackers, vec![]);
    runner.advance_until_stack_empty();
    count_clues(&runner)
}

#[test]
fn other_creature_attacks_alone_fires_even_when_observer_stays_back() {
    // The bug: without the fix this is 0 — the lone attacker is wrongly counted
    // as a co-attacker of the non-attacking observer, so the trigger never fires.
    assert_eq!(
        clues_when_others_attack(1),
        1,
        "CR 506.5: a single non-observer attacker attacks alone → investigate once"
    );
}

#[test]
fn two_other_creatures_attacking_do_not_fire_attacks_alone() {
    // Positive control on the opposite side: two attackers is NOT attacking
    // alone, so the ability must stay silent.
    assert_eq!(
        clues_when_others_attack(2),
        0,
        "CR 506.5: two attackers is not 'attacks alone' → no investigate"
    );
}
