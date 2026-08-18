//! Regression coverage for duplicate Worldspine Wurm triggers during a
//! Recurring Nightmare activation.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn card_db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

#[test]
fn worldspine_wurm_sacrifice_creates_each_trigger_once() {
    let db = card_db();
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

    let wurm = scenario.add_real_card(P0, "Worldspine Wurm", Zone::Battlefield, db);
    let nightmare = scenario.add_real_card(P0, "Recurring Nightmare", Zone::Battlefield, db);
    let graveyard_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let _other_graveyard_creature =
        scenario.add_real_card(P0, "Elvish Mystic", Zone::Graveyard, db);
    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&nightmare]
        .abilities
        .iter()
        .position(|ability| matches!(ability.kind, engine::types::ability::AbilityKind::Activated))
        .expect("Recurring Nightmare must have an activated ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: nightmare,
            ability_index,
        })
        .expect("begin Recurring Nightmare activation");

    let mut saw_sacrifice = false;
    let mut saw_target = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(graveyard_creature)],
                    })
                    .expect("select Recurring Nightmare target");
                saw_target = true;
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![wurm] })
                    .expect("sacrifice Worldspine Wurm");
                saw_sacrifice = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay Recurring Nightmare's mana cost");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting state during activation: {other:?}"),
        }
    }

    assert!(saw_sacrifice, "activation must sacrifice Worldspine Wurm");
    assert!(saw_target, "activation must choose a graveyard creature");

    // Without the cost-event ownership check, the sacrifice event is parked a
    // second time while this ordering prompt is being returned, producing four
    // Wurm trigger entries instead of the two below.
    let wurm_triggers: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == wurm)
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { description, .. } => description.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        wurm_triggers,
        vec![
            "When ~ dies, create three 5/5 green Wurm creature tokens with trample.".to_string(),
            "When ~ is put into a graveyard from anywhere, shuffle it into its owner's library."
                .to_string(),
        ],
        "a single Battlefield-to-Graveyard move must create one of each Wurm trigger",
    );
}
