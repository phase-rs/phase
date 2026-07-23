//! Damage-to-player assertions (CR 119 / CR 120 / CR 122).
//!
//! Creature marked-damage lives in [`super::combat`]; this module covers the
//! player-facing consequences of damage: life loss (CR 120.3a) and poison
//! counters from infect/toxic sources (CR 122.1 / CR 704.5c).
//!
//! Life loss from damage (CR 120.3a) is observed with the existing
//! `player_life` assertion — a source without infect causes the player to lose
//! that much life, so the post-condition is simply the reduced life total. No
//! separate "damage dealt" ledger read is required.

use engine::game::scenario::GameRunner;
use engine::types::player::PlayerId;

use super::AssertionFailure;

/// Assert a player's poison counter total (CR 122.1 poison counters; CR 704.5c
/// ten or more poison counters loses the game).
pub fn assert_player_poison(
    runner: &GameRunner,
    player: PlayerId,
    expected: u32,
) -> Result<(), AssertionFailure> {
    let actual = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.poison_counters)
        .ok_or_else(|| AssertionFailure {
            kind: "player_poison".into(),
            detail: format!("player {player:?} not found"),
        })?;
    if actual != expected {
        return Err(AssertionFailure {
            kind: "player_poison".into(),
            detail: format!("player {player:?}: expected poison {expected}, got {actual}"),
        });
    }
    Ok(())
}
