//! Regression for GitHub issue #3315 — Explore spell extra land + draw.

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

    assert!(
        matches!(&*def.effect, Effect::GenericEffect { .. }),
        "Root effect should be GenericEffect for additional land, got {:?}",
        def.effect
    );

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
fn explore_spell_grants_extra_land_drop_and_draws() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Forest");

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

    assert_eq!(
        additional_land_drops(outcome.state(), P0),
        1,
        "Explore spell must grant 1 additional land drop"
    );
    assert_eq!(
        outcome.state().players[0].hand.len(),
        1,
        "Explore spell must draw 1 card"
    );
}
