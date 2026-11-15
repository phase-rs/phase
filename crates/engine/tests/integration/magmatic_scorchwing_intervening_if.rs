//! Magmatic Scorchwing's ETB damage is guarded by a library-scoped
//! intervening-if condition (CR 603.4), not merely a parser-shape marker.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const MAGMATIC_SCORCHWING_ORACLE: &str = "Flying\nWhen Magmatic Scorchwing enters, if there are no nonbasic land cards in your library, Magmatic Scorchwing deals 3 damage to any target.";

fn resolve_scorchwing_with_library_land(basic: bool) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scorchwing = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Magmatic Scorchwing",
            4,
            4,
            MAGMATIC_SCORCHWING_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let library_land = scenario.add_card_to_library_top(P0, "Library Land");

    let mut runner = scenario.build();
    let land = runner
        .state_mut()
        .objects
        .get_mut(&library_land)
        .expect("library land must exist");
    land.card_types.core_types.push(CoreType::Land);
    if basic {
        land.card_types.supertypes.push(Supertype::Basic);
    }
    land.base_card_types = land.card_types.clone();

    let outcome = runner.cast(scorchwing).target_player(P1).resolve();
    outcome.life_delta(P1)
}

#[test]
fn magmatic_scorchwing_intervening_if_counts_only_nonbasic_lands_in_its_library() {
    assert_eq!(
        resolve_scorchwing_with_library_land(true),
        -3,
        "with only a basic land in its library, Scorchwing's ETB trigger must deal 3 damage"
    );
    assert_eq!(
        resolve_scorchwing_with_library_land(false),
        0,
        "a nonbasic land in Scorchwing controller's library must stop the intervening-if trigger"
    );
}
