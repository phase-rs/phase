use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::players;

/// Eliminate a player from the game per CR 800.4.
///
/// - Marks the player as eliminated
/// - Removes their spells from the stack
/// - Exiles all permanents they own on the battlefield
/// - Emits PlayerEliminated event
/// - For team-based formats (2HG): also eliminates all teammates
/// - Checks if the game is over (1 or fewer living players/teams remain)
pub fn eliminate_player(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    // Skip if already eliminated
    if state
        .players
        .iter()
        .any(|p| p.id == player && p.is_eliminated)
    {
        return;
    }

    do_eliminate(state, player, events);

    // For team-based formats, eliminate teammates too
    if state.format_config.team_based {
        let team = players::teammates(state, player);
        for teammate in team {
            if !state
                .players
                .iter()
                .any(|p| p.id == teammate && p.is_eliminated)
            {
                do_eliminate(state, teammate, events);
            }
        }
    }

    // Check if game is over
    check_game_over(state, events);
}

/// Perform the actual elimination of a single player (CR 800.4).
fn do_eliminate(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    // Mark as eliminated
    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
        p.is_eliminated = true;
    }
    if !state.eliminated_players.contains(&player) {
        state.eliminated_players.push(player);
    }

    // CR 800.4a: Remove spells they control from the stack
    state.stack.retain(|entry| entry.controller != player);

    // CR 800.4b: Exile permanents they own from the battlefield
    let to_exile: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.owner == player)
                .unwrap_or(false)
        })
        .collect();

    for id in to_exile {
        super::zones::move_to_zone(state, id, Zone::Exile, events);
    }

    events.push(GameEvent::PlayerEliminated { player_id: player });
}

/// Check if the game should end. Game ends when 1 or fewer living players/teams remain.
fn check_game_over(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let living: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .collect();

    if state.format_config.team_based {
        // Count living teams (team = pair of players with same team index)
        let mut living_teams = std::collections::HashSet::new();
        for &pid in &living {
            let team_idx = pid.0 / 2;
            living_teams.insert(team_idx);
        }

        if living_teams.len() <= 1 {
            let winner = if living.len() == 1 {
                Some(living[0])
            } else if living.len() > 1 {
                // Multiple living players on one team — pick the first
                Some(living[0])
            } else {
                None // draw
            };
            events.push(GameEvent::GameOver { winner });
            state.waiting_for = WaitingFor::GameOver { winner };
        }
    } else {
        // Non-team: game over when 0 or 1 living players
        if living.len() <= 1 {
            let winner = living.first().copied();
            events.push(GameEvent::GameOver { winner });
            state.waiting_for = WaitingFor::GameOver { winner };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::ResolvedAbility;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{StackEntry, StackEntryKind};
    use crate::types::identifiers::CardId;

    fn setup_two_player() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state
    }

    fn setup_three_player() -> GameState {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 1;
        state
    }

    fn setup_2hg() -> GameState {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        state
    }

    // --- 2-player elimination (immediate GameOver) ---

    #[test]
    fn two_player_elimination_ends_game() {
        let mut state = setup_two_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(state.players[0].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerEliminated {
                player_id: PlayerId(0)
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::GameOver {
                winner: Some(PlayerId(1))
            }
        )));
    }

    // --- 3-player elimination (game continues) ---

    #[test]
    fn three_player_elimination_game_continues() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(state.players[1].is_eliminated);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerEliminated {
                player_id: PlayerId(1)
            }
        )));
        // Game should NOT be over — 2 players still alive
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn three_player_two_eliminations_ends_game() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);
        eliminate_player(&mut state, PlayerId(2), &mut events);

        // Now only P0 remains — game over
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    // --- Elimination cleanup ---

    #[test]
    fn elimination_removes_spells_from_stack() {
        let mut state = setup_two_player();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    obj_id,
                    PlayerId(0),
                ),
                cast_as_adventure: false,
            },
        });

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(state.stack.is_empty());
    }

    #[test]
    fn elimination_exiles_owned_permanents() {
        let mut state = setup_three_player();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        // Permanent should be exiled, not on battlefield
        assert!(!state.battlefield.contains(&id));
        assert!(state.exile.contains(&id));
    }

    #[test]
    fn elimination_skips_already_eliminated_player() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);
        let event_count = events.len();

        // Try to eliminate again
        eliminate_player(&mut state, PlayerId(1), &mut events);

        // No new events should be emitted
        assert_eq!(events.len(), event_count);
    }

    // --- Simultaneous elimination ---

    #[test]
    fn simultaneous_elimination_multiple_players() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        // Eliminate P1 and P2 simultaneously
        eliminate_player(&mut state, PlayerId(1), &mut events);
        // After P1 eliminated, game still goes (P0 and P2 alive)
        // Now eliminate P2
        eliminate_player(&mut state, PlayerId(2), &mut events);

        assert!(state.players[1].is_eliminated);
        assert!(state.players[2].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    // --- 2HG team elimination ---

    #[test]
    fn two_hg_eliminating_one_teammate_eliminates_both() {
        let mut state = setup_2hg();
        let mut events = Vec::new();

        // Eliminate P0 (team A)
        eliminate_player(&mut state, PlayerId(0), &mut events);

        // Both P0 and P1 (team A) should be eliminated
        assert!(state.players[0].is_eliminated);
        assert!(state.players[1].is_eliminated);

        // Team B wins
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver { winner: Some(_) }
        ));
    }

    #[test]
    fn two_hg_team_b_elimination() {
        let mut state = setup_2hg();
        let mut events = Vec::new();

        // Eliminate P2 (team B)
        eliminate_player(&mut state, PlayerId(2), &mut events);

        // Both P2 and P3 (team B) should be eliminated
        assert!(state.players[2].is_eliminated);
        assert!(state.players[3].is_eliminated);

        // Team A wins (P0 is first living player)
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn eliminated_player_added_to_eliminated_list() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(state.eliminated_players.contains(&PlayerId(1)));
    }
}
