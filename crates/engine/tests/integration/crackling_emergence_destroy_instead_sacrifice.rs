//! Crackling Emergence:
//!
//! "Enchant land you control
//!  Enchanted land is a 3/3 red Spirit creature with haste. It's still a land.
//!  If enchanted land would be destroyed, instead sacrifice this Aura and that
//!  land gains indestructible until end of turn."
//!
//! CR 614.1a + CR 614.6: the land's destruction is replaced, so it never
//! happens. The replacing instructions have two distinct referents (CR 608.2c):
//! "this Aura" is the replacement's source and "that land" is the permanent
//! that would have been destroyed.

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CRACKLING_EMERGENCE: &str = "Enchant land you control\nEnchanted land is a 3/3 red Spirit creature with haste. It's still a land.\nIf enchanted land would be destroyed, instead sacrifice this Aura and that land gains indestructible until end of turn.";
const STONE_RAIN: &str = "Destroy target land.";
const ARMAGEDDON: &str = "Destroy all lands.";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

/// Add a Crackling Emergence to P0's hand with {1}{R} in the pool to cast it.
/// Returns the Aura's id.
fn add_crackling_emergence(scenario: &mut GameScenario) -> ObjectId {
    let aura = scenario
        .add_spell_to_hand(P0, "Crackling Emergence", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        .from_oracle_text_with_keywords(&["enchant"], CRACKLING_EMERGENCE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
        ],
    );
    aura
}

#[test]
fn aura_is_sacrificed_and_land_survives_indestructible() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Red);
    let aura = scenario
        .add_spell_to_hand(P0, "Crackling Emergence", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        .from_oracle_text_with_keywords(&["enchant"], CRACKLING_EMERGENCE)
        .id();
    let stone_rain = scenario
        .add_spell_to_hand_from_oracle(P0, "Stone Rain", false, STONE_RAIN)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_991), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(aura).target_object(land).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
    // Reach guard: the destroy replacement parsed onto the Aura.
    assert!(
        runner.state().objects[&aura]
            .replacement_definitions
            .iter_all()
            .any(|r| r.execute.is_some()),
        "Crackling Emergence must parse its destroy replacement"
    );

    runner.cast(stone_rain).target_object(land).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.objects[&aura].zone,
        Zone::Graveyard,
        "\"sacrifice this Aura\" sacrifices the Aura"
    );
    assert_eq!(
        state.objects[&land].zone,
        Zone::Battlefield,
        "the land's destruction never happens"
    );
    assert!(
        state.objects[&land].has_keyword(&Keyword::Indestructible),
        "\"that land\" gains indestructible until end of turn"
    );
}

/// CR 614.1a + CR 614.6: one destroy-all effect replaced on two lands. Each
/// replaced destruction carries out its own replacing instructions, so both
/// Auras are sacrificed and both lands survive with indestructible.
#[test]
fn armageddon_runs_each_replacement_for_two_enchanted_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land_a = scenario.add_basic_land(P0, ManaColor::Red);
    let land_b = scenario.add_basic_land(P0, ManaColor::Red);
    let aura_a = add_crackling_emergence(&mut scenario);
    let aura_b = add_crackling_emergence(&mut scenario);
    let armageddon = scenario
        .add_spell_to_hand_from_oracle(P0, "Armageddon", false, ARMAGEDDON)
        .id();

    let mut runner = scenario.build();
    for (aura, land) in [(aura_a, land_a), (aura_b, land_b)] {
        runner.cast(aura).target_object(land).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
    }

    runner.cast(armageddon).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    for (aura, land) in [(aura_a, land_a), (aura_b, land_b)] {
        assert_eq!(
            state.objects[&aura].zone,
            Zone::Graveyard,
            "{aura:?}: each Aura is sacrificed"
        );
        assert_eq!(
            state.objects[&land].zone,
            Zone::Battlefield,
            "{land:?}: each land's destruction never happens"
        );
        assert!(
            state.objects[&land].has_keyword(&Keyword::Indestructible),
            "{land:?}: each land gains indestructible"
        );
    }
}

/// CR 704.5g: the enchanted land is a 3/3 creature, so 3 damage destroys it as
/// a state-based action. That destruction is replaced too: the Aura is
/// sacrificed and the land survives with indestructible.
#[test]
fn land_creature_with_lethal_damage_survives_and_aura_is_sacrificed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Red);
    let aura = add_crackling_emergence(&mut scenario);
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
        .id();

    let mut runner = scenario.build();
    runner.cast(aura).target_object(land).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);

    runner.cast(bolt).target_object(land).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.objects[&aura].zone,
        Zone::Graveyard,
        "Aura sacrificed"
    );
    assert_eq!(
        state.objects[&land].zone,
        Zone::Battlefield,
        "land survives"
    );
    assert!(state.objects[&land].has_keyword(&Keyword::Indestructible));
}
