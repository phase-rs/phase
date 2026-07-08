//! Issue #1021 — Recurring Nightmare must return itself to hand when activating.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RECURRING_NIGHTMARE_ORACLE: &str = "{2}{B}, Sacrifice a creature, Return Recurring Nightmare to its owner's hand: Return target creature card from a graveyard to the battlefield. Activate only as a sorcery.";

#[test]
fn recurring_nightmare_returns_itself_to_hand_when_activating() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(10_000), false, vec![]),
        ],
    );

    let nightmare = scenario
        .add_creature(P0, "Recurring Nightmare", 0, 0)
        .as_enchantment()
        .from_oracle_text(RECURRING_NIGHTMARE_ORACLE)
        .id();
    let sacrifice = scenario.add_creature(P0, "Sacrifice Me", 1, 1).id();
    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Return", 2, 2)
        .id();
    // A second pre-existing graveyard creature keeps target selection
    // genuinely interactive: per CR 601.2c / CR 602.2b (targets are chosen
    // before costs are paid) and Recurring Nightmare's own Gatherer ruling,
    // the creature sacrificed to pay this activation's cost can never be the
    // ability's own target — see `exclude_cost_paid_object_that_left_battlefield`
    // in `ability_utils.rs`. With only one legal target the engine would
    // auto-select it and skip the `TargetSelection` round trip this test
    // exercises.
    let _other_gy_creature = scenario
        .add_creature_to_graveyard(P0, "Other Graveyard Resident", 1, 1)
        .id();

    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&nightmare]
        .abilities
        .iter()
        .position(|ability| matches!(ability.kind, engine::types::ability::AbilityKind::Activated))
        .expect("activated ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: nightmare,
            ability_index,
        })
        .expect("begin activation");

    let mut saw_sacrifice = false;
    let mut saw_target = false;

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![sacrifice],
                    })
                    .expect("sacrifice creature");
                saw_sacrifice = true;
            }
            WaitingFor::TargetSelection { ref selection, .. } => {
                assert!(
                    !selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(sacrifice)),
                    "the creature just sacrificed to pay this activation's cost must never \
                     be offered as the ability's own return target"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(gy_creature)],
                    })
                    .expect("select graveyard creature");
                saw_target = true;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(gy_creature)],
                    })
                    .expect("select graveyard creature");
                saw_target = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay activation mana from pool");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve ability");
            }
            other => panic!("unexpected waiting state during activation: {other:?}"),
        }
    }

    assert!(
        saw_sacrifice,
        "activation must require sacrificing a creature"
    );
    assert!(
        saw_target,
        "activation must require choosing a graveyard creature"
    );
    assert_eq!(
        runner.state().objects[&nightmare].zone,
        Zone::Hand,
        "Recurring Nightmare must return itself to hand as part of the activation cost"
    );
    assert_eq!(
        runner.state().objects[&gy_creature].zone,
        Zone::Battlefield,
        "ability must reanimate the chosen graveyard creature"
    );
    assert_eq!(
        runner.state().objects[&sacrifice].zone,
        Zone::Graveyard,
        "sacrificed creature must be in the graveyard"
    );
}
