use std::collections::{HashMap, HashSet};

use engine::game::engine::apply;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::log::GameLogEntry;
use engine::types::player::PlayerId;

use crate::config::AiConfig;
use crate::search::choose_action;

/// Maximum AI actions before forcing a stop (safety invariant — not CR-derived).
/// Typical AI sequences (mulligans + full turn) are 30–50 actions.
const MAX_AI_ACTIONS_PER_SEQUENCE: usize = 200;

/// Result of a single AI action: the action taken and the resulting events.
pub struct AiActionResult {
    pub action: GameAction,
    pub events: Vec<GameEvent>,
    pub log_entries: Vec<GameLogEntry>,
}

/// Run AI actions on the game state until the next actor is human or the game is over.
///
/// Returns one `AiActionResult` per AI action taken, preserving granularity for
/// the caller to broadcast individual state updates with animation timing.
///
/// # Arguments
/// * `state` — mutable game state (modified in place)
/// * `ai_players` — set of AI-controlled player IDs
/// * `ai_configs` — per-player AI configuration
///
/// CR 116.3 + CR 116.4: AI players receive and pass priority automatically.
/// The loop terminates when a non-AI player receives priority or the game ends.
pub fn run_ai_actions(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
) -> Vec<AiActionResult> {
    let mut results = Vec::new();
    let mut rng = rand::rng();

    for _ in 0..MAX_AI_ACTIONS_PER_SEQUENCE {
        let actor = match state.waiting_for.acting_player() {
            Some(p) if ai_players.contains(&p) => p,
            _ => break, // Human's turn or game over
        };

        let config = match ai_configs.get(&actor) {
            Some(c) => c,
            None => {
                tracing::warn!(player = ?actor, "AI seat has no config — stopping AI loop");
                break;
            }
        };

        let action = match choose_action(state, actor, config, &mut rng) {
            Some(a) => a,
            None => {
                tracing::warn!(player = ?actor, "choose_action returned None — stopping AI loop");
                break;
            }
        };

        match apply(state, action.clone()) {
            Ok(result) => {
                results.push(AiActionResult {
                    action,
                    events: result.events,
                    log_entries: result.log_entries,
                });
            }
            Err(e) => {
                tracing::error!(player = ?actor, error = %e, "AI action apply failed — stopping");
                break;
            }
        }
    }

    if results.len() >= MAX_AI_ACTIONS_PER_SEQUENCE {
        tracing::warn!(
            count = results.len(),
            "AI action loop hit safety cap — possible infinite loop"
        );
    }

    results
}
