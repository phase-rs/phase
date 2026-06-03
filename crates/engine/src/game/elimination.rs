use std::collections::HashSet;

use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::match_config::MatchPhase;
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
    eliminate_players_simultaneously(state, &[player], events);
}

/// CR 704.3 + CR 104.4a: Eliminate a set of players who lost in the SAME
/// state-based-action event.
///
/// All eliminations (and, for team formats, their teammate eliminations) are
/// applied BEFORE the single `check_game_over`, so the game-over check observes
/// the true post-event living set. When every remaining player is in the set
/// the result is a draw (`GameOver { winner: None }`) per CR 104.4a, rather than
/// crowning whichever player happened to be processed first. With a single loser
/// this is exactly the previous per-player behavior.
pub fn eliminate_players_simultaneously(
    state: &mut GameState,
    players_to_eliminate: &[PlayerId],
    events: &mut Vec<GameEvent>,
) {
    let mut eliminated_any = false;

    for &player in players_to_eliminate {
        // Skip if already eliminated (e.g. a teammate eliminated alongside an
        // earlier loser in this same batch).
        if !players::is_alive(state, player) {
            continue;
        }

        do_eliminate(state, player, events);
        eliminated_any = true;

        // For team-based formats, eliminate teammates too.
        if state.format_config.team_based {
            let team = players::teammates(state, player);
            for teammate in team {
                do_eliminate(state, teammate, events);
            }
        }
    }

    if !eliminated_any {
        return;
    }

    // CR 704.3 + CR 104.4a: a SINGLE game-over check after all simultaneous
    // eliminations — so a finish where every remaining player lost at once
    // resolves to a draw (`winner: None`) rather than a spurious winner.
    check_game_over(state, events);

    // CR 800.4a: If the active `WaitingFor` was waiting on any newly-eliminated
    // player (the conceder, or — for team formats — a teammate eliminated alongside
    // them), advance to `Priority` for the next living player so the game does not
    // deadlock waiting on a player who has left. Skip when the game just ended
    // (`GameOver` is terminal) or the waiting player is still alive.
    if !matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        // CR 103.5: For simultaneous mulligan states, prune eliminated players
        // from the pending list. If the list becomes empty, advance the flow
        // by emitting MulliganStarted-equivalent transition state.
        prune_mulligan_pending(state, events);

        // CR 603.3b + CR 800.4a: Resolve any in-flight trigger-ordering pass
        // around the elimination — drop triggers controlled by eliminated
        // players, auto-resolve their ordering groups with identity order,
        // and re-emit / advance the prompt as needed.
        prune_pending_trigger_order(state);

        if let Some(waiting_pid) = state.waiting_for.acting_player() {
            if !players::is_alive(state, waiting_pid) {
                let next = players::next_player(state, waiting_pid);
                state.waiting_for = WaitingFor::Priority { player: next };
            }
        }
    }
}

/// CR 103.5 + CR 800.4a: Prune eliminated players from in-flight mulligan
/// pending lists. If pruning empties the decision phase, transition to the
/// bottoms phase (or finish mulligans). If it empties the bottoms phase,
/// finish mulligans directly.
fn prune_mulligan_pending(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 800.4a: Drop any final-mulligan-count entries for players who have
    // been eliminated. Symmetric with the pending-list pruning below so
    // enter_bottom_phase never sees stale entries for dead players.
    let alive: HashSet<PlayerId> = state
        .final_mulligan_counts
        .keys()
        .chain(state.prepaid_mulligan_bottoms.keys())
        .copied()
        .filter(|pid| players::is_alive(state, *pid))
        .collect();
    state
        .final_mulligan_counts
        .retain(|pid, _| alive.contains(pid));
    state
        .prepaid_mulligan_bottoms
        .retain(|pid, _| alive.contains(pid));

    match state.waiting_for.clone() {
        WaitingFor::MulliganDecision {
            pending,
            free_first_mulligan,
        } => {
            let alive: Vec<_> = pending
                .into_iter()
                .filter(|e| players::is_alive(state, e.player))
                .collect();
            if alive.is_empty() {
                state.waiting_for = super::mulligan::enter_bottom_phase_public(state, events);
            } else {
                state.waiting_for = WaitingFor::MulliganDecision {
                    pending: alive,
                    free_first_mulligan,
                };
            }
        }
        WaitingFor::MulliganBottomCards { pending } => {
            let alive: Vec<_> = pending
                .into_iter()
                .filter(|e| players::is_alive(state, e.player))
                .collect();
            if alive.is_empty() {
                state.final_mulligan_counts.clear();
                state.prepaid_mulligan_bottoms.clear();
                state.waiting_for = super::mulligan::finish_mulligans_public(state, events);
            } else {
                state.waiting_for = WaitingFor::MulliganBottomCards { pending: alive };
            }
        }
        WaitingFor::OpeningHandBottomCards { pending, reason } => {
            let alive: Vec<_> = pending
                .into_iter()
                .filter(|e| players::is_alive(state, e.player))
                .collect();
            if alive.is_empty() {
                state.waiting_for = super::mulligan::enter_normal_mulligan_public(state);
            } else {
                state.waiting_for = WaitingFor::OpeningHandBottomCards {
                    pending: alive,
                    reason,
                };
            }
        }
        _ => {}
    }
}

/// CR 603.3b + CR 800.4a: Resolve an in-flight trigger-ordering pass when one
/// or more players have left the game. Triggers controlled by eliminated
/// players are dropped (CR 800.4a — abilities they would control are removed
/// from the queue / not placed). Groups for eliminated controllers are
/// auto-resolved with the identity order (an eliminated player makes no
/// choices). If the prompted group is the one being resolved, the
/// `WaitingFor::OrderTriggers` prompt is updated to point at the next-most-AP
/// unordered group; if every group becomes ordered, the pending ordering
/// pass is collapsed and the concatenated queue is stashed in
/// `state.deferred_triggers` so the next drain-site picks it up.
fn prune_pending_trigger_order(state: &mut GameState) {
    let living_players: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|player| !player.is_eliminated)
        .map(|player| player.id)
        .collect();
    let Some(order) = state.pending_trigger_order.as_mut() else {
        return;
    };

    // Drop triggers controlled by eliminated players and auto-resolve
    // eliminated controllers' groups with identity order.
    for group in order.groups.iter_mut() {
        if !living_players.contains(&group.controller) {
            // Identity order = current order; just mark as resolved.
            group.ordered = true;
        }
        // CR 800.4a: even within an alive controller's group, drop any
        // triggers whose own controller is now eliminated (delayed-trigger
        // re-attribution corner case — pre-elimination snapshot may have
        // triggers whose `pending.controller` belongs to a now-dead player).
        group
            .triggers
            .retain(|ctx| living_players.contains(&ctx.pending.controller));
        if group.triggers.len() <= 1 {
            group.ordered = true;
        }
    }
    // Drop groups whose controller is gone AND whose triggers were all dropped.
    order.groups.retain(|g| !g.triggers.is_empty());

    // If every group is now ordered, collapse the pending pass and stash
    // the concatenated queue into deferred_triggers so the next drain-site
    // (engine_stack, engine_resolution_choices) flushes it onto the stack.
    if order.groups.iter().all(|g| g.ordered) {
        let order = state.pending_trigger_order.take().expect("present above");
        let triggers: Vec<_> = order.groups.into_iter().flat_map(|g| g.triggers).collect();
        state.deferred_triggers.extend(triggers);
        // The waiting_for caller below (`acting_player()` is_alive check) will
        // re-point to a living player's Priority since OrderTriggers no longer
        // matches.
        return;
    }

    // Some groups still need a choice — refresh the OrderTriggers prompt so
    // it points at the next-most-AP unordered group (possibly the same one
    // if its controller is alive).
    if let Some(wf) = super::triggers::build_next_order_triggers_prompt_public(state) {
        state.waiting_for = wf;
    }
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

    // CR 800.4a: Exile permanents they own from the battlefield
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

    state.auto_pass.remove(&player);

    // CR 725.4: If the monarch leaves the game, the active player becomes the monarch.
    // If the active player is also leaving, the next living player in turn order gets it.
    if state.monarch == Some(player) {
        let any_alive = state
            .players
            .iter()
            .any(|p| !p.is_eliminated && p.id != player);

        if !any_alive {
            state.monarch = None;
        } else {
            // Prefer active player; fall back to next living in turn order.
            let new_monarch =
                if players::is_alive(state, state.active_player) && state.active_player != player {
                    state.active_player
                } else {
                    players::next_player(state, player)
                };
            state.monarch = Some(new_monarch);
            events.push(GameEvent::MonarchChanged {
                player_id: new_monarch,
            });
        }
    }

    // CR 725.4: If the player who has the initiative leaves the game,
    // the active player takes the initiative. If the active player is
    // also leaving, the next living player in turn order gets it.
    if state.initiative == Some(player) {
        let any_alive = state
            .players
            .iter()
            .any(|p| !p.is_eliminated && p.id != player);

        if !any_alive {
            state.initiative = None;
        } else {
            let new_holder =
                if players::is_alive(state, state.active_player) && state.active_player != player {
                    state.active_player
                } else {
                    players::next_player(state, player)
                };
            state.initiative = Some(new_holder);
            events.push(GameEvent::InitiativeTaken {
                player_id: new_holder,
            });
            // CR 725.2: "Whenever a player takes the initiative, that player ventures
            // into Undercity." Push as a pending trigger so it goes on the stack.
            let source_id = crate::game::dungeon::dungeon_sentinel_id(new_holder);
            let venture_ability = crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::VentureInto {
                    dungeon: crate::game::dungeon::DungeonId::Undercity,
                },
                vec![],
                source_id,
                new_holder,
            );
            crate::game::triggers::push_pending_trigger_to_stack(
                state,
                crate::game::triggers::PendingTrigger {
                    source_id,
                    controller: new_holder,
                    condition: None,
                    ability: venture_ability,
                    timestamp: 0,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: Some(GameEvent::InitiativeTaken {
                        player_id: new_holder,
                    }),
                    modal: None,
                    mode_abilities: vec![],
                    description: Some("Take the initiative — venture into Undercity".to_string()),
                    may_trigger_origin: None,
                    subject_match_count: None,
                    die_result: None,
                },
                events,
            );
        }
    }

    events.push(GameEvent::PlayerEliminated { player_id: player });
}

/// CR 104.2a: A player wins if all opponents have left. CR 104.3g: A team loses if all members have lost.
///
/// Check if the game should end. Game ends when 1 or fewer living players/teams remain.
fn check_game_over(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if state.match_phase != MatchPhase::InGame
        || matches!(state.waiting_for, WaitingFor::GameOver { .. })
    {
        return;
    }

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

/// Re-establish the CR 104 terminal-state invariant if an outer action path
/// overwrote the `WaitingFor::GameOver` produced by elimination.
pub(super) fn ensure_game_over_if_terminal(state: &mut GameState, events: &mut Vec<GameEvent>) {
    check_game_over(state, events);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
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

    // --- Simultaneous loss / draw (CR 104.4a + CR 704.3) ---

    #[test]
    fn simultaneous_two_player_loss_is_a_draw() {
        // CR 104.4a + CR 704.3: when all remaining players lose in a single SBA
        // event, the game is a DRAW (winner: None) — NOT a win for whichever
        // player happened to be processed first.
        let mut state = setup_two_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(&mut state, &[PlayerId(0), PlayerId(1)], &mut events);

        assert!(
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            "simultaneous loss of all players must be a draw, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn simultaneous_single_loss_has_sole_winner() {
        // Only one player loses → the other wins (single-loser behavior preserved).
        let mut state = setup_two_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(&mut state, &[PlayerId(1)], &mut events);

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "a single loser leaves the other player as sole winner, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn three_player_two_simultaneous_losses_leave_sole_winner() {
        // Two of three players die together; the lone survivor wins (not a draw).
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(&mut state, &[PlayerId(1), PlayerId(2)], &mut events);

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "two simultaneous losses with one survivor → that survivor wins, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn three_player_all_simultaneous_losses_is_a_draw() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(
            &mut state,
            &[PlayerId(0), PlayerId(1), PlayerId(2)],
            &mut events,
        );

        assert!(
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            "all players losing simultaneously is a draw, got {:?}",
            state.waiting_for
        );
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
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
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

    // --- Initiative transfer on elimination (CR 725.4) ---

    #[test]
    fn initiative_transfers_on_elimination() {
        let mut state = setup_three_player();
        state.active_player = PlayerId(0);
        state.initiative = Some(PlayerId(1));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        // CR 725.4: Active player (P0) takes the initiative.
        assert_eq!(state.initiative, Some(PlayerId(0)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::InitiativeTaken {
                player_id: PlayerId(0)
            }
        )));
        // CR 725.2: Venture into Undercity should be on the stack.
        assert!(
            !state.stack.is_empty(),
            "venture trigger should be pushed to stack"
        );
    }

    #[test]
    fn initiative_transfers_to_next_when_active_leaving() {
        let mut state = setup_three_player();
        state.active_player = PlayerId(0);
        state.initiative = Some(PlayerId(0));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        // CR 725.4: Active player is leaving, so next living player in turn order gets it.
        // P1 is next after P0 in a 3-player game.
        assert_eq!(state.initiative, Some(PlayerId(1)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::InitiativeTaken {
                player_id: PlayerId(1)
            }
        )));
    }

    #[test]
    fn initiative_transfers_in_two_player_game() {
        let mut state = setup_two_player();
        state.active_player = PlayerId(0);
        state.initiative = Some(PlayerId(0));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        // CR 725.4: P1 is still alive, so they get initiative (game ends immediately after).
        assert_eq!(state.initiative, Some(PlayerId(1)));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
    }
}
