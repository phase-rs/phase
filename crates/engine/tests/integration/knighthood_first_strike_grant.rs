//! Knighthood — "Creatures you control have first strike." (an enchantment)
//!
//! Regression coverage for the continuous static **keyword-grant** building
//! block (Layer 6 ability-adding effect, CR 613.1f) granting **first strike**
//! (CR 702.7) from a NON-creature source (an enchantment), on the
//! controller-only filter axis. Axes:
//!   - **non-creature source** — the grant comes from an enchantment, not a
//!     creature lord (the source itself never gains the keyword),
//!   - **controller-only** — all creatures you control gain first strike,
//!   - **"you control"** — opponents' creatures are excluded (CR 109.4),
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

const KNIGHTHOOD: &str = "Creatures you control have first strike.";

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn knighthood_enchantment_grants_first_strike_to_your_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: an ENCHANTMENT carrying the grant (built through the real parse +
    // synthesis pipeline, then flipped to an enchantment permanent). Its id is
    // not referenced again in this test — the grant is read off the creatures.
    let _knighthood = scenario
        .add_creature_from_oracle(P0, "Knighthood", 0, 0, KNIGHTHOOD)
        .as_enchantment()
        .id();

    // A creature you control — gains first strike.
    let your_knight = scenario
        .add_creature(P0, "Benalish Knight", 2, 2)
        .with_subtypes(vec!["Human", "Knight"])
        .id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    // CR 613.1f: a creature you control gains first strike from the enchantment.
    assert!(
        has_kw(&mut runner, your_knight, &Keyword::FirstStrike),
        "a creature you control must gain first strike from Knighthood"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert!(
        !has_kw(&mut runner, foe, &Keyword::FirstStrike),
        "an opponent's creature must NOT gain first strike"
    );
}

#[test]
fn knighthood_first_strike_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let knighthood = scenario
        .add_creature_from_oracle(P0, "Knighthood", 0, 0, KNIGHTHOOD)
        .as_enchantment()
        .id();
    let your_knight = scenario
        .add_creature(P0, "Benalish Knight", 2, 2)
        .with_subtypes(vec!["Human", "Knight"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, your_knight, &Keyword::FirstStrike),
        "baseline: your creature has first strike while the enchantment is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != knighthood);
        state.objects.remove(&knighthood);
    }
    assert!(
        !has_kw(&mut runner, your_knight, &Keyword::FirstStrike),
        "your creature must lose first strike once the enchantment is gone"
    );
}
