//! Regression for issue #6979: land-mana amplification triggers must observe a
//! mana ability's aggregate output once, with the printed source restrictions.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::game::zones::create_object;
use engine::types::ability::ChosenAttribute;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, ManaAbilityTriggerState, ManaTapState};
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::zones::Zone;

const CAGED_SUN_ORACLE: &str = "As this artifact enters, choose a color.\nCreatures you control of the chosen color get +1/+1.\nWhenever a land's ability causes you to add one or more mana of the chosen color, add an additional one mana of that color.";
const EXTRAPLANAR_LENS_ORACLE: &str = "Imprint — When this artifact enters, you may exile target land you control.\nWhenever a land with the same name as the exiled card is tapped for mana, its controller adds one mana of any type that land produced.";

#[test]
fn caged_sun_triggers_once_from_a_non_tap_land_mana_ability() {
    let mut scenario = GameScenario::new();
    let sun = scenario
        .add_creature(P0, "Caged Sun", 0, 0)
        .as_artifact()
        .from_oracle_text(CAGED_SUN_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&sun)
        .expect("Caged Sun")
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));

    let state = runner.state_mut();
    let land = create_object(
        state,
        CardId(6979),
        P0,
        "Red land".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&land)
        .expect("land")
        .card_types
        .core_types
        .push(CoreType::Land);

    process_triggers(
        runner.state_mut(),
        &[GameEvent::ManaAbilityProduced {
            player_id: P0,
            source_id: land,
            produced: vec![ManaType::Red, ManaType::Red],
            trigger_state: ManaAbilityTriggerState::Pending,
        }],
    );

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        1,
        "Caged Sun adds exactly one chosen-color mana per qualifying ability resolution"
    );
}

#[test]
fn extraplanar_lens_matches_the_name_of_its_imprinted_land() {
    let mut scenario = GameScenario::new();
    let lens = scenario
        .add_creature(P0, "Extraplanar Lens", 0, 0)
        .as_artifact()
        .from_oracle_text(EXTRAPLANAR_LENS_ORACLE)
        .id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    let imprinted = create_object(state, CardId(6980), P0, "Forest".to_string(), Zone::Exile);
    let land = create_object(
        state,
        CardId(6981),
        P0,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&land)
        .expect("land")
        .card_types
        .core_types
        .push(CoreType::Land);
    state.exile_links.push(ExileLink {
        source_id: lens,
        exiled_id: imprinted,
        kind: ExileLinkKind::TrackedBySource,
    });

    process_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: P0,
            source_id: land,
            produced: vec![ManaType::Green],
            tap_state: ManaTapState::FromTap,
        }],
    );

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "Lens adds one mana of the imprinted land's produced type"
    );
}
