use crate::types::events::GameEvent;
use crate::types::game_state::{AutoPassMode, GameState, WaitingFor};
use crate::types::player::PlayerId;

use super::players;
use super::turns;

/// Handle a priority pass from the current priority player (CR 117.4).
///
/// Uses a BTreeSet (priority_passes) to track which players have passed consecutively.
/// CR 117.4: When all players pass in succession, the top object on the stack resolves
/// (or the phase advances if the stack is empty).
/// Any non-pass action clears the set (handled by callers via `reset_priority`).
pub fn handle_priority_pass(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    // Record this player's pass
    let current = state.priority_player;
    state.priority_passes.insert(current);

    // Also maintain legacy counter for transition period
    state.priority_pass_count += 1;

    // CR 800.4: Eliminated players are excluded from priority passing.
    let living_count = state.players.iter().filter(|p| !p.is_eliminated).count();

    if state.priority_passes.len() >= living_count {
        // CR 117.4: All living players have passed consecutively.
        state.priority_passes.clear();
        state.priority_pass_count = 0;

        if state.stack.is_empty() {
            // CR 117.4: Empty stack — advance to next phase.
            turns::advance_phase(state, events);
            turns::auto_advance(state, events)
        } else {
            // CR 117.4: Non-empty stack — resolve top object.
            super::stack::resolve_top(state, events);

            // After resolve_top: the stack should have shrunk by 1.
            // Update auto-pass baselines so trigger-growth detection works across apply() calls.
            for mode in state.auto_pass.values_mut() {
                if let AutoPassMode::UntilStackEmpty { initial_stack_len } = mode {
                    *initial_stack_len = initial_stack_len.saturating_sub(1);
                }
            }

            // If resolve_top set an interactive WaitingFor (e.g. RevealChoice,
            // ScryChoice, SearchChoice), preserve it instead of overwriting
            // with Priority. Only reset to Priority if the effect didn't
            // request player interaction.
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                reset_priority(state);
                WaitingFor::Priority {
                    player: state.active_player,
                }
            } else {
                state.waiting_for.clone()
            }
        }
    } else {
        // CR 117.3d: Player passed; priority moves to next player in turn order.
        let next = next_priority_player(state);
        state.priority_player = next;

        events.push(GameEvent::PriorityPassed { player_id: next });

        WaitingFor::Priority { player: next }
    }
}

/// Determine the next player to receive priority, using APNAP order (CR 101.4).
///
/// For non-team formats: next living player in seat order after current priority player.
/// For team-based formats (2HG): CR 101.4 APNAP within teams — active team members first,
/// then opponent team members.
fn next_priority_player(state: &GameState) -> PlayerId {
    if state.format_config.team_based {
        // 2HG: APNAP order within teams
        // Build the full APNAP order and find the next player who hasn't passed
        let order = players::apnap_order(state);
        let current_idx = order
            .iter()
            .position(|&id| id == state.priority_player)
            .unwrap_or(0);
        for offset in 1..=order.len() {
            let idx = (current_idx + offset) % order.len();
            let candidate = order[idx];
            if !state.priority_passes.contains(&candidate) {
                return candidate;
            }
        }
        // Fallback (shouldn't reach here if called before all have passed)
        players::next_player(state, state.priority_player)
    } else {
        // Non-team: simple clockwise in seat order
        players::next_player(state, state.priority_player)
    }
}

/// CR 117.3a: After resolution, active player receives priority.
/// Reset priority state: clear passes, set priority to active player.
pub fn reset_priority(state: &mut GameState) {
    state.priority_player = state.active_player;
    state.priority_passes.clear();
    state.priority_pass_count = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ResolvedAbility;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{CastingVariant, StackEntry};
    use crate::types::identifiers::CardId;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_pass_count = 0;
        state.priority_passes.clear();
        state
    }

    fn setup_three_player() -> GameState {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_passes.clear();
        state
    }

    // --- 2-player backward compatibility ---

    #[test]
    fn two_player_single_pass_gives_priority_to_opponent() {
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
        assert!(state.priority_passes.contains(&PlayerId(0)));
    }

    #[test]
    fn two_player_both_pass_empty_stack_advances_phase() {
        let mut state = setup();
        state.priority_passes.insert(PlayerId(0));
        state.priority_pass_count = 1;
        state.priority_player = PlayerId(1);

        let mut events = Vec::new();
        let result = handle_priority_pass(&mut state, &mut events);

        // Should advance past combat to PostCombatMain
        assert!(matches!(result, WaitingFor::Priority { .. }));
    }

    #[test]
    fn two_player_both_pass_non_empty_stack_resolves_top() {
        let mut state = setup();
        state.priority_passes.insert(PlayerId(0));
        state.priority_pass_count = 1;
        state.priority_player = PlayerId(1);

        use crate::game::zones::create_object;
        use crate::types::zones::Zone;
        let created_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );

        state.stack.push_back(StackEntry {
            id: created_id,
            source_id: created_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        state
            .objects
            .get_mut(&created_id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Instant);

        let mut events = Vec::new();
        let result = handle_priority_pass(&mut state, &mut events);

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(state.priority_passes.is_empty());
        assert!(state.stack.is_empty());
    }

    #[test]
    fn priority_resets_to_active_player() {
        let mut state = setup();
        state.priority_player = PlayerId(1);
        state.priority_passes.insert(PlayerId(0));
        state.priority_passes.insert(PlayerId(1));

        reset_priority(&mut state);

        assert_eq!(state.priority_player, PlayerId(0));
        assert!(state.priority_passes.is_empty());
        assert_eq!(state.priority_pass_count, 0);
    }

    // --- 3-player N-player priority ---

    #[test]
    fn three_player_first_pass_does_not_resolve_stack() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        let result = handle_priority_pass(&mut state, &mut events);

        // P0 passes, priority goes to P1
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        assert_eq!(state.priority_player, PlayerId(1));
        assert_eq!(state.priority_passes.len(), 1);
    }

    #[test]
    fn three_player_two_passes_does_not_resolve_stack() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        // P0 passes
        handle_priority_pass(&mut state, &mut events);
        // P1 passes
        let result = handle_priority_pass(&mut state, &mut events);

        // Still not all 3 have passed, priority goes to P2
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ));
        assert_eq!(state.priority_passes.len(), 2);
    }

    #[test]
    fn three_player_all_pass_advances_phase() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        // P0 passes
        handle_priority_pass(&mut state, &mut events);
        // P1 passes
        handle_priority_pass(&mut state, &mut events);
        // P2 passes - all 3 have passed
        let result = handle_priority_pass(&mut state, &mut events);

        // Should advance phase (empty stack)
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert!(state.priority_passes.is_empty());
    }

    #[test]
    fn three_player_action_clears_priority_passes() {
        let mut state = setup_three_player();
        state.priority_passes.insert(PlayerId(0));
        state.priority_passes.insert(PlayerId(1));

        // Simulate an action resetting priority
        reset_priority(&mut state);

        assert!(state.priority_passes.is_empty());
        assert_eq!(state.priority_player, PlayerId(0));
    }

    #[test]
    fn three_player_skips_eliminated_player() {
        let mut state = setup_three_player();
        // Eliminate P1
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));
        let mut events = Vec::new();

        // P0 passes
        let result = handle_priority_pass(&mut state, &mut events);

        // Should skip P1 and go to P2
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ));
    }

    #[test]
    fn three_player_two_living_all_pass_resolves() {
        let mut state = setup_three_player();
        // Eliminate P1
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));
        let mut events = Vec::new();

        // P0 passes -> P2
        handle_priority_pass(&mut state, &mut events);
        // P2 passes -> both living players passed
        let result = handle_priority_pass(&mut state, &mut events);

        // Should advance phase (2 living players both passed)
        assert!(matches!(result, WaitingFor::Priority { .. }));
    }

    // --- 2HG team-based priority ---

    #[test]
    fn two_hg_priority_uses_apnap_order() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_passes.clear();
        let mut events = Vec::new();

        // P0 (active team member) passes
        let result = handle_priority_pass(&mut state, &mut events);

        // In APNAP order with P0 active: P0, P1 (teammate), P2, P3
        // Next should be P1 (teammate on active team)
        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn two_hg_all_four_pass_resolves() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.priority_passes.clear();
        let mut events = Vec::new();

        // All 4 pass in APNAP order
        handle_priority_pass(&mut state, &mut events); // P0
        handle_priority_pass(&mut state, &mut events); // P1
        handle_priority_pass(&mut state, &mut events); // P2
        let result = handle_priority_pass(&mut state, &mut events); // P3

        // All passed, should advance
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert!(state.priority_passes.is_empty());
    }

    #[test]
    fn resolve_preserves_interactive_waiting_for() {
        use crate::game::zones::create_object;
        use crate::types::ability::{Effect, TargetFilter, TargetRef};
        use crate::types::zones::Zone;

        let mut state = setup();
        state.priority_passes.insert(PlayerId(0));
        state.priority_pass_count = 1;
        state.priority_player = PlayerId(1);

        // Create a triggered ability on the stack with RevealHand effect
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Battlefield,
        );

        // Add a card to opponent's hand so RevealChoice is meaningful
        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        let _ = hand_card;

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
                count: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source_id,
            PlayerId(0),
        );

        state.stack.push_back(StackEntry {
            id: source_id,
            source_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
            },
        });

        let mut events = Vec::new();
        let result = handle_priority_pass(&mut state, &mut events);

        // RevealHand should set RevealChoice, and priority pass should preserve it
        assert!(
            matches!(result, WaitingFor::RevealChoice { .. }),
            "Expected RevealChoice, got {:?}",
            result
        );
    }
}
