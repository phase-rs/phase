//! CR 809: Emperor Variant — asymmetric team win/loss.
//!
//! CR 809.1-809.2: two or more teams of three players each, one emperor per
//! team, the remaining players are generals whose job is to protect the
//! emperor. This module is the runtime sibling of `game::archenemy` and
//! `game::planechase`: unlike those, Emperor introduces no new zone or
//! command-zone object — it is entirely a topology + win/loss overlay on top
//! of `FormatTopology::FixedTeams { team_size: 3, .. }` (see
//! `FormatConfig::topology`), so this module owns only the one thing that
//! topology can't express generically: WHICH team member is the emperor, and
//! the asymmetric CR 809.5a-c win/loss consequence that follows from that.
//!
//! CR 809.4: Emperor uses INDIVIDUAL turns, not Two-Headed-Giant-style shared
//! team turns — `FormatConfig::topology()` sets
//! `TurnStructure::IndividualTurns`, and `game::topology`'s
//! `has_shared_team_turns()`-gated functions (`next_turn_representative`,
//! `priority_pass_representative`, `apnap_choice_groups_from`) already route
//! that case through the plain individual-player path with zero changes.
//!
//! Deferred (documented, not silently dropped): CR 804 (Deploy Creatures
//! Option, a separable CR 809.3b default), CR 809.3c (attack-only-the-
//! adjacent-seat restriction) — CR 809.2 places the emperor "in the middle"
//! of a team that "sits together on one side of the table," implying real
//! Emperor games use a seating geometry (two teams facing each other across
//! a table) this engine's sequential `seat_order` model does not represent. A
//! literal circular-seat-adjacency reading would leave the middle seat of
//! each 3-player team — where CR 809.2 places the emperor — with zero
//! adjacent opponents, which cannot be the intended behavior; rather than
//! guess at an unverifiable geometry, Emperor players are subject only to
//! their CR 801.3 range-of-influence restriction on attacks (asymmetric per
//! CR 809.3a) in this pass, not the additional 809.3c narrowing. CR
//! 809.6/809.6a (team sizes other than 3, team counts other than 2 — v1
//! ships the simplest legal case, two teams of three, per CR 809.1's own
//! example).

use crate::types::format::GameFormat;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

/// CR 809.2: `player` is the designated emperor of their team.
pub(crate) fn is_emperor(state: &GameState, player: PlayerId) -> bool {
    state.format_config.format == GameFormat::Emperor
        && state.format_config.emperor_players.contains(&player)
}

/// CR 809.5b + CR 810.8a: does eliminating `player` cascade to their whole
/// team? Unconditionally true for Two-Headed Giant (unchanged from the
/// existing symmetric model — any teammate's loss ends the team). For
/// Emperor, true only when `player` is specifically the emperor: a general's
/// individual loss does NOT end their team's game.
pub(crate) fn team_elimination_cascades_from(state: &GameState, player: PlayerId) -> bool {
    super::topology::has_two_headed_giant_shared_resources(state) || is_emperor(state, player)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::format::FormatConfig;

    #[test]
    fn is_emperor_true_only_for_designated_emperors() {
        let state = GameState::new(FormatConfig::emperor(), 6, 42);
        for &emperor in &state.format_config.emperor_players.clone() {
            assert!(is_emperor(&state, emperor));
        }
        let generals: Vec<PlayerId> = (0..6)
            .map(PlayerId)
            .filter(|p| !state.format_config.emperor_players.contains(p))
            .collect();
        for general in generals {
            assert!(!is_emperor(&state, general));
        }
    }

    #[test]
    fn is_emperor_false_outside_emperor_format() {
        let state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        assert!(!is_emperor(&state, PlayerId(0)));
    }

    #[test]
    fn team_elimination_cascades_only_from_emperors_not_generals() {
        let state = GameState::new(FormatConfig::emperor(), 6, 42);
        let emperors = state.format_config.emperor_players.clone();
        for &emperor in &emperors {
            assert!(team_elimination_cascades_from(&state, emperor));
        }
        let generals: Vec<PlayerId> = (0..6)
            .map(PlayerId)
            .filter(|p| !emperors.contains(p))
            .collect();
        for general in generals {
            assert!(!team_elimination_cascades_from(&state, general));
        }
    }

    /// Regression: 2HG's unconditional cascade (every player, teammate or
    /// not) must be byte-for-byte unchanged by the new predicate.
    #[test]
    fn team_elimination_cascades_unconditionally_for_two_headed_giant() {
        let state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        for player_idx in 0..4u8 {
            assert!(team_elimination_cascades_from(&state, PlayerId(player_idx)));
        }
    }
}
