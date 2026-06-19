//! Hag of Noxious Nightmares — "Warlocks you control have menace."
//!
//! Regression coverage for the continuous static **keyword-grant** building
//! block (Layer 6 ability-adding effect, CR 613.1f) granting **menace**
//! (CR 702.111) across the filter axes the Oracle clause carries:
//!   - **subtype** — only Warlocks gain the keyword (CR 205.3m),
//!   - **"you control"** — opponents' Warlocks are excluded (CR 109.4),
//!   - **self-inclusion** — the source is itself a Warlock you control,
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

const HAG_NOXIOUS: &str = "Warlocks you control have menace.";

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn hag_grants_menace_to_your_warlocks_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a Warlock carrying the grant (real parse + synthesis pipeline).
    // It is itself a "Warlock you control".
    let hag = scenario
        .add_creature_from_oracle(P0, "Hag of Noxious Nightmares", 3, 3, HAG_NOXIOUS)
        .with_subtypes(vec!["Hag", "Warlock"])
        .id();

    // Another Warlock you control — gains menace.
    let ally_warlock = scenario
        .add_creature(P0, "Apprentice Warlock", 1, 1)
        .with_subtypes(vec!["Warlock"])
        .id();

    // A non-Warlock you control — outside the subtype filter.
    let ally_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    // An opponent's Warlock — outside the "you control" filter.
    let foe_warlock = scenario
        .add_creature(P1, "Rival Warlock", 1, 1)
        .with_subtypes(vec!["Warlock"])
        .id();

    let mut runner = scenario.build();

    // CR 613.1f: Warlocks you control (including the source) gain menace.
    assert!(
        has_kw(&mut runner, hag, &Keyword::Menace),
        "Hag is a Warlock you control and must have menace"
    );
    assert!(
        has_kw(&mut runner, ally_warlock, &Keyword::Menace),
        "another Warlock you control must gain menace"
    );

    // CR 205.3m: a non-Warlock you control is outside the subtype filter.
    assert!(
        !has_kw(&mut runner, ally_bear, &Keyword::Menace),
        "a non-Warlock you control must NOT gain menace"
    );

    // CR 109.4: "you control" excludes the opponent's Warlock.
    assert!(
        !has_kw(&mut runner, foe_warlock, &Keyword::Menace),
        "an opponent's Warlock must NOT gain menace ('you control')"
    );
}

#[test]
fn hag_menace_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hag = scenario
        .add_creature_from_oracle(P0, "Hag of Noxious Nightmares", 3, 3, HAG_NOXIOUS)
        .with_subtypes(vec!["Hag", "Warlock"])
        .id();
    let ally_warlock = scenario
        .add_creature(P0, "Apprentice Warlock", 1, 1)
        .with_subtypes(vec!["Warlock"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, ally_warlock, &Keyword::Menace),
        "baseline: ally Warlock has menace while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != hag);
        state.objects.remove(&hag);
    }
    assert!(
        !has_kw(&mut runner, ally_warlock, &Keyword::Menace),
        "ally Warlock must lose menace once the source is gone"
    );
}
