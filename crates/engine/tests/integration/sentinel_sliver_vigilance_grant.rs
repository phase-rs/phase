//! Sentinel Sliver — "Sliver creatures you control have vigilance."
//!
//! Regression coverage for the continuous static **keyword-grant** building
//! block (Layer 6 ability-adding effect, CR 613.1f) exercised across the filter
//! axes the Oracle clause carries:
//!   - **subtype** — only Slivers gain the keyword (CR 205.3m),
//!   - **"you control"** — opponents' Slivers are excluded (CR 109.4),
//!   - **self-inclusion** — the source is itself a Sliver you control, so it
//!     gains the keyword too,
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

const SENTINEL_SLIVER: &str =
    "Sliver creatures you control have vigilance. (Attacking doesn't cause them to tap.)";

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn sentinel_sliver_grants_vigilance_to_your_slivers_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a Sliver carrying the grant (built through the real parse +
    // synthesis pipeline). It is itself a "Sliver creature you control".
    let sentinel = scenario
        .add_creature_from_oracle(P0, "Sentinel Sliver", 2, 1, SENTINEL_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();

    // Another Sliver you control — gains vigilance.
    let ally_sliver = scenario
        .add_creature(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    // A non-Sliver you control — outside the subtype filter.
    let ally_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    // An opponent's Sliver — outside the "you control" filter.
    let foe_sliver = scenario
        .add_creature(P1, "Plated Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    let mut runner = scenario.build();

    // CR 613.1f: Slivers you control (including the source) gain vigilance.
    assert!(
        has_kw(&mut runner, sentinel, &Keyword::Vigilance),
        "Sentinel Sliver is a Sliver you control and must have vigilance"
    );
    assert!(
        has_kw(&mut runner, ally_sliver, &Keyword::Vigilance),
        "another Sliver you control must gain vigilance"
    );

    // CR 205.3m: a non-Sliver you control is outside the subtype filter.
    assert!(
        !has_kw(&mut runner, ally_bear, &Keyword::Vigilance),
        "a non-Sliver you control must NOT gain vigilance"
    );

    // CR 109.4: "you control" excludes the opponent's Sliver.
    assert!(
        !has_kw(&mut runner, foe_sliver, &Keyword::Vigilance),
        "an opponent's Sliver must NOT gain vigilance ('you control')"
    );
}

#[test]
fn sentinel_sliver_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sentinel = scenario
        .add_creature_from_oracle(P0, "Sentinel Sliver", 2, 1, SENTINEL_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();
    let ally_sliver = scenario
        .add_creature(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, ally_sliver, &Keyword::Vigilance),
        "baseline: ally Sliver has vigilance while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != sentinel);
        state.objects.remove(&sentinel);
    }
    assert!(
        !has_kw(&mut runner, ally_sliver, &Keyword::Vigilance),
        "ally Sliver must lose vigilance once the source is gone"
    );
}
