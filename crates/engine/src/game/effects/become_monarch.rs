use crate::types::ability::{EffectError, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 725: Become the monarch.
///
/// CR 725.3: Only one player can be the monarch at a time. As a player becomes
/// the monarch, the current monarch ceases to be the monarch.
///
/// `target` is the CR 109.5 subject printed on the clause, resolved through
/// [`super::resolve_player_for_context_ref`] — the same single authority
/// `Effect::Draw`, `Effect::Mill` and every other "target player does X" effect
/// uses. A context-ref filter (`Controller`, "you become the monarch") answers
/// the controller; a real target filter ("target opponent becomes the monarch"
/// — M'Baku, Jabari Chieftain; Garland, Royal Kidnapper; Jared Carthalion, True
/// Heir; Éomer, King of Rohan; Denethor, Stone Seer) reads the player chosen
/// into `ability.targets` at announcement (CR 115.1).
///
/// Before the subject axis existed this read `ability.controller`
/// unconditionally, so every targeted printing crowned its own controller — the
/// one player those clauses exist to deny.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 725.1: The monarch is a designation a player can have.
    let player_id = super::resolve_player_for_context_ref(state, ability, target);
    state.monarch = Some(player_id);
    events.push(GameEvent::MonarchChanged { player_id });
    Ok(())
}
