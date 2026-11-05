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
