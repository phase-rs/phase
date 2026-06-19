//! Regression for issue #3650: Grievous Wound must prevent its enchanted player
//! from gaining life.
//!
//! https://github.com/phase-rs/phase/issues/3650

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::static_abilities::player_has_cant_gain_life;
use engine::types::phase::Phase;

const GRIEVOUS_WOUND: &str = "Enchant player\n\
Enchanted player can't gain life.\n\
Whenever enchanted player is dealt damage, they lose half their life, rounded up.";

/// CR 119.7 + CR 303.4e: Grievous Wound's CantGainLife static must apply only
/// to the enchanted player.
#[test]
fn grievous_wound_prevents_enchanted_player_from_gaining_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aura = scenario
        .add_creature(P0, "Grievous Wound", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(GRIEVOUS_WOUND)
        .id();

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), aura, P1);
    evaluate_layers(runner.state_mut());

    assert!(
        player_has_cant_gain_life(runner.state(), P1),
        "Grievous Wound must stop its enchanted player from gaining life"
    );
    assert!(
        !player_has_cant_gain_life(runner.state(), P0),
        "Grievous Wound must not stop other players from gaining life"
    );
}
