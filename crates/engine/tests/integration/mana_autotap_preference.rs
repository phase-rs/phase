use engine::game::scenario::{GameScenario, P0};
use engine::types::events::GameEvent;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ADARKAR_WASTES_ORACLE: &str =
    "{T}: Add {C}.\n{T}: Add {W} or {U}. This land deals 1 damage to you.";
const MANA_CONFLUENCE_ORACLE: &str = "{T}, Pay 1 life: Add one mana of any color.";
const BOUNTIFUL_PROMENADE_ORACLE: &str =
    "This land enters tapped unless you have two or more opponents.\n{T}: Add {G} or {W}.";
const RAISE_THE_ALARM_ORACLE: &str = "Create two 1/1 white Soldier creature tokens.";

fn raise_the_alarm(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id()
}

#[test]
fn autotap_reserves_free_colorless_painland_mode_for_generic_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let adarkar_wastes = scenario
        .add_land_from_oracle(P0, "Adarkar Wastes", ADARKAR_WASTES_ORACLE)
        .id();
    let mana_confluence = scenario
        .add_land_from_oracle(P0, "Mana Confluence", MANA_CONFLUENCE_ORACLE)
        .id();
    let spell = raise_the_alarm(&mut scenario);

    let outcome = scenario.build().cast(spell).resolve();

    outcome.assert_life_delta(P0, -1);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(outcome.state().objects[&adarkar_wastes].tapped);
    assert!(outcome.state().objects[&mana_confluence].tapped);
    assert!(outcome.events().iter().any(|event| {
        matches!(event, GameEvent::ManaAdded { source_id, mana_type: ManaType::Colorless, .. }
            if *source_id == adarkar_wastes)
    }));
    assert!(outcome.events().iter().any(|event| {
        matches!(event, GameEvent::ManaAdded { source_id, mana_type: ManaType::White, .. }
            if *source_id == mana_confluence)
    }));
    assert_eq!(outcome.state().battlefield.len(), 4);
}

#[test]
fn autotap_prefers_basic_lands_to_an_equivalent_free_dual_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bountiful_promenade = scenario
        .add_land_from_oracle(P0, "Bountiful Promenade", BOUNTIFUL_PROMENADE_ORACLE)
        .id();
    let first_plains = scenario.add_basic_land(P0, ManaColor::White);
    let second_plains = scenario.add_basic_land(P0, ManaColor::White);
    let spell = raise_the_alarm(&mut scenario);

    let outcome = scenario.build().cast(spell).resolve();

    outcome.assert_life_delta(P0, 0);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(outcome.state().objects[&first_plains].tapped);
    assert!(outcome.state().objects[&second_plains].tapped);
    assert!(!outcome.state().objects[&bountiful_promenade].tapped);
    assert_eq!(outcome.state().battlefield.len(), 5);
}

#[test]
fn autotap_prefers_basic_lands_to_a_painland_when_both_cover_the_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let adarkar_wastes = scenario
        .add_land_from_oracle(P0, "Adarkar Wastes", ADARKAR_WASTES_ORACLE)
        .id();
    let first_plains = scenario.add_basic_land(P0, ManaColor::White);
    let second_plains = scenario.add_basic_land(P0, ManaColor::White);
    let spell = raise_the_alarm(&mut scenario);

    let outcome = scenario.build().cast(spell).resolve();

    outcome.assert_life_delta(P0, 0);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(outcome.state().objects[&first_plains].tapped);
    assert!(outcome.state().objects[&second_plains].tapped);
    assert!(!outcome.state().objects[&adarkar_wastes].tapped);
}
