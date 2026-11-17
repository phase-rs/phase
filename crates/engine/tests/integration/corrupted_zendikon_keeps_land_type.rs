//! Corrupted Zendikon ("Enchanted land is a 3/3 black Ooze creature. It's still
//! a land.") must stay on the battlefield: the enchanted permanent keeps its
//! Land type (CR 205.1a + CR 205.1b), so the Aura is not an illegal attachment
//! (CR 704.5m) and goes to the graveyard only if the land dies.

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CORRUPTED_ZENDIKON_ORACLE: &str = "Enchant land\nEnchanted land is a 3/3 black Ooze creature. It's still a land.\nWhen enchanted land dies, return that card to its owner's hand.";

#[test]
fn corrupted_zendikon_stays_attached_and_land_remains_a_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    // A second land that must NOT be affected (presence guard for the filter).
    let other_land = scenario.add_basic_land(P0, ManaColor::Red);
    let aura = scenario
        .add_spell_to_hand(P0, "Corrupted Zendikon", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .from_oracle_text_with_keywords(&["enchant"], CORRUPTED_ZENDIKON_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(aura).target_object(land).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&aura].zone, Zone::Battlefield);
    assert_eq!(state.objects[&land].zone, Zone::Battlefield);

    let enchanted = &state.objects[&land];
    assert!(enchanted.card_types.core_types.contains(&CoreType::Land));
    assert!(enchanted
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(enchanted.power, Some(3));
    assert_eq!(enchanted.toughness, Some(3));

    let other = &state.objects[&other_land];
    assert!(!other.card_types.core_types.contains(&CoreType::Creature));
}

#[test]
fn corrupted_zendikon_on_artifact_land_keeps_artifact_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    // A second land that must NOT be affected (presence guard for the filter).
    let other_land = scenario.add_basic_land(P0, ManaColor::Red);
    let aura = scenario
        .add_spell_to_hand(P0, "Corrupted Zendikon", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .from_oracle_text_with_keywords(&["enchant"], CORRUPTED_ZENDIKON_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    // CR 205.1b: "still a land" retains every prior card type.
    {
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
    }
    runner.cast(aura).target_object(land).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&aura].zone, Zone::Battlefield);
    assert_eq!(state.objects[&land].zone, Zone::Battlefield);

    let enchanted = &state.objects[&land];
    assert!(enchanted.card_types.core_types.contains(&CoreType::Land));
    assert!(enchanted
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert!(enchanted
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(enchanted.power, Some(3));
    assert_eq!(enchanted.toughness, Some(3));

    let other = &state.objects[&other_land];
    assert!(!other.card_types.core_types.contains(&CoreType::Creature));
}

#[test]
fn corrupted_zendikon_on_creature_land_keeps_prior_creature_types() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    // A second land that must NOT be affected (presence guard for the filter).
    let other_land = scenario.add_basic_land(P0, ManaColor::Red);
    let aura = scenario
        .add_spell_to_hand(P0, "Corrupted Zendikon", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .from_oracle_text_with_keywords(&["enchant"], CORRUPTED_ZENDIKON_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    // CR 205.1b: "still a land" retains every prior card type.
    {
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.card_types.subtypes.push("Dryad".to_string());
        obj.base_card_types = obj.card_types.clone();
    }
    // The Creature subtype-set removal matches against the registry.
    runner.state_mut().all_creature_types = vec!["Dryad".to_string(), "Ooze".to_string()];
    runner.cast(aura).target_object(land).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&aura].zone, Zone::Battlefield);
    assert_eq!(state.objects[&land].zone, Zone::Battlefield);

    let enchanted = &state.objects[&land];
    assert!(enchanted.card_types.core_types.contains(&CoreType::Land));
    // CR 205.1b: prior subtypes survive "still a land"; Ooze is added.
    for sub in ["Forest", "Dryad", "Ooze"] {
        assert!(
            enchanted.card_types.subtypes.iter().any(|s| s == sub),
            "{sub} missing: {:?}",
            enchanted.card_types.subtypes
        );
    }
    assert!(enchanted
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(enchanted.power, Some(3));
    assert_eq!(enchanted.toughness, Some(3));

    let other = &state.objects[&other_land];
    assert!(!other.card_types.core_types.contains(&CoreType::Creature));
}
