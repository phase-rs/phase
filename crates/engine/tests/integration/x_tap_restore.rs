use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::mana_sources::activatable_mana_source_selections;
use engine::game::printed_cards::{
    rehydrate_game_from_card_db_with_finalization, CardDbRehydrationFinalization,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TapCreaturesSelectionMode::{Fixed, VariableX};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, CostResume, ManaChoice, PayCostKind, PayableResource, PersistedGameState,
    PersistedRestoreFinalization, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use serde_json::{json, Value};

use crate::support::shared_card_db;

fn fixtures() -> &'static CardDatabase {
    shared_card_db().expect("printed fixture")
}

fn restore(wire: Value) -> GameRunner {
    let state = serde_json::from_value::<PersistedGameState>(wire)
        .expect("compatible checkpoint")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("prepare checkpoint")
        .finalize_after_rehydration(|state| {
            rehydrate_game_from_card_db_with_finalization(
                state,
                fixtures(),
                CardDbRehydrationFinalization::Defer,
            );
            Ok(())
        })
        .expect("finish checkpoint");
    GameRunner::from_state(state)
}

fn historical_face(name: &str, numeric: bool) -> Value {
    let mut face = serde_json::to_value(fixtures().get_face_by_name(name).unwrap()).unwrap();
    let leg = if name == "Merchant's Dockhand" {
        &mut face["abilities"][0]["cost"]["costs"][2]
    } else {
        &mut face["triggers"][1]["execute"]["effect"]["cost"]
    };
    assert_eq!(leg["type"], "TapCreatures");
    if numeric {
        let fields = leg.as_object_mut().unwrap();
        fields.remove("requirement");
        fields.insert("count".into(), json!(4294967295u64));
    } else {
        leg["requirement"] = json!({"requirement": "count", "count": 4294967295u64});
    }
    face
}

fn ingest(name: &str, face: Value) -> Result<CardDatabase, serde_json::Error> {
    CardDatabase::from_json_str(&json!({(name.to_lowercase()): face}).to_string())
}

fn dockhand_at_tap_prompt(db: &CardDatabase) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(P0, "Merchant's Dockhand", Zone::Battlefield, db);
    let helpers = (0..2)
        .map(|_| scenario.add_real_card(P0, "Ornithopter", Zone::Battlefield, fixtures()))
        .collect::<Vec<_>>();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Plains");
    }
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { player: P0, .. }
    ));
    runner.act(GameAction::ChooseX { value: 2 }).unwrap();
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::PayCost {
            player: P0,
            kind: PayCostKind::TapCreatures { mode: Fixed },
            count: 2,
            min_count: 2,
            choices,
            ..
        } if choices.len() == 2 && helpers.iter().all(|helper| choices.contains(helper))
    ));
    (runner, source, helpers)
}

#[test]
fn dockhand_historical_card_costs_restore_as_exact_announced_payment() {
    for numeric in [false, true] {
        let face = historical_face("Merchant's Dockhand", numeric);
        let mut malformed = face.clone();
        malformed["abilities"][0]["kind"] = json!("Spell");
        let Err(error) = ingest("Merchant's Dockhand", malformed) else {
            panic!("legacy tap X under a spell cost must fail");
        };
        assert!(error
            .to_string()
            .contains("AbilityDefinition.cost.costs[2]"));
        let db = ingest("Merchant's Dockhand", face).unwrap();
        let (mut uninterrupted, source, helpers) = dockhand_at_tap_prompt(&db);
        let snapshot =
            serde_json::to_value(PersistedGameState::capture(uninterrupted.state().clone()))
                .unwrap();

        let mut unbound = snapshot.clone();
        unbound["state"]["waiting_for"]["data"]["kind"]["mode"] = json!("VariableX");
        assert!(serde_json::from_value::<PersistedGameState>(unbound)
            .err()
            .unwrap()
            .to_string()
            .contains("tap X prompt"));

        let mut cancelled = restore(snapshot.clone());
        cancelled.act(GameAction::CancelCast).unwrap();
        assert!(matches!(
            cancelled.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert!(cancelled.state().stack.is_empty());
        assert!(!cancelled.state().objects[&source].tapped);
        assert!(helpers
            .iter()
            .all(|id| !cancelled.state().objects[id].tapped));

        let mut restored = restore(snapshot);
        assert_eq!(
            restored.state().waiting_for,
            uninterrupted.state().waiting_for
        );
        let before = restored.state().clone();
        assert!(restored
            .act(GameAction::SelectCards {
                cards: vec![helpers[0]]
            })
            .is_err());
        assert_eq!(restored.state().waiting_for, before.waiting_for);
        for helper in &helpers {
            assert_eq!(
                restored.state().objects[helper].tapped,
                before.objects[helper].tapped
            );
        }
        for runner in [&mut uninterrupted, &mut restored] {
            runner
                .act(GameAction::SelectCards {
                    cards: helpers.clone(),
                })
                .unwrap();
            assert!(runner.state().objects[&source].tapped);
            assert!(helpers
                .iter()
                .all(|helper| runner.state().objects[helper].tapped));
            assert_eq!(runner.state().stack.len(), 1);
            assert!(matches!(
                &runner.state().stack[0].kind,
                StackEntryKind::ActivatedAbility { source_id, ability, .. }
                    if *source_id == source && ability.chosen_x == Some(2)
            ));
        }
        assert_eq!(restored.state().stack, uninterrupted.state().stack);
    }
}

#[test]
fn battlesphere_historical_resolution_cost_and_checkpoint_allow_underfill() {
    for numeric in [false, true] {
        let db = ingest(
            "Myr Battlesphere",
            historical_face("Myr Battlesphere", numeric),
        )
        .unwrap();
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_real_card(P0, "Myr Battlesphere", Zone::Battlefield, &db);
        let helpers = (0..3)
            .map(|_| scenario.add_real_card(P0, "Myr Retriever", Zone::Battlefield, fixtures()))
            .collect::<Vec<_>>();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(source, AttackTarget::Player(P1))])
            .unwrap();
        runner.resolve_top();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        assert!(matches!(
            &runner.state().waiting_for,
            WaitingFor::PayCost {
                player: P0,
                kind: PayCostKind::TapCreatures { mode: VariableX },
                count: 3,
                min_count: 0,
                resume: CostResume::Resolution,
                choices,
                ..
            } if choices.len() == 3 && helpers.iter().all(|helper| choices.contains(helper))
        ));

        let snapshot =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let mut raw = snapshot["state"].clone();
        let saved_cost = &mut raw["objects"][source.0.to_string()]["trigger_definitions"][1]
            ["definition"]["execute"]["effect"]["cost"];
        assert_eq!(saved_cost["requirement"]["requirement"], "resolution_x");
        *saved_cost = historical_face("Myr Battlesphere", numeric)["triggers"][1]["execute"]
            ["effect"]["cost"]
            .clone();
        let mut fabricated = raw.clone();
        fabricated["resolution_frames"]["frames"] = json!([]);
        assert!(serde_json::from_value::<PersistedGameState>(fabricated)
            .err()
            .unwrap()
            .to_string()
            .contains("tap X prompt"));
        let mut no_mode = raw.clone();
        no_mode["waiting_for"]["data"]["kind"]
            .as_object_mut()
            .unwrap()
            .remove("mode");
        assert!(serde_json::from_value::<PersistedGameState>(no_mode).is_err());

        let mut zero = restore(raw.clone());
        zero.state_mut()
            .objects
            .get_mut(&source)
            .unwrap()
            .cost_x_paid = Some(5);
        zero.act(GameAction::SelectCards { cards: Vec::new() })
            .unwrap();
        assert_eq!(zero.state().objects[&source].power, Some(4));
        assert_eq!(zero.life(P1), 20);

        let mut trusted = restore(snapshot);
        let mut raw = restore(raw);
        for restored in [&mut trusted, &mut raw] {
            assert_eq!(restored.state().waiting_for, runner.state().waiting_for);
            restored
                .act(GameAction::SelectCards {
                    cards: vec![helpers[0]],
                })
                .unwrap();
            assert_eq!(restored.life(P1), 19);
            assert_eq!(restored.state().objects[&source].power, Some(5));
            assert!(restored.state().objects[&helpers[0]].tapped);
            assert!(helpers[1..]
                .iter()
                .all(|helper| !restored.state().objects[helper].tapped));
        }
    }
}

#[test]
fn hazel_round_trips_the_announcement_and_the_bound_tap_cursor() {
    let db = fixtures();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hazel = scenario.add_real_card(P0, "Hazel of the Rootbloom", Zone::Battlefield, db);
    let goose = scenario.add_real_card(P0, "Gilded Goose", Zone::Hand, db);
    let chatter = scenario.add_real_card(P0, "Chatter of the Squirrel", Zone::Hand, db);
    let first = scenario.add_basic_land(P0, ManaColor::Green);
    let second = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, first, false, vec![]),
            ManaUnit::new(ManaType::Green, second, false, vec![]),
        ],
    );
    let mut uninterrupted = scenario.build();
    uninterrupted.cast(goose).resolve();
    uninterrupted.cast(chatter).resolve();
    let selection = activatable_mana_source_selections(uninterrupted.state(), P0)
        .into_iter()
        .find(|selection| selection.source.object_id == hazel)
        .unwrap_or_else(|| {
            panic!(
                "Hazel has no mana action at {:?}",
                uninterrupted.state().waiting_for
            )
        });
    uninterrupted
        .act(GameAction::ActivateManaSource { selection })
        .unwrap();
    assert!(matches!(
        uninterrupted.state().waiting_for,
        WaitingFor::PayAmountChoice {
            player: P0, resource: PayableResource::TapCountX, source_id, min: 0, max: 2, ..
        } if source_id == hazel
    ));

    let amount_snapshot =
        serde_json::to_value(PersistedGameState::capture(uninterrupted.state().clone())).unwrap();
    let mut resumed = restore(amount_snapshot);
    assert_eq!(
        resumed.state().waiting_for,
        uninterrupted.state().waiting_for
    );
    for runner in [&mut uninterrupted, &mut resumed] {
        runner
            .act(GameAction::SubmitPayAmount { amount: 2 })
            .unwrap();
    }
    assert_eq!(
        resumed.state().waiting_for,
        uninterrupted.state().waiting_for
    );

    let WaitingFor::PayCost {
        player: P0,
        kind: PayCostKind::TapCreatures { mode: Fixed },
        min_count: 2,
        count: 2,
        choices,
        ..
    } = &resumed.state().waiting_for
    else {
        panic!("expected Hazel's exact tap prompt")
    };
    let tokens = choices.clone();
    assert_eq!(tokens.len(), 2);

    let tap_snapshot =
        serde_json::to_value(PersistedGameState::capture(resumed.state().clone())).unwrap();
    let mut resumed = restore(tap_snapshot);
    assert!(resumed
        .act(GameAction::SelectCards {
            cards: vec![tokens[0]]
        })
        .is_err());
    assert_eq!(
        resumed.state().waiting_for,
        uninterrupted.state().waiting_for
    );
    for runner in [&mut uninterrupted, &mut resumed] {
        runner
            .act(GameAction::SelectCards {
                cards: tokens.clone(),
            })
            .unwrap();
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::Combination(vec![ManaType::White, ManaType::Blue]),
                count: 1,
            })
            .unwrap();
        assert_eq!(runner.life(P0), 18);
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().objects[&hazel].tapped);
        assert!(tokens.iter().all(|id| runner.state().objects[id].tapped));
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::White),
            1
        );
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Blue),
            1
        );
    }
}
