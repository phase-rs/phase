//! True Conviction — "Creatures you control have double strike and lifelink."
//! (an enchantment)
//!
//! Regression coverage for the continuous static **keyword-grant** building
//! block (Layer 6 ability-adding effect, CR 613.1f) where a single Oracle
//! clause grants TWO keywords conjoined ("X and Y"). Axes:
//!   - **conjunction** — both double strike (CR 702.4) and lifelink
//!     (CR 702.15) are granted from one clause,
//!   - **non-creature source** — the grant comes from an enchantment,
//!   - **"you control"** — opponents' creatures are excluded (CR 109.4),
//!   - **lifetime** — both grants end when the source leaves (CR 611.3).
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

const TRUE_CONVICTION: &str = "Creatures you control have double strike and lifelink.";

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn true_conviction_grants_both_double_strike_and_lifelink() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: an enchantment carrying the conjoined grant (real parse +
    // synthesis pipeline, then flipped to an enchantment permanent).
    let _conviction = scenario
        .add_creature_from_oracle(P0, "True Conviction", 0, 0, TRUE_CONVICTION)
        .as_enchantment()
        .id();

    // A creature you control — gains BOTH keywords.
    let your_creature = scenario
        .add_creature(P0, "Serra Angel", 4, 4)
        .with_subtypes(vec!["Angel"])
        .id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    // CR 613.1f: the single clause grants both keywords to creatures you control.
    assert!(
        has_kw(&mut runner, your_creature, &Keyword::DoubleStrike),
        "a creature you control must gain double strike"
    );
    assert!(
        has_kw(&mut runner, your_creature, &Keyword::Lifelink),
        "the SAME creature must ALSO gain lifelink (conjoined grant)"
    );

    // CR 109.4: "you control" excludes the opponent's creature for both.
    assert!(
        !has_kw(&mut runner, foe, &Keyword::DoubleStrike),
        "an opponent's creature must NOT gain double strike"
    );
    assert!(
        !has_kw(&mut runner, foe, &Keyword::Lifelink),
        "an opponent's creature must NOT gain lifelink"
    );
}

#[test]
fn true_conviction_both_grants_turn_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let conviction = scenario
        .add_creature_from_oracle(P0, "True Conviction", 0, 0, TRUE_CONVICTION)
        .as_enchantment()
        .id();
    let your_creature = scenario
        .add_creature(P0, "Serra Angel", 4, 4)
        .with_subtypes(vec!["Angel"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, your_creature, &Keyword::DoubleStrike)
            && has_kw(&mut runner, your_creature, &Keyword::Lifelink),
        "baseline: your creature has both keywords while the source is present"
    );

    // CR 611.3: both continuous effects end when the source leaves.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != conviction);
        state.objects.remove(&conviction);
    }
    assert!(
        !has_kw(&mut runner, your_creature, &Keyword::DoubleStrike)
            && !has_kw(&mut runner, your_creature, &Keyword::Lifelink),
        "your creature must lose BOTH keywords once the source is gone"
    );
}
