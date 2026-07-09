//! Runtime proof for Cluster 59 — Everything Comes to Dust (WHO).
//!
//! "Convoke. Exile all creatures except those that share a creature type with a
//! creature that convoked this spell, all artifacts, and all enchantments."
//!
//! Proves the parsed `ChangeZoneAll { Exile, Or[...] }` resolves correctly:
//! - leg 1 (CR 702.51c + CR 205.3m): creatures EXCEPT those sharing a creature
//!   type with a convoker survive; every other creature is exiled.
//! - legs 2/3 (CR 701.13a): all artifacts and all enchantments are exiled.
//! - lands, and the convoker's tribe, survive.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nExile all creatures except those that share a creature type with a creature that convoked this spell, all artifacts, and all enchantments.";

#[test]
fn convoke_exception_protects_convoker_tribe() {
    let mut scenario = GameScenario::new_n_player(2, 5959);
    scenario.at_phase(Phase::PreCombatMain);

    // goblin_a convokes the spell; goblin_b shares its creature type.
    let goblin_a = scenario
        .add_creature(P0, "Goblin A", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let goblin_b = scenario
        .add_creature(P1, "Goblin B", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();
    // A creature that shares NO type with a Goblin.
    let bird = scenario
        .add_creature(P1, "Sparrow", 1, 1)
        .with_subtypes(vec!["Bird"])
        .id();
    let artifact = scenario.add_creature(P1, "Relic", 0, 0).as_artifact().id();
    let enchantment = scenario
        .add_creature(P1, "Aura Thing", 0, 0)
        .as_enchantment()
        .id();
    let land = scenario.add_basic_land(P1, ManaColor::Green);

    // Override the printed {7}{W}{W}{W} with {1} so a single convoked creature
    // pays the whole cost — the effect's resolution is what's under test.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Everything Comes to Dust", false, ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    // CR 205.3m: the live game seeds `all_creature_types` from the card DB; the
    // scenario harness leaves it empty, so register the types under test (a
    // subtype is only counted toward `SharedQuality::CreatureType` if it is a
    // known creature type).
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Bird".into()];
    // Pay the {1} via convoke → stamps the spell's convoked_creatures = [goblin_a].
    runner.cast(spell).convoke_with(&[goblin_a]).resolve();
    runner.advance_until_stack_empty();

    let zone_of = |id| runner.state().objects[&id].zone;

    // CR 702.51c + CR 205.3m: convoker and its tribe share a creature type → survive.
    assert_eq!(zone_of(goblin_a), Zone::Battlefield, "convoker survives");
    assert_eq!(
        zone_of(goblin_b),
        Zone::Battlefield,
        "shares Goblin with convoker → survives"
    );
    // CR 701.13a: every other arm of the union is exiled.
    assert_eq!(zone_of(bird), Zone::Exile, "non-sharing creature exiled");
    assert_eq!(zone_of(artifact), Zone::Exile, "artifact exiled");
    assert_eq!(zone_of(enchantment), Zone::Exile, "enchantment exiled");
    assert_eq!(zone_of(land), Zone::Battlefield, "land untouched");
}

#[test]
fn no_convoke_exiles_every_creature() {
    let mut scenario = GameScenario::new_n_player(2, 5960);
    scenario.at_phase(Phase::PreCombatMain);

    let goblin = scenario
        .add_creature(P0, "Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let bird = scenario
        .add_creature(P1, "Sparrow", 1, 1)
        .with_subtypes(vec!["Bird"])
        .id();
    let artifact = scenario.add_creature(P1, "Relic", 0, 0).as_artifact().id();
    let enchantment = scenario
        .add_creature(P1, "Aura Thing", 0, 0)
        .as_enchantment()
        .id();
    let land = scenario.add_basic_land(P1, ManaColor::Green);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Everything Comes to Dust", false, ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // No convoke → convoked_creatures is empty → the exception protects nobody.
    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();

    let zone_of = |id| runner.state().objects[&id].zone;
    assert_eq!(zone_of(goblin), Zone::Exile, "no convoker → goblin exiled");
    assert_eq!(zone_of(bird), Zone::Exile);
    assert_eq!(zone_of(artifact), Zone::Exile);
    assert_eq!(zone_of(enchantment), Zone::Exile);
    assert_eq!(zone_of(land), Zone::Battlefield, "land untouched");
}
