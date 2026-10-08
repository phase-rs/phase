use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ESCAPE_TUNNEL_ORACLE: &str = "{T}, Sacrifice this land: Search your library for a basic land card, put it onto the battlefield tapped, then shuffle.\n{T}, Sacrifice this land: Target creature with power 2 or less can't be blocked this turn.";
const KAZANDU_NECTARPOT_ORACLE: &str =
    "Landfall — Whenever a land you control enters, you gain 1 life.";

#[test]
fn escape_tunnel_search_land_enters_and_triggers_landfall() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tunnel = scenario
        .add_land_from_oracle(P0, "Escape Tunnel", ESCAPE_TUNNEL_ORACLE)
        .id();
    scenario.add_creature_from_oracle(P0, "Kazandu Nectarpot", 1, 2, KAZANDU_NECTARPOT_ORACLE);
    let forest = scenario.add_card_to_library_top(P0, "Forest");
    let mut runner = scenario.build();
    let forest_object = runner.state_mut().objects.get_mut(&forest).unwrap();
    forest_object.card_types.core_types.push(CoreType::Land);
    forest_object.card_types.supertypes.push(Supertype::Basic);
    forest_object.base_card_types = forest_object.card_types.clone();

    // CR 603.2 + CR 603.3: the fetched basic land's entry triggers landfall,
    // which goes on the stack before priority and resolves after both players pass.
    let outcome = runner.activate(tunnel, 0).search_first_legal().resolve();

    outcome.assert_zone(&[tunnel], Zone::Graveyard);
    outcome.assert_zone(&[forest], Zone::Battlefield);
    assert!(outcome.state().objects[&forest].tapped);
    outcome.assert_life_delta(P0, 1);
}
