//! Roughshod Mentor — "Green creatures you control have trample."
//!
//! Regression coverage for the continuous static keyword-grant building block
//! (Layer 6 ability-adding effect, CR 613.1f) granting **trample** (CR 702.19)
//! on the **color** filter axis (green). Axes: color filter (only green
//! creatures), self-inclusion (the green source grants to itself), the "you
//! control" exclusion, and grant lifetime (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` keyword set — a runtime test, not an
//! AST-shape test. Colors are set via `with_mana_cost` (production color path).

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const ROUGHSHOD_MENTOR: &str = "Green creatures you control have trample.";

fn green() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    }
}

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn roughshod_mentor_grants_trample_to_green_creatures_you_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a green creature carrying the grant (real parse + synthesis
    // pipeline). It is green and you control it, so it grants to itself.
    let mentor = scenario
        .add_creature_from_oracle(P0, "Roughshod Mentor", 4, 3, ROUGHSHOD_MENTOR)
        .with_mana_cost(green())
        .with_subtypes(vec!["Beast"])
        .id();

    // Another GREEN creature you control — gains trample.
    let green_ally = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_mana_cost(green())
        .with_subtypes(vec!["Elf", "Druid"])
        .id();

    // A NON-green creature you control — outside the color filter.
    let colorless_ally = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .with_subtypes(vec!["Thopter"])
        .id();

    // An opponent's green creature — outside the "you control" filter.
    let green_foe = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_mana_cost(green())
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    // CR 613.1f: green creatures you control (including the source) gain trample.
    assert!(
        has_kw(&mut runner, mentor, &Keyword::Trample),
        "Roughshod Mentor is a green creature you control and must have trample"
    );
    assert!(
        has_kw(&mut runner, green_ally, &Keyword::Trample),
        "another green creature you control gains trample"
    );

    // CR 105.2: a non-green creature is outside the color filter.
    assert!(
        !has_kw(&mut runner, colorless_ally, &Keyword::Trample),
        "a non-green creature you control must NOT gain trample"
    );

    // CR 109.4: "you control" excludes the opponent's green creature.
    assert!(
        !has_kw(&mut runner, green_foe, &Keyword::Trample),
        "an opponent's green creature must NOT gain trample ('you control')"
    );
}

#[test]
fn roughshod_mentor_grant_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mentor = scenario
        .add_creature_from_oracle(P0, "Roughshod Mentor", 4, 3, ROUGHSHOD_MENTOR)
        .with_mana_cost(green())
        .with_subtypes(vec!["Beast"])
        .id();
    let green_ally = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_mana_cost(green())
        .with_subtypes(vec!["Elf", "Druid"])
        .id();

    let mut runner = scenario.build();
    assert!(
        has_kw(&mut runner, green_ally, &Keyword::Trample),
        "baseline: green ally has trample while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != mentor);
        state.objects.remove(&mentor);
    }
    assert!(
        !has_kw(&mut runner, green_ally, &Keyword::Trample),
        "green ally must lose trample once the source is gone"
    );
}
