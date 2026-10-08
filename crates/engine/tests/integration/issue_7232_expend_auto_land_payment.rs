//! Issue #7232: auto-tapping lands must count toward Expend.
//!
//! Bakersbane Duo's Oracle text (Scryfall):
//! "Whenever you expend 4, this creature gets +1/+1 until end of turn."

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

const BAKERSBANE_DUO_ORACLE: &str = "When this creature enters, create a Food token.\n\
Whenever you expend 4, this creature gets +1/+1 until end of turn. (You expend 4 as you \
spend your fourth total mana to cast spells during a turn.)";

#[test]
fn auto_tapped_lands_count_toward_expend() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let duo = scenario
        .add_creature_from_oracle(P0, "Bakersbane Duo", 2, 2, BAKERSBANE_DUO_ORACLE)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let spell = scenario
        .add_creature_to_hand(P0, "Four-Mana Test Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).commit();

    assert_eq!(
        runner.state().mana_spent_on_spells_this_turn.get(&P0),
        Some(&4),
        "auto-tapped lands must contribute their four mana to Expend"
    );

    runner.advance_until_stack_empty();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&duo].power,
            runner.state().objects[&duo].toughness,
        ),
        (Some(3), Some(3)),
        "crossing Expend 4 must resolve Bakersbane Duo's +1/+1 trigger"
    );
}
