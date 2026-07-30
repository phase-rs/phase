//! Issue #5983 — Sothera, the Supervoid: when a creature you control dies, each
//! opponent must choose and exile one of *their* creatures. The Discord report
//! had the controller prompted to sacrifice/exile their own instead.
//!
//! Exercises the production stack-resolution path: card-db (or parsed) execute
//! bodies are pushed as triggered abilities and resolved through `apply()` so
//! `player_scope` / `EffectZoneChoice` routing is covered end-to-end.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::stack;
use engine::types::ability::AbilityDefinition;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const SOTHERA_ORACLE: &str = "Whenever a creature you control dies, each opponent chooses a creature they control and exiles it.\n\
At the beginning of your end step, if a player controls no creatures, sacrifice Sothera, then put a creature card exiled with it onto the battlefield under your control with two additional +1/+1 counters on it.";

fn battlefield_creature_names(
    state: &engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
) -> Vec<String> {
    let mut names: Vec<String> = state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|o| o.name.clone())
        .collect();
    names.sort();
    names
}

fn exiled_creature_names(
    state: &engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
) -> Vec<String> {
    let mut names: Vec<String> = state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Exile
                && o.owner == player
                && o.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|o| o.name.clone())
        .collect();
    names.sort();
    names
}

fn sothera_dies_execute(state: &engine::types::game_state::GameState) -> AbilityDefinition {
    state
        .objects
        .values()
        .find(|o| o.name == "Sothera, the Supervoid" && o.zone == Zone::Battlefield)
        .expect("Sothera on battlefield")
        .trigger_definitions
        .as_slice()
        .iter()
        .find(|entry| {
            entry.definition.mode == TriggerMode::ChangesZone
                && entry.definition.origin == Some(Zone::Battlefield)
                && entry.definition.destination == Some(Zone::Graveyard)
        })
        .and_then(|entry| entry.definition.execute.as_deref().cloned())
        .expect("Sothera dies-trigger execute body")
}

fn push_sothera_dies_trigger(
    runner: &mut engine::game::scenario::GameRunner,
    execute: &AbilityDefinition,
) -> engine::types::identifiers::ObjectId {
    let sothera_id = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Sothera, the Supervoid" && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .expect("Sothera on battlefield");
    let ability = build_resolved_from_def(execute, sothera_id, P0);
    let entry_id = engine::types::identifiers::ObjectId(runner.state().next_object_id);
    runner.state_mut().next_object_id += 1;
    let entry = StackEntry {
        id: entry_id,
        source_id: sothera_id,
        controller: P0,
        kind: StackEntryKind::TriggeredAbility {
            source_id: sothera_id,
            ability: Box::new(ability),
            condition: None,
            trigger_event: None,
            description: Some(
                "Whenever a creature you control dies, each opponent chooses a creature they control and exiles it.".into(),
            ),
            source_name: "Sothera, the Supervoid".into(),
            subject_match_count: None,
            die_result: None,
        },
    };
    stack::push_to_stack(runner.state_mut(), entry, &mut vec![]);
    sothera_id
}

fn drive_edict_to_completion(
    runner: &mut engine::game::scenario::GameRunner,
    p1_creature: engine::types::identifiers::ObjectId,
) -> bool {
    let mut saw_opponent_prompt = false;
    for _ in 0..80 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ) {
            if let WaitingFor::EffectZoneChoice { player, .. } = runner.state().waiting_for.clone()
            {
                assert_eq!(
                    player,
                    P1,
                    "the exile choice must be offered to the opponent, not Sothera's controller; \
                     got waiting_for {:?}",
                    runner.state().waiting_for
                );
                saw_opponent_prompt = true;
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![p1_creature],
                    })
                    .expect("opponent exiles their own creature");
            }
            continue;
        }

        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }

        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while stack resolving");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => runner.pass_both_players(),
        }
    }
    saw_opponent_prompt
}

/// Discord scenario: P0 still controls creatures when the edict resolves — the
/// choice must go to P1, not back to Sothera's controller.
#[test]
fn sothera_dies_edict_prompts_opponent_while_controller_has_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Sothera, the Supervoid", 0, 0)
        .as_enchantment()
        .from_oracle_text(SOTHERA_ORACLE);
    scenario.add_creature(P0, "P0 Bear", 2, 2);
    let p1_creature = scenario.add_creature(P1, "Opponent Wolf", 3, 3).id();
    scenario.add_creature(P1, "Blocker", 3, 3);

    let mut runner = scenario.build();
    let execute = sothera_dies_execute(runner.state());
    push_sothera_dies_trigger(&mut runner, &execute);

    let saw_opponent_prompt = drive_edict_to_completion(&mut runner, p1_creature);

    assert!(
        saw_opponent_prompt,
        "must prompt P1 even while P0 controls other creatures; waiting_for={:?}",
        runner.state().waiting_for,
    );
    assert_eq!(
        battlefield_creature_names(runner.state(), P0),
        vec!["P0 Bear".to_string()],
        "P0's creatures must never be exiled by the opponent-edict"
    );
    assert_eq!(
        exiled_creature_names(runner.state(), P1),
        vec!["Opponent Wolf".to_string()]
    );
}

/// CR 700.4 + CR 109.5: stack resolution via card-db execute body.
#[test]
fn sothera_dies_edict_resolves_from_card_db() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Sothera, the Supervoid", Zone::Battlefield, db);
    scenario.add_creature(P0, "P0 Bear", 2, 2);
    let p1_creature = scenario.add_creature(P1, "Opponent Wolf", 3, 3).id();
    scenario.add_creature(P1, "Blocker", 3, 3);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let execute = sothera_dies_execute(runner.state());
    push_sothera_dies_trigger(&mut runner, &execute);

    let saw_opponent_prompt = drive_edict_to_completion(&mut runner, p1_creature);
    assert!(
        saw_opponent_prompt,
        "card-db Sothera must prompt P1; final waiting_for={:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        exiled_creature_names(runner.state(), P1),
        vec!["Opponent Wolf".to_string()]
    );
    assert_eq!(
        battlefield_creature_names(runner.state(), P1),
        vec!["Blocker".to_string()]
    );
}

/// When the opponent controls exactly one creature, the edict auto-exiles it
/// without pausing (no meaningful choice). P0's board must stay untouched.
#[test]
fn sothera_dies_edict_auto_exiles_single_opponent_creature_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Sothera, the Supervoid", 0, 0)
        .as_enchantment()
        .from_oracle_text(SOTHERA_ORACLE);
    scenario.add_creature(P0, "P0 Bear", 2, 2);
    let wolf = scenario.add_creature(P1, "Opponent Wolf", 3, 3).id();

    let mut runner = scenario.build();
    let execute = sothera_dies_execute(runner.state());
    push_sothera_dies_trigger(&mut runner, &execute);

    let saw_opponent_prompt = drive_edict_to_completion(&mut runner, wolf);

    assert!(
        !saw_opponent_prompt,
        "single eligible opponent creature should auto-exile without a pause"
    );
    assert_eq!(
        exiled_creature_names(runner.state(), P1),
        vec!["Opponent Wolf".to_string()]
    );
    assert_eq!(
        battlefield_creature_names(runner.state(), P0),
        vec!["P0 Bear".to_string()]
    );
}
