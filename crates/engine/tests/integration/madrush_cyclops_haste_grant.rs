//! Madrush Cyclops — "Creatures you control have haste."
//!
//! Regression coverage for the continuous static **keyword-grant** building
//! block (Layer 6 ability-adding effect, CR 613.1f) granting **haste**
//! (CR 702.10) on the controller-only filter axis. Axes:
//!   - **controller-only** — every creature you control gains haste, with no
//!     subtype/color narrowing,
//!   - **"you control"** — opponents' creatures are excluded (CR 109.4),
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

const MADRUSH_CYCLOPS: &str = "Creatures you control have haste.";

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn madrush_cyclops_grants_haste_to_all_your_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a creature carrying the grant (real parse + synthesis pipeline).
    // It is itself a creature you control.
    let madrush = scenario
        .add_creature_from_oracle(P0, "Madrush Cyclops", 4, 5, MADRUSH_CYCLOPS)
        .with_subtypes(vec!["Cyclops", "Warrior"])
        .id();

    // Two creatures you control of different subtypes — both gain haste.
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

    // CR 613.1f: every creature you control (including the source) gains haste.
    assert!(
        has_kw(&mut runner, madrush, &Keyword::Haste),
        "Madrush Cyclops is a creature you control and must have haste"
    );
    assert!(
        has_kw(&mut runner, your_bear, &Keyword::Haste),
        "a creature you control gains haste (no subtype filter)"
    );
    assert!(
        has_kw(&mut runner, your_goblin, &Keyword::Haste),
        "another creature you control of a different subtype also gains haste"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert!(
        !has_kw(&mut runner, foe, &Keyword::Haste),
        "an opponent's creature must NOT gain haste"
    );
}

#[test]
fn madrush_cyclops_haste_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let madrush = scenario
        .add_creature_from_oracle(P0, "Madrush Cyclops", 4, 5, MADRUSH_CYCLOPS)
        .id();
    let your_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, your_bear, &Keyword::Haste),
        "baseline: your creature has haste while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != madrush);
        state.objects.remove(&madrush);
    }
    assert!(
        !has_kw(&mut runner, your_bear, &Keyword::Haste),
        "your creature must lose haste once the source is gone"
    );
}
