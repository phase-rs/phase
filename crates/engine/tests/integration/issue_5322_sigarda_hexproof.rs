//! Regression for issue #5322: Sigarda, Heron's Grace must grant hexproof only to
//! Humans you control — not to every creature you control, and not to Sigarda
//! herself (she is an Angel, not a Human).
//!
//! https://github.com/phase-rs/phase/issues/5322

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const SIGARDA_ORACLE: &str = "Flying\nYou and Humans you control have hexproof.\n{2}, Exile a card from your graveyard: Create a 1/1 white Human Soldier token.";

fn has_creature_hexproof(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], &Keyword::Hexproof)
}

#[test]
fn sigarda_grants_hexproof_to_humans_not_all_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sigarda = scenario
        .add_creature_from_oracle(P0, "Sigarda, Heron's Grace", 4, 5, SIGARDA_ORACLE)
        .with_subtypes(vec!["Angel"])
        .id();
    let human = scenario
        .add_creature(P0, "Human Soldier", 2, 2)
        .with_subtypes(vec!["Human"])
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();

    assert!(
        has_creature_hexproof(&mut runner, human),
        "Humans you control must have hexproof from Sigarda"
    );
    assert!(
        !has_creature_hexproof(&mut runner, bear),
        "non-Human creatures must NOT receive Sigarda's creature hexproof"
    );
    assert!(
        !has_creature_hexproof(&mut runner, sigarda),
        "Sigarda (Angel, not Human) must NOT receive creature hexproof from the Human filter"
    );
}
