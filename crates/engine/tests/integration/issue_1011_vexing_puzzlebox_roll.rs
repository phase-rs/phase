//! Issue #1011 — Vexing Puzzlebox {T} ability must roll a d20 after adding mana.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ManaChoice, WaitingFor};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const VEXING_PUZZLEBOX_ORACLE: &str = "Whenever you roll one or more dice, put a number of charge counters on this artifact equal to the result.\n{T}: Add one mana of any color. Roll a d20.\n{T}, Remove 100 charge counters from this artifact: Search your library for an artifact card, put that card onto the battlefield, then shuffle.";

#[test]
fn vexing_puzzlebox_tap_ability_is_mana_ability() {
    let parsed = engine::parser::parse_oracle_text(
        VEXING_PUZZLEBOX_ORACLE,
        "Vexing Puzzlebox",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    let ability = &parsed.abilities[0];
    assert!(
        engine::game::mana_abilities::is_mana_ability(ability),
        "CR 605.1: mana plus d20 roll remains a mana ability resolved inline"
    );
}

#[test]
fn vexing_puzzlebox_tap_ability_rolls_d20() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let puzzlebox = scenario
        .add_creature_from_oracle(P0, "Vexing Puzzlebox", 0, 0, VEXING_PUZZLEBOX_ORACLE)
        .as_artifact()
        .id();

    let mut runner = scenario.build();
    let mut events = Vec::new();
    events.extend(
        runner
            .act(GameAction::ActivateAbility {
                source_id: puzzlebox,
                ability_index: 0,
            })
            .expect("activation must succeed")
            .events,
    );

    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseManaColor { .. } => {
                events.extend(
                    runner
                        .act(GameAction::ChooseManaColor {
                            choice: ManaChoice::SingleColor(ManaType::Red),
                            count: 1,
                        })
                        .expect("mana color choice must succeed")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority must succeed")
                        .events,
                );
            }
            _ => break,
        }
    }

    let rolled = events.iter().any(|e| {
        matches!(
            e,
            GameEvent::DieRolled {
                sides: 20,
                result: Some(_),
                ..
            }
        )
    });
    assert!(rolled, "activating tap ability must emit DieRolled");
}
