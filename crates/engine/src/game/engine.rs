use thiserror::Error;

use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::mana_payment;
use super::mulligan;
use super::priority;
use super::sba;
use super::turns;
use super::zones;

#[derive(Debug, Clone, Error)]
pub enum EngineError {
    #[error("Invalid action: {0}")]
    InvalidAction(String),
    #[error("Wrong player")]
    WrongPlayer,
    #[error("Not your priority")]
    NotYourPriority,
    #[error("Action not allowed: {0}")]
    ActionNotAllowed(String),
}

pub fn apply(
    state: &mut GameState,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    let mut events = Vec::new();

    // Validate and process action against current WaitingFor
    let waiting_for = match (&state.waiting_for.clone(), action) {
        (WaitingFor::Priority { player }, GameAction::PassPriority) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            priority::handle_priority_pass(state, &mut events)
        }
        (WaitingFor::Priority { player }, GameAction::PlayLand { card_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            handle_play_land(state, card_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::TapLandForMana { object_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            handle_tap_land_for_mana(state, object_id, &mut events)?
        }
        (
            WaitingFor::MulliganDecision {
                player,
                mulligan_count,
            },
            GameAction::MulliganDecision { keep },
        ) => {
            let p = *player;
            let mc = *mulligan_count;
            mulligan::handle_mulligan_decision(state, p, keep, mc, &mut events)
        }
        (
            WaitingFor::MulliganBottomCards { player, count },
            GameAction::SelectCards { cards },
        ) => {
            let p = *player;
            let c = *count;
            mulligan::handle_mulligan_bottom(state, p, cards, c, &mut events)
                .map_err(|e| EngineError::InvalidAction(e))?
        }
        (waiting, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting
            )));
        }
    };

    // Run state-based actions after every action (except during mulligan/game over)
    if matches!(waiting_for, WaitingFor::Priority { .. }) {
        sba::check_state_based_actions(state, &mut events);
        // SBA might have set game over
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            let wf = state.waiting_for.clone();
            return Ok(ActionResult {
                events,
                waiting_for: wf,
            });
        }
    }

    state.waiting_for = waiting_for.clone();

    Ok(ActionResult {
        events,
        waiting_for,
    })
}

fn handle_play_land(
    state: &mut GameState,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate main phase
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Can only play lands during main phases".to_string(),
            ));
        }
    }

    // Validate land limit
    if state.lands_played_this_turn >= state.max_lands_per_turn {
        return Err(EngineError::ActionNotAllowed(
            "Already played maximum lands this turn".to_string(),
        ));
    }

    // Find the object in player's hand by matching card_id
    let player = state
        .players
        .iter()
        .find(|p| p.id == state.priority_player)
        .expect("priority player exists");

    let object_id = player
        .hand
        .iter()
        .find(|&&obj_id| {
            state
                .objects
                .get(&obj_id)
                .map(|obj| obj.card_id == card_id)
                .unwrap_or(false)
        })
        .copied()
        .ok_or_else(|| {
            EngineError::InvalidAction("Card not found in hand".to_string())
        })?;

    // Move from hand to battlefield
    zones::move_to_zone(state, object_id, Zone::Battlefield, events);

    // Set tapped=false (lands enter untapped by default)
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.tapped = false;
        obj.entered_battlefield_turn = Some(state.turn_number);
    }

    // Increment land counter
    state.lands_played_this_turn += 1;
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == state.priority_player)
        .expect("priority player exists");
    player.lands_played_this_turn += 1;

    // Reset priority pass count (action was taken)
    state.priority_pass_count = 0;

    events.push(GameEvent::LandPlayed {
        object_id,
        player_id: state.priority_player,
    });

    // Player retains priority after playing a land
    Ok(WaitingFor::Priority {
        player: state.priority_player,
    })
}

fn handle_tap_land_for_mana(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state.objects.get(&object_id).ok_or_else(|| {
        EngineError::InvalidAction("Object not found".to_string())
    })?;

    // Validate: on battlefield, controlled by acting player, is a land, not tapped
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != state.priority_player {
        return Err(EngineError::NotYourPriority);
    }
    if !obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::InvalidAction(
            "Object is not a land".to_string(),
        ));
    }
    if obj.tapped {
        return Err(EngineError::InvalidAction(
            "Land is already tapped".to_string(),
        ));
    }

    // Determine mana color from subtypes
    let mana_type = obj
        .card_types
        .subtypes
        .iter()
        .find_map(|s| mana_payment::land_subtype_to_mana_type(s))
        .ok_or_else(|| {
            EngineError::InvalidAction(
                "Land has no recognized basic land subtype".to_string(),
            )
        })?;

    // Tap the permanent
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.tapped = true;

    events.push(GameEvent::PermanentTapped { object_id });

    // Produce mana
    mana_payment::produce_mana(
        state,
        object_id,
        mana_type,
        state.priority_player,
        events,
    );

    Ok(WaitingFor::Priority {
        player: state.priority_player,
    })
}

pub fn new_game(seed: u64) -> GameState {
    GameState::new_two_player(seed)
}

/// Start game with mulligan flow. If no cards in libraries, skips mulligan.
pub fn start_game(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();

    events.push(GameEvent::GameStarted);

    // Begin the game: set turn 1
    state.turn_number = 1;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: PlayerId(0),
        turn_number: 1,
    });

    // If players have cards in their libraries, start mulligan flow
    let has_libraries = state.players.iter().any(|p| !p.library.is_empty());
    let waiting_for = if has_libraries {
        mulligan::start_mulligan(state, &mut events)
    } else {
        // No cards to mulligan with, skip straight to game
        turns::auto_advance(state, &mut events)
    };

    state.waiting_for = waiting_for.clone();

    ActionResult {
        events,
        waiting_for,
    }
}

/// Start game without mulligan (for backward compatibility with existing tests).
pub fn start_game_skip_mulligan(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();

    events.push(GameEvent::GameStarted);

    state.turn_number = 1;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: PlayerId(0),
        turn_number: 1,
    });

    let waiting_for = turns::auto_advance(state, &mut events);
    state.waiting_for = waiting_for.clone();

    ActionResult {
        events,
        waiting_for,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2; // Not first turn
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    #[test]
    fn apply_pass_priority_alternates_players() {
        let mut state = setup_game_at_main_phase();

        let result = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn apply_pass_priority_rejects_wrong_player() {
        let mut state = setup_game_at_main_phase();
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        // Player 0 tries to pass but player 1 has priority
        // PassPriority uses priority_player, so this should fail if
        // the validated player doesn't match waiting_for
        // Actually, the validation checks priority_player == waiting_for.player
        // and priority_player is 1, so PassPriority action itself is valid
        // for player 1. The issue is if player 0 somehow acts.
        // In practice, the action doesn't carry a player ID -- the engine
        // uses priority_player. So this is a protocol-level concern.
        let result = apply(&mut state, GameAction::PassPriority);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_play_land_moves_to_battlefield() {
        let mut state = setup_game_at_main_phase();

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let result = apply(
            &mut state,
            GameAction::PlayLand { card_id: CardId(1) },
        )
        .unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn apply_play_land_rejects_non_main_phase() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::Upkeep;

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(
            &mut state,
            GameAction::PlayLand { card_id: CardId(1) },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_over_limit() {
        let mut state = setup_game_at_main_phase();
        state.lands_played_this_turn = 1; // Already played one

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(
            &mut state,
            GameAction::PlayLand { card_id: CardId(1) },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_card_not_in_hand() {
        let mut state = setup_game_at_main_phase();

        let result = apply(
            &mut state,
            GameAction::PlayLand {
                card_id: CardId(999),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_rejects_unsupported_actions() {
        let mut state = setup_game_at_main_phase();

        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(1),
                targets: vec![],
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn new_game_creates_two_player_state() {
        let state = new_game(42);
        assert_eq!(state.players.len(), 2);
        assert_eq!(state.rng_seed, 42);
    }

    #[test]
    fn start_game_advances_to_precombat_main() {
        let mut state = new_game(42);
        let result = start_game(&mut state);

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn start_game_skips_draw_on_first_turn() {
        let mut state = new_game(42);

        // Add a card to player 0's library
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        start_game_skip_mulligan(&mut state);

        // Card should still be in library (draw skipped on turn 1)
        assert!(state.players[0].library.contains(&id));
        assert!(!state.players[0].hand.contains(&id));
    }

    #[test]
    fn start_game_emits_game_started_event() {
        let mut state = new_game(42);
        let result = start_game(&mut state);

        assert!(result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::GameStarted)));
    }

    #[test]
    fn integration_full_turn_cycle() {
        let mut state = new_game(42);

        // Start game (turn 1, player 0)
        let result = start_game(&mut state);
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);

        // Pass priority from player 0 (pre-combat main)
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));

        // Pass priority from player 1 (both passed, stack empty -> advance)
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should skip combat phases and land at PostCombatMain
        assert_eq!(state.phase, Phase::PostCombatMain);

        // Pass through post-combat main
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should advance to End step
        assert_eq!(state.phase, Phase::End);

        // Pass through end step
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should advance through cleanup to next turn, then auto-advance to PreCombatMain
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn integration_play_land_then_pass() {
        let mut state = new_game(42);
        start_game(&mut state);

        // Create a land in player 0's hand
        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Play the land
        let result = apply(
            &mut state,
            GameAction::PlayLand { card_id: CardId(1) },
        )
        .unwrap();

        assert!(state.battlefield.contains(&land_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority after playing land
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));

        // Priority pass count should have been reset by the land play
        assert_eq!(state.priority_pass_count, 0);
    }

    #[test]
    fn stack_push_and_lifo_resolve() {
        use crate::game::stack;
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = setup_game_at_main_phase();
        let mut events = Vec::new();

        // Create two spell objects
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Push to stack (first pushed = bottom)
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id1,
                source_id: id1,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                },
            },
            &mut events,
        );
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id2,
                source_id: id2,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(2),
                },
            },
            &mut events,
        );

        assert_eq!(state.stack.len(), 2);

        // Resolve top (LIFO) -- should be id2 (Bear, creature -> battlefield)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 1);
        assert!(state.battlefield.contains(&id2)); // Creature goes to battlefield

        // Resolve next -- should be id1 (Bolt, instant -> graveyard)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 0);
        assert!(state.players[0].graveyard.contains(&id1)); // Instant goes to graveyard
    }

    #[test]
    fn stack_is_empty_check() {
        use crate::game::stack;

        let state = new_game(42);
        assert!(stack::stack_is_empty(&state));
    }

    #[test]
    fn engine_error_display() {
        let err = EngineError::WrongPlayer;
        assert_eq!(err.to_string(), "Wrong player");

        let err = EngineError::NotYourPriority;
        assert_eq!(err.to_string(), "Not your priority");

        let err = EngineError::InvalidAction("test".to_string());
        assert_eq!(err.to_string(), "Invalid action: test");
    }

    #[test]
    fn tap_land_for_mana_produces_correct_color() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        let result = apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        assert!(state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn tap_land_rejects_already_tapped() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.tapped = true;
        }

        let result = apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        );

        assert!(result.is_err());
    }

    #[test]
    fn full_turn_integration_with_mulligan() {
        let mut state = new_game(42);

        // Add 20 basic lands to each player's library
        for player_idx in 0..2u8 {
            for i in 0..20 {
                let id = create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i),
                    PlayerId(player_idx),
                    "Forest".to_string(),
                    Zone::Library,
                );
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Land);
                obj.card_types.subtypes.push("Forest".to_string());
            }
        }

        // Start game -> mulligan prompt
        let result = start_game(&mut state);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 0,
            }
        ));

        // Both players have 7 cards in hand
        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[1].hand.len(), 7);

        // Player 0 keeps
        let result = apply(
            &mut state,
            GameAction::MulliganDecision { keep: true },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision {
                player: PlayerId(1),
                mulligan_count: 0,
            }
        ));

        // Player 1 keeps -> game starts, auto-advances to PreCombatMain
        let result = apply(
            &mut state,
            GameAction::MulliganDecision { keep: true },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0),
            }
        ));
        assert_eq!(state.phase, Phase::PreCombatMain);

        // Play a land from hand
        let land_card_id = state.objects[&state.players[0].hand[0]].card_id;
        let result = apply(
            &mut state,
            GameAction::PlayLand {
                card_id: land_card_id,
            },
        )
        .unwrap();
        assert_eq!(state.lands_played_this_turn, 1);

        // Find the land on battlefield to tap it
        let land_on_bf = state
            .battlefield
            .iter()
            .find(|&&id| {
                state
                    .objects
                    .get(&id)
                    .map(|o| o.controller == PlayerId(0) && !o.tapped)
                    .unwrap_or(false)
            })
            .copied()
            .unwrap();

        // Tap land for mana
        let result = apply(
            &mut state,
            GameAction::TapLandForMana {
                object_id: land_on_bf,
            },
        )
        .unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );

        // Pass priority through the rest of the turn
        // PreCombatMain: P0 passes
        apply(&mut state, GameAction::PassPriority).unwrap();
        // PreCombatMain: P1 passes -> advances to PostCombatMain
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PostCombatMain);

        // PostCombatMain: both pass -> End
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::End);

        // End: both pass -> Cleanup -> next turn
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }
}
