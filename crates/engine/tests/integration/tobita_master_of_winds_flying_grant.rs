//! Tobita, Master of Winds — "Creatures you control have flying."
//!
//! Regression coverage for the continuous static **keyword-grant** building
//! block (Layer 6 ability-adding effect, CR 613.1f) granting **flying**
//! (CR 702.9) on the *controller-only* filter axis — the clause has NO subtype
//! or color restriction, so it reaches every creature you control. Axes:
//!   - **controller-only** — all creatures you control gain flying, with no
//!     type narrowing (CR 109.4),
//!   - **"you control"** — opponents' creatures are excluded,
//!   - **self-inclusion** — the source is itself a creature you control,
//!   - **lifetime** — the grant ends when the source leaves (CR 611.3).
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

const TOBITA: &str = "Creatures you control have flying.";

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn tobita_grants_flying_to_all_your_creatures_no_type_filter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a creature carrying the grant (real parse + synthesis pipeline).
    // It is itself a creature you control.
    let tobita = scenario
        .add_creature_from_oracle(P0, "Tobita, Master of Winds", 3, 3, TOBITA)
        .with_subtypes(vec!["Human", "Monk"])
        .id();

    // Two unrelated creatures you control, different subtypes — both gain flying
    // (no subtype/color narrowing).
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

    // CR 613.1f: every creature you control (including the source) gains flying.
    assert!(
        has_kw(&mut runner, tobita, &Keyword::Flying),
        "Tobita is a creature you control and must have flying"
    );
    assert!(
        has_kw(&mut runner, your_bear, &Keyword::Flying),
        "a creature you control gains flying (no subtype filter)"
    );
    assert!(
        has_kw(&mut runner, your_goblin, &Keyword::Flying),
        "another creature you control of a different subtype also gains flying"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert!(
        !has_kw(&mut runner, foe, &Keyword::Flying),
        "an opponent's creature must NOT gain flying"
    );
}

#[test]
fn tobita_flying_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tobita = scenario
        .add_creature_from_oracle(P0, "Tobita, Master of Winds", 3, 3, TOBITA)
        .id();
    let your_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, your_bear, &Keyword::Flying),
        "baseline: your creature has flying while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != tobita);
        state.objects.remove(&tobita);
    }
    assert!(
        !has_kw(&mut runner, your_bear, &Keyword::Flying),
        "your creature must lose flying once the source is gone"
    );
}
