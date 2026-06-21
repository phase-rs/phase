//! Selenia — "Creatures you control have vigilance."
//!
//! Regression coverage for the continuous static keyword-grant building block
//! (Layer 6 ability-adding effect, CR 613.1f) granting **vigilance**
//! (CR 702.20) on the controller-only filter axis from a creature source that
//! grants to itself. Axes: controller-only (no subtype narrowing),
//! self-inclusion, the "you control" exclusion, and grant lifetime (CR 611.3).
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

const SELENIA: &str = "Creatures you control have vigilance.";

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn selenia_grants_vigilance_to_all_your_creatures_including_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a creature carrying the grant (real parse + synthesis pipeline).
    // It is itself a creature you control.
    let selenia = scenario
        .add_creature_from_oracle(P0, "Selenia", 3, 3, SELENIA)
        .with_subtypes(vec!["Angel"])
        .id();

    // Another creature you control — gains vigilance.
    let your_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    assert!(
        has_kw(&mut runner, selenia, &Keyword::Vigilance),
        "Selenia is a creature you control and must have vigilance"
    );
    assert!(
        has_kw(&mut runner, your_bear, &Keyword::Vigilance),
        "another creature you control gains vigilance"
    );
    assert!(
        !has_kw(&mut runner, foe, &Keyword::Vigilance),
        "an opponent's creature must NOT gain vigilance ('you control')"
    );
}

#[test]
fn selenia_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let selenia = scenario
        .add_creature_from_oracle(P0, "Selenia", 3, 3, SELENIA)
        .with_subtypes(vec!["Angel"])
        .id();
    let your_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, your_bear, &Keyword::Vigilance),
        "baseline: your creature has vigilance while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != selenia);
        state.objects.remove(&selenia);
    }
    assert!(
        !has_kw(&mut runner, your_bear, &Keyword::Vigilance),
        "your creature must lose vigilance once the source is gone"
    );
}
