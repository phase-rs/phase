//! Regression for issue #2852: Breya's modal activated ability must sacrifice
//! two artifacts as part of its cost.
//!
//! https://github.com/phase-rs/phase/issues/2852

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BREYA_ORACLE: &str =
    "When Breya enters, create two 1/1 blue Thopter artifact creature tokens with flying.\n\
{2}, Sacrifice two artifacts: Choose one —\n\
• Breya deals 3 damage to target player or planeswalker.\n\
• Target creature gets -4/-4 until end of turn.\n\
• You gain 5 life.";

fn artifact_on_battlefield(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario.add_creature(P0, name, 0, 0).as_artifact().id()
}

#[test]
fn issue_2852_breya_modal_activation_sacrifices_two_artifacts_before_resolving() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let breya = scenario
        .add_creature_from_oracle(P0, "Breya, Etherium Shaper", 4, 4, BREYA_ORACLE)
        .as_artifact()
        .id();
    let artifact_a = artifact_on_battlefield(&mut scenario, "Artifact A");
    let artifact_b = artifact_on_battlefield(&mut scenario, "Artifact B");
    let artifact_c = artifact_on_battlefield(&mut scenario, "Artifact C");

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: breya,
            ability_index: 0,
        })
        .expect("Breya's activated ability should be activatable");

    let mut saw_modal = false;
    let mut saw_sacrifice = false;

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes { indices: vec![2] })
                    .expect("choosing the gain-5-life mode should succeed");
                saw_modal = true;
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                count,
                min_count,
                ..
            } => {
                assert_eq!(min_count, 2);
                assert_eq!(count, 2);
                assert!(choices.contains(&artifact_a));
                assert!(choices.contains(&artifact_b));
                assert!(choices.contains(&artifact_c));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![artifact_a, artifact_b],
                    })
                    .expect("sacrificing two artifacts should succeed");
                saw_sacrifice = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("paying generic mana from the pool should succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should resolve Breya's ability");
            }
            other => panic!("unexpected waiting state during Breya activation: {other:?}"),
        }
    }

    assert!(saw_modal, "Breya's ability must prompt for a mode");
    assert!(
        saw_sacrifice,
        "Breya's modal activation must pay Sacrifice two artifacts before resolving"
    );
    assert_eq!(runner.state().objects[&artifact_a].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&artifact_b].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&artifact_c].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 5,
        "the chosen mode should gain 5 life after costs are paid"
    );
}
