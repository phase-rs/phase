use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;

use super::turns;

pub fn handle_priority_pass(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    state.priority_pass_count += 1;

    if state.priority_pass_count >= 2 {
        // Both players have passed consecutively
        if state.stack.is_empty() {
            // Empty stack: advance to next phase
            turns::advance_phase(state, events);
            turns::auto_advance(state, events)
        } else {
            // Non-empty stack: resolve top of stack
            super::stack::resolve_top(state, events);
            reset_priority(state);
            WaitingFor::Priority {
                player: state.active_player,
            }
        }
    } else {
        // Only one player passed, give priority to opponent
        let opp = opponent(state.priority_player, state);
        state.priority_player = opp;

        events.push(GameEvent::PriorityPassed {
            player_id: state.priority_player,
        });

        WaitingFor::Priority { player: opp }
    }
}

pub fn reset_priority(state: &mut GameState) {
    state.priority_player = state.active_player;
    state.priority_pass_count = 0;
}

pub fn opponent(player: PlayerId, _state: &GameState) -> PlayerId {
    PlayerId(1 - player.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ResolvedAbility;
    use crate::types::game_state::StackEntry;
    use crate::types::identifiers::CardId;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_pass_count = 0;
        state
    }

    #[test]
    fn single_pass_gives_priority_to_opponent() {
        let mut state = setup();
        let mut events = Vec::new();

        let result = handle_priority_pass(&mut state, &mut events);

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        assert_eq!(state.priority_player, PlayerId(1));
        assert_eq!(state.priority_pass_count, 1);
    }

    #[test]
    fn both_pass_empty_stack_advances_phase() {
        let mut state = setup();
        state.priority_pass_count = 1; // First player already passed
        state.priority_player = PlayerId(1);

        let mut events = Vec::new();
        let result = handle_priority_pass(&mut state, &mut events);

        // Should advance past combat to PostCombatMain
        assert!(matches!(result, WaitingFor::Priority { .. }));
    }

    #[test]
    fn both_pass_non_empty_stack_resolves_top() {
        let mut state = setup();
        state.priority_pass_count = 1;
        state.priority_player = PlayerId(1);

        // Create the object in state so resolve_top can find it
        use crate::game::zones::create_object;
        use crate::types::zones::Zone;
        let created_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );

        // Add a stack entry matching the created object
        state.stack.push(StackEntry {
            id: created_id,
            source_id: created_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(1),
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    created_id,
                    PlayerId(0),
                ),
            },
        });

        // Mark as instant/sorcery so it goes to graveyard
        state
            .objects
            .get_mut(&created_id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Instant);

        let mut events = Vec::new();
        let result = handle_priority_pass(&mut state, &mut events);

        // Priority should reset to active player
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.priority_pass_count, 0);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn priority_resets_to_active_player() {
        let mut state = setup();
        state.priority_player = PlayerId(1);
        state.priority_pass_count = 3;

        reset_priority(&mut state);

        assert_eq!(state.priority_player, PlayerId(0));
        assert_eq!(state.priority_pass_count, 0);
    }

    #[test]
    fn opponent_returns_other_player() {
        let state = setup();
        assert_eq!(opponent(PlayerId(0), &state), PlayerId(1));
        assert_eq!(opponent(PlayerId(1), &state), PlayerId(0));
    }
}
