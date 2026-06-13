//! Issue #1428 — Crossway Troublemakers.
//!
//! Crossway Troublemakers has a static ability: "Attacking Vampires you control
//! have deathtouch and lifelink."
//!
//! The bug was that deathtouch and lifelink only appeared on attacking Vampires
//! after combat damage was dealt, instead of being active for the entire combat
//! phase. This happened because `FilterProp::Attacking { defender: None }` wasn't re-evaluated
//! until after damage assignment.
//!
//! The fix adds `state.layers_dirty = true` after populating `combat.attackers`
//! in `declare_attackers`, ensuring continuous effects with `FilterProp::Attacking { defender: None }`
//! are re-evaluated immediately when creatures become attackers.
//!
//! CR 506.4: A creature is "attacking" from when it is declared as an attacker
//! until it leaves combat or combat ends.
//! CR 702.2c: Deathtouch - any nonzero damage is lethal.
//! CR 702.15b: Lifelink - damage causes controller to gain that much life.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

/// Crossway Troublemakers static ability line.
const CROSSWAY_TROUBLEMAKERS: &str = "Attacking Vampires you control have deathtouch and lifelink.";

/// Drive the runner from a PreCombatMain start through declaring `attacker` as
/// an attacker, then proceed to the Declare Blockers step.
fn declare_attacker(runner: &mut engine::game::scenario::GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
}

/// CR 506.4 + CR 702.2c: An attacking Vampire should have deathtouch immediately
/// after being declared as an attacker, before damage assignment.
#[test]
fn crossway_troublemakers_grants_deathtouch_to_attacking_vampire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _troublemakers = scenario
        .add_creature_from_oracle(P0, "Crossway Troublemakers", 2, 2, CROSSWAY_TROUBLEMAKERS)
        .id();
    let vampire = scenario
        .add_creature(P0, "Vampire", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();

    let mut runner = scenario.build();
    declare_attacker(&mut runner, vampire);

    let obj = &runner.state().objects[&vampire];
    assert!(
        obj.has_keyword(&Keyword::Deathtouch),
        "Attacking Vampire should have deathtouch immediately after declaration"
    );
}

/// CR 506.4 + CR 702.15b: An attacking Vampire should have lifelink immediately
/// after being declared as an attacker, before damage assignment.
#[test]
fn crossway_troublemakers_grants_lifelink_to_attacking_vampire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _troublemakers = scenario
        .add_creature_from_oracle(P0, "Crossway Troublemakers", 2, 2, CROSSWAY_TROUBLEMAKERS)
        .id();
    let vampire = scenario
        .add_creature(P0, "Vampire", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();

    let mut runner = scenario.build();
    declare_attacker(&mut runner, vampire);

    let obj = &runner.state().objects[&vampire];
    assert!(
        obj.has_keyword(&Keyword::Lifelink),
        "Attacking Vampire should have lifelink immediately after declaration"
    );
}

/// CR 506.4: A non-attacking Vampire should NOT have deathtouch/lifelink.
#[test]
fn crossway_troublemakers_does_not_grant_keywords_to_non_attacking_vampire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _troublemakers = scenario
        .add_creature_from_oracle(P0, "Crossway Troublemakers", 2, 2, CROSSWAY_TROUBLEMAKERS)
        .id();
    let vampire = scenario
        .add_creature(P0, "Vampire", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();

    let mut runner = scenario.build();
    // Don't attack - just pass through combat
    runner.pass_both_players();
    runner.pass_both_players();

    let obj = &runner.state().objects[&vampire];
    assert!(
        !obj.has_keyword(&Keyword::Deathtouch),
        "Non-attacking Vampire should not have deathtouch"
    );
    assert!(
        !obj.has_keyword(&Keyword::Lifelink),
        "Non-attacking Vampire should not have lifelink"
    );
}

/// CR 506.4: A non-Vampire attacker should NOT receive deathtouch/lifelink.
#[test]
fn crossway_troublemakers_does_not_grant_keywords_to_non_vampire_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _troublemakers = scenario
        .add_creature_from_oracle(P0, "Crossway Troublemakers", 2, 2, CROSSWAY_TROUBLEMAKERS)
        .id();
    let non_vampire = scenario.add_creature(P0, "Human", 2, 2).id();

    let mut runner = scenario.build();
    declare_attacker(&mut runner, non_vampire);

    let obj = &runner.state().objects[&non_vampire];
    assert!(
        !obj.has_keyword(&Keyword::Deathtouch),
        "Non-Vampire attacker should not have deathtouch"
    );
    assert!(
        !obj.has_keyword(&Keyword::Lifelink),
        "Non-Vampire attacker should not have lifelink"
    );
}
