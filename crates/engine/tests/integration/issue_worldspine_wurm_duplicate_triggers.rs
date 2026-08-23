//! Regression coverage for duplicate Worldspine Wurm triggers during a
//! Recurring Nightmare activation.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0};
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

/// Sacrifice Worldspine Wurm to a Recurring Nightmare activation, optionally
/// under extra battlefield observers, and drive the activation to priority.
fn activate_recurring_nightmare(observers: &[&str]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
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
    let observer_ids: Vec<ObjectId> = observers
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
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
    (runner, wurm, observer_ids)
}

fn trigger_descriptions(runner: &GameRunner, source_id: ObjectId) -> Vec<String> {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source_id)
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { description, .. } => description.clone(),
            _ => None,
        })
        .collect()
}

#[test]
fn worldspine_wurm_sacrifice_creates_each_trigger_once() {
    let (runner, wurm, _) = activate_recurring_nightmare(&[]);

    // Without the cost-event ownership filter, the sacrifice event is parked a
    // second time while this ordering prompt is being returned, producing four
    // Wurm trigger entries instead of the two below.
    assert_eq!(
        trigger_descriptions(&runner, wurm),
        vec![
            "When ~ dies, create three 5/5 green Wurm creature tokens with trample.".to_string(),
            "When ~ is put into a graveyard from anywhere, shuffle it into its owner's library."
                .to_string(),
        ],
        "a single Battlefield-to-Graveyard move must create one of each Wurm trigger",
    );
}

/// CR 603.2c: the same parked cost span carries three distinct occurrences — the
/// Wurm's death, its sacrifice, and Recurring Nightmare's own return to hand —
/// and `finish_pending_cost_or_cast`'s announcement drain has already claimed
/// all three in `consumed_before_priority_trigger_events` by the time the span
/// is parked. This pins that the parking helper suppresses exactly the claimed
/// occurrences and nothing else, so every observer of the span still reaches the
/// stack once. Without the filter the Wurm triggers double.
#[test]
fn paused_cost_resume_keeps_every_occurrence_in_the_span_exactly_once() {
    let (runner, wurm, observers) =
        activate_recurring_nightmare(&["Korvold, Fae-Cursed King", "Justice, Vance Astrovik"]);
    let (korvold, justice) = (observers[0], observers[1]);

    assert_eq!(
        trigger_descriptions(&runner, wurm).len(),
        2,
        "the owned sacrifice occurrence must still produce exactly one of each Wurm trigger",
    );
    assert_eq!(
        trigger_descriptions(&runner, korvold).len(),
        1,
        "the sacrifice occurrence in the same span must trigger its other observer once",
    );
    assert_eq!(
        trigger_descriptions(&runner, justice).len(),
        1,
        "the return-to-hand occurrence in the same span must still trigger once",
    );
}
