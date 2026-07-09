//! Regression test for issue #3315 — "Explore" spell that grants both
//! additional land drop and draw should parse both effects correctly and
//! resolve to grant the additional land permission at runtime.

use engine::game::scenario::{GameScenario, P0};
use engine::game::static_abilities::additional_land_drops;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

#[test]
fn explore_spell_parses_both_effects() {
    let def = parse_effect_chain(
        "You may play an additional land this turn.\nDraw a card.",
        AbilityKind::Spell,
    );

    // Root effect should be the additional land permission (GenericEffect)
    assert!(
        matches!(&*def.effect, Effect::GenericEffect { .. }),
        "Root effect should be GenericEffect for additional land, got {:?}",
        def.effect
    );

    // Sub-ability should be the draw effect
    let sub = def
        .sub_ability
        .as_ref()
        .expect("Should have a sub_ability for the draw");
    assert!(
        matches!(&*sub.effect, Effect::Draw { .. }),
        "Sub-effect should be Draw, got {:?}",
        sub.effect
    );
}

#[test]
fn explore_spell_resolves_both_effects() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Add a card to library so draw works
    scenario.add_card_to_library_top(P0, "Forest");

    // Create and cast the explore spell
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Explore",
            false,
            "You may play an additional land this turn.\nDraw a card.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let outcome = scenario.build().cast(spell).resolve();

    // Check that the additional land drop was granted
    assert_eq!(
        additional_land_drops(outcome.state(), P0),
        1,
        "Explore spell must grant 1 additional land drop"
    );

    // Check that a card was drawn
    let hand_count = outcome.state().players[0].hand.len();
    assert_eq!(hand_count, 1, "Explore spell must draw 1 card");
}
