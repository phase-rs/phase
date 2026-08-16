use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FIELD_OF_RUIN: &str = "{T}: Add {C}.\n{2}, {T}, Sacrifice this land: Destroy target nonbasic land an opponent controls. Each player searches their library for a basic land card, puts it onto the battlefield, then shuffles.";

#[test]
fn field_of_ruin_stays_sacrificed_after_both_players_search() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let field = scenario
        .add_land_from_oracle(P0, "Field of Ruin", FIELD_OF_RUIN)
        .id();
    let victim = scenario
        .add_land_from_oracle(P1, "Victim Land", "{T}: Add {C}.")
        .id();
    let forest = scenario.add_card_to_library_top(P0, "Forest");
    let island = scenario.add_card_to_library_top(P1, "Island");
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    for id in [forest, island] {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Land);
        object.card_types.supertypes.push(Supertype::Basic);
        object.base_card_types = object.card_types.clone();
    }

    let outcome = runner
        .activate(field, 1)
        .target_object(victim)
        .search_first_legal()
        .resolve();

    outcome.assert_zone(&[field, victim], Zone::Graveyard);
    outcome.assert_zone(&[forest, island], Zone::Battlefield);
    assert_eq!(
        outcome
            .state()
            .objects
            .values()
            .filter(|object| object.zone == Zone::Battlefield && object.name == "Field of Ruin")
            .count(),
        0,
        "the sacrificed Field of Ruin must not be returned by the search-result move"
    );
}

#[test]
fn field_of_ruin_stays_sacrificed_when_one_player_has_no_basic_to_find() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let field = scenario
        .add_land_from_oracle(P0, "Field of Ruin", FIELD_OF_RUIN)
        .id();
    let victim = scenario
        .add_land_from_oracle(P1, "Victim Land", "{T}: Add {C}.")
        .id();
    let forest = scenario.add_card_to_library_top(P0, "Forest");
    let filler = scenario.add_card_to_library_top(P1, "Library Filler");
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&forest).unwrap();
    object.card_types.core_types.push(CoreType::Land);
    object.card_types.supertypes.push(Supertype::Basic);
    object.base_card_types = object.card_types.clone();

    let outcome = runner
        .activate(field, 1)
        .target_object(victim)
        .search_first_legal()
        .resolve();

    outcome.assert_zone(&[field, victim], Zone::Graveyard);
    outcome.assert_zone(&[forest], Zone::Battlefield);
    outcome.assert_zone(&[filler], Zone::Library);
}
