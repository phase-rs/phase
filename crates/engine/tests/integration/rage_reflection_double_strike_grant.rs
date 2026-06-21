//! Rage Reflection — "Creatures you control have double strike." (an enchantment)
//!
//! Regression coverage for the continuous static keyword-grant building block
//! (Layer 6 ability-adding effect, CR 613.1f) granting **double strike**
//! (CR 702.4) on the controller-only filter axis from a NON-creature source.
//! Axes: controller-only (no subtype narrowing), the "you control" exclusion,
//! non-creature source, and grant lifetime (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` keyword set — a runtime test, not an
//! AST-shape test.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const RAGE_REFLECTION: &str = "Creatures you control have double strike.";

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn rage_reflection_grants_double_strike_to_your_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: an enchantment carrying the grant (real parse + synthesis pipeline,
    // then flipped to an enchantment permanent).
    let _reflection = scenario
        .add_creature_from_oracle(P0, "Rage Reflection", 0, 0, RAGE_REFLECTION)
        .as_enchantment()
        .id();

    // Two creatures you control of different subtypes — both gain double strike.
    let your_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let your_goblin = scenario
        .add_creature(P0, "Raging Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    assert!(
        has_kw(&mut runner, your_bear, &Keyword::DoubleStrike),
        "a creature you control gains double strike (no subtype filter)"
    );
    assert!(
        has_kw(&mut runner, your_goblin, &Keyword::DoubleStrike),
        "another creature you control of a different subtype also gains it"
    );
    assert!(
        !has_kw(&mut runner, foe, &Keyword::DoubleStrike),
        "an opponent's creature must NOT gain double strike ('you control')"
    );
}

#[test]
fn rage_reflection_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let reflection = scenario
        .add_creature_from_oracle(P0, "Rage Reflection", 0, 0, RAGE_REFLECTION)
        .as_enchantment()
        .id();
    let your_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, your_bear, &Keyword::DoubleStrike),
        "baseline: your creature has double strike while the enchantment is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != reflection);
        state.objects.remove(&reflection);
    }
    assert!(
        !has_kw(&mut runner, your_bear, &Keyword::DoubleStrike),
        "your creature must lose double strike once the enchantment is gone"
    );
}
