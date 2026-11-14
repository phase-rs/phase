use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::events::GameEvent;
use engine::types::game_state::ExtraTurn;
use engine::types::phase::Phase;

const TIME_WARP: &str = "Target player takes an extra turn after this one.";
const TIME_STRETCH: &str = "Target player takes two extra turns after this one.";

fn cast_extra_turn_spell(name: &str, oracle: &str) -> engine::game::scenario::CastOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_player(P1).resolve()
}

fn assert_extra_turn_result(name: &str, oracle: &str, expected: usize) {
    let outcome = cast_extra_turn_spell(name, oracle);
    assert_eq!(
        outcome.state().extra_turns,
        vec![
            ExtraTurn {
                player: P1,
                anchor: P0,
            };
            expected
        ]
    );
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|event| matches!(event, GameEvent::ExtraTurnCreated { player_id, anchor } if *player_id == P1 && *anchor == P0))
            .count(),
        expected
    );
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ExtraTurn,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[test]
fn time_warp_creates_one_extra_turn() {
    assert_extra_turn_result("Time Warp", TIME_WARP, 1);
}

#[test]
fn time_stretch_creates_two_extra_turns() {
    assert_extra_turn_result("Time Stretch", TIME_STRETCH, 2);
}
