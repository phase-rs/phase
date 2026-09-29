use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const OWNER_RELATIVE_ORACLE: &str =
    "When this creature dies, its owner draws a card and each other player loses 1 life.";

#[test]
fn owner_relative_each_other_player_excludes_owner_not_controller() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Reward Card"]);
    let creature = scenario
        .add_creature_from_oracle(P0, "Owner Anchor Creature", 2, 2, OWNER_RELATIVE_ORACLE)
        .controlled_by(P1)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Destroy Probe", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(destroy).target_object(creature).resolve();

    outcome.assert_zone(&[creature], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, 0);
    outcome.assert_life_delta(P1, -1);
    outcome.assert_life_delta(p2, -1);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}
