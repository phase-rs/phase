//! Fury Sliver — "All Sliver creatures have double strike."
//!
//! Regression coverage for the continuous static **keyword-grant** building
//! block (Layer 6 ability-adding effect, CR 613.1f) on the *uncontrolled*
//! filter axis: "All Sliver creatures" carries NO controller restriction, so
//! the grant reaches Slivers controlled by ANY player (CR 109.4 — the absence
//! of a controller clause is the discriminator against Sentinel Sliver's
//! "Sliver creatures you control"). Filter axes exercised:
//!   - **subtype** — only Slivers gain the keyword (CR 205.3m),
//!   - **no controller filter** — opponents' Slivers gain it too,
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

const FURY_SLIVER: &str = "All Sliver creatures have double strike.";

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn fury_sliver_grants_double_strike_to_all_slivers_any_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a Sliver carrying the grant (built through the real parse +
    // synthesis pipeline). It is itself a Sliver creature.
    let fury = scenario
        .add_creature_from_oracle(P0, "Fury Sliver", 3, 3, FURY_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();

    // Another Sliver you control — gains double strike.
    let ally_sliver = scenario
        .add_creature(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    // An opponent's Sliver — STILL gains double strike (no controller filter).
    let foe_sliver = scenario
        .add_creature(P1, "Plated Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    // A non-Sliver — outside the subtype filter, regardless of controller.
    let foe_bear = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    // CR 613.1f: every Sliver creature gains double strike, including the source.
    assert!(
        has_kw(&mut runner, fury, &Keyword::DoubleStrike),
        "Fury Sliver is a Sliver and must have double strike"
    );
    assert!(
        has_kw(&mut runner, ally_sliver, &Keyword::DoubleStrike),
        "a Sliver you control must gain double strike"
    );

    // CR 109.4: "All Sliver creatures" has no controller clause — the opponent's
    // Sliver gains it too. This discriminates against a "you control" grant.
    assert!(
        has_kw(&mut runner, foe_sliver, &Keyword::DoubleStrike),
        "an opponent's Sliver must ALSO gain double strike ('All Sliver creatures')"
    );

    // CR 205.3m: a non-Sliver is outside the subtype filter.
    assert!(
        !has_kw(&mut runner, foe_bear, &Keyword::DoubleStrike),
        "a non-Sliver must NOT gain double strike"
    );
}

#[test]
fn fury_sliver_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let fury = scenario
        .add_creature_from_oracle(P0, "Fury Sliver", 3, 3, FURY_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();
    let foe_sliver = scenario
        .add_creature(P1, "Plated Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, foe_sliver, &Keyword::DoubleStrike),
        "baseline: opponent Sliver has double strike while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != fury);
        state.objects.remove(&fury);
    }
    assert!(
        !has_kw(&mut runner, foe_sliver, &Keyword::DoubleStrike),
        "opponent Sliver must lose double strike once the source is gone"
    );
}
