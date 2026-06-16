//! Regression test for issue #3315 — "Explore" spell that grants both
//! additional land drop and draw should parse both effects correctly and
//! resolve to grant the additional land permission at runtime.

use engine::game::scenario::{GameScenario, P0};
use engine::game::static_abilities::additional_land_drops;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

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

#[test]
fn explore_keyword_parsing() {
    let def = parse_effect_chain(
        "Explore.\nYou may play an additional land this turn.\nDraw a card.",
        AbilityKind::Spell,
    );

    println!("Root effect: {:?}", def.effect);
    if let Some(sub) = &def.sub_ability {
        println!("Sub 1 effect: {:?}", sub.effect);
        if let Some(sub2) = &sub.sub_ability {
            println!("Sub 2 effect: {:?}", sub2.effect);
            if let Some(sub3) = &sub2.sub_ability {
                println!("Sub 3 effect: {:?}", sub3.effect);
            }
        }
    }
}

#[test]
fn explore_keyword_with_additional_land_and_draw() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Add a card to library so both explore and draw work
    scenario.add_card_to_library_top(P0, "Island");

    // Create and cast a spell with "Explore" keyword followed by land permission and draw
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Explore Card",
            false,
            "Explore.\nYou may play an additional land this turn.\nDraw a card.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let outcome = scenario.build().cast(spell).resolve();

    // Check that the additional land drop was granted
    let lands_granted = additional_land_drops(outcome.state(), P0);
    println!("Additional land drops: {}", lands_granted);
    assert_eq!(
        lands_granted,
        1,
        "Card with explore keyword + land permission must grant 1 additional land drop"
    );

    // Check that a card was drawn
    let hand_count = outcome.state().players[0].hand.len();
    println!("Hand count: {}", hand_count);
    assert_eq!(hand_count, 1, "Card with explore + draw must draw 1 card");
}
