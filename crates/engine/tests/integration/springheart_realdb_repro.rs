//! PRODUCTION-PATH repro for Springheart Nantuko: loads the real card-data.json
//! ability structure via `add_real_card` + `rehydrate_game_from_card_db`, rather
//! than re-parsing the Oracle text. This exercises the exact typed structure the
//! deployed app uses (serialization → deserialization round-trip).

use super::support::shared_card_db;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn insect_tokens(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Insect")
        })
        .count()
}

fn grizzly_copies(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield && o.name == "Grizzly Bears")
        .count()
}

#[test]
fn springheart_realdb_attached_accept_makes_copy() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host_id = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let sh_id = scenario.add_real_card(P0, "Springheart Nantuko", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .state_mut()
        .objects
        .get_mut(&sh_id)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host_id));
    // Floating mana so the pay clearly succeeds.
    for ty in [ManaType::Green, ManaType::Green] {
        runner.state_mut().players[0]
            .mana_pool
            .add(ManaUnit::new(ty, ObjectId(0), false, vec![]));
    }

    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("play land");

    let mut prompts = 0;
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
    assert_eq!(prompts, 1, "exactly one optional pay prompt when attached");
    assert_eq!(grizzly_copies(&runner), 1, "accept → copy of host");
    assert_eq!(
        insect_tokens(&runner),
        0,
        "no insect fallback when copy made"
    );
}

#[test]
fn springheart_realdb_attached_decline_makes_insect() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host_id = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let sh_id = scenario.add_real_card(P0, "Springheart Nantuko", Zone::Battlefield, db);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .state_mut()
        .objects
        .get_mut(&sh_id)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host_id));

    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("play land");

    let mut prompts = 0;
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
    assert_eq!(
        prompts, 1,
        "attached but declining still offers the pay prompt"
    );
    assert_eq!(insect_tokens(&runner), 1, "decline → insect fallback token");
    assert_eq!(grizzly_copies(&runner), 0, "no copy when declined");
}

#[test]
fn springheart_realdb_unattached_no_prompt_makes_insect() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Springheart Nantuko", Zone::Battlefield, db);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    for ty in [ManaType::Green, ManaType::Green] {
        runner.state_mut().players[0]
            .mana_pool
            .add(ManaUnit::new(ty, ObjectId(0), false, vec![]));
    }

    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("play land");

    let mut prompts = 0;
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decide");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
    assert_eq!(prompts, 0, "unattached → NO optional prompt");
    assert_eq!(
        insect_tokens(&runner),
        1,
        "unattached → insect fallback token"
    );
    assert_eq!(grizzly_copies(&runner), 0, "no copy when unattached");
}
