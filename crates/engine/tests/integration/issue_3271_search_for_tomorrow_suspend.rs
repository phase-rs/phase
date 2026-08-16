//! Issue #3271 — Search for Tomorrow: accepting the suspend last-counter "you
//! may cast it" prompt must put the spell on the stack (CR 702.62a + CR 608.2g).
//!
//! https://github.com/phase-rs/phase/issues/3271
//!
//! Reported bug: removing the last time counter prompts "cast it?", but
//! accepting does nothing — the sorcery never reaches the stack.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SEARCH_FOR_TOMORROW_ORACLE: &str = "Search your library for a basic land card, put it onto the battlefield, then shuffle.\n\
Suspend 2—{G} (Rather than cast this card from your hand, you may pay {G} and exile it with two time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)";

fn suspend_ability_index(state: &engine::types::game_state::GameState, card: ObjectId) -> usize {
    legal_actions(state)
        .into_iter()
        .find_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if source_id == card => Some(ability_index),
            _ => None,
        })
        .expect("Search for Tomorrow must offer its Suspend activation from hand")
}

fn time_counters(state: &engine::types::game_state::GameState, card: ObjectId) -> u32 {
    state
        .objects
        .get(&card)
        .and_then(|o| o.counters.get(&CounterType::Time).copied())
        .unwrap_or(0)
}

fn last_counter_cast_uses_during_resolution(
    state: &engine::types::game_state::GameState,
    card: ObjectId,
) -> bool {
    state.objects[&card]
        .trigger_definitions
        .iter_unchecked()
        .find(|t| {
            matches!(
                t.definition.mode,
                engine::types::triggers::TriggerMode::CounterRemoved
            )
        })
        .and_then(|t| t.definition.execute.as_ref())
        .is_some_and(|execute| {
            matches!(
                execute.effect.as_ref(),
                Effect::CastFromZone {
                    driver,
                    without_paying_mana_cost: true,
                    ..
                } if driver.is_during_resolution()
            )
        })
}

/// Drive the turn structure until the suspend last-counter optional cast prompt
/// surfaces, draining the stack and auto-passing combat steps as needed.
fn drive_until_suspend_cast_prompt(
    runner: &mut engine::game::scenario::GameRunner,
) -> Result<(), String> {
    for _ in 0..300 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => return Ok(()),
            WaitingFor::Priority { .. } => {
                if !runner.state().stack.is_empty() {
                    runner
                        .act(GameAction::PassPriority)
                        .map_err(|e| format!("PassPriority (stack drain): {e:?}"))?;
                    continue;
                }
                if runner.state().phase == Phase::DeclareAttackers {
                    runner
                        .act(GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        })
                        .map_err(|e| format!("DeclareAttackers: {e:?}"))?;
                } else if runner.state().phase == Phase::DeclareBlockers {
                    runner
                        .act(GameAction::DeclareBlockers {
                            assignments: vec![],
                        })
                        .map_err(|e| format!("DeclareBlockers: {e:?}"))?;
                } else {
                    runner
                        .act(GameAction::PassPriority)
                        .map_err(|e| format!("PassPriority: {e:?}"))?;
                    runner
                        .act(GameAction::PassPriority)
                        .map_err(|e| format!("PassPriority (second): {e:?}"))?;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .map_err(|e| format!("DeclareAttackers: {e:?}"))?;
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .map_err(|e| format!("DeclareBlockers: {e:?}"))?;
            }
            other => {
                return Err(format!("unexpected waiting_for: {other:?}"));
            }
        }
    }
    Err("timed out waiting for suspend last-counter cast prompt".to_string())
}

#[test]
fn search_for_tomorrow_last_counter_accept_casts_onto_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.add_basic_land(P0, ManaColor::Green);
    let search = scenario
        .add_spell_to_hand_from_oracle(P0, "Search for Tomorrow", false, SEARCH_FOR_TOMORROW_ORACLE)
        .id();

    let mut runner = scenario.build();

    assert!(
        last_counter_cast_uses_during_resolution(runner.state(), search),
        "parser must synthesize suspend last-counter CastFromZone with DuringResolution driver"
    );

    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));

    let suspend_idx = suspend_ability_index(runner.state(), search);
    runner
        .activate(search, suspend_idx)
        .target_object(search)
        .resolve();

    assert_eq!(runner.state().objects[&search].zone, Zone::Exile);
    assert_eq!(time_counters(runner.state(), search), 2);

    // Fast-forward to one counter remaining, then drive a single upkeep so the
    // last counter is removed and the CounterRemoved cast trigger fires.
    {
        let obj = runner.state_mut().objects.get_mut(&search).unwrap();
        obj.counters.insert(CounterType::Time, 1);
    }
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    drive_until_suspend_cast_prompt(&mut runner).expect("must reach suspend cast prompt");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "expected optional cast prompt for P0, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept suspend last-counter cast");

    assert_eq!(
        runner.state().objects[&search].zone,
        Zone::Stack,
        "accepting must cast Search for Tomorrow during resolution (CR 608.2g); \
         zone = {:?}, waiting_for = {:?}",
        runner.state().objects[&search].zone,
        runner.state().waiting_for,
    );
    assert_eq!(runner.state().stack.len(), 1);
}
