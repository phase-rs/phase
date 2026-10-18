use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Digital-only Alchemy (no CR entry): `Effect::NoteNumber` — evaluate
/// `value` and record it as the resolving player's noted number
/// (`Player::noted_number`), overwriting any previous note ("note its
/// power", Dragonborn Immolator; "note that excess damage", Mephit's
/// Enthusiasm / Molten Impact). The sibling `CreateBoon` leg snapshots this
/// resolution's note (via the `noted_numbers_this_resolution` slot, never
/// the live global) into the granted ability at install time
/// (`SpellContext::boon_captured_noted_number`), so each boon reads what
/// its own resolution noted even after a later note overwrites the live
/// global; `QuantityRef::NotedNumber` ("where X is the noted number")
/// prefers that per-grant capture and falls back to the live global
/// outside a boon grant (or when the granting resolution noted nothing).
///
/// The noting player is `original_controller.unwrap_or(controller)` — the
/// same subject `resolve_quantity_with_targets` resolves `value` under —
/// so a note and its grant-time capture agree even under `player_scope`
/// fanout.
///
/// Doing the write at resolution — not when the trigger fires — means a
/// countered or otherwise removed-from-stack ability never notes anything
/// (CR 608.2c: instructions are followed only on resolution), mirroring
/// `note_mana_spent`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::NoteNumber { value } = &ability.effect else {
        return Err(EffectError::MissingParam("NoteNumber".to_string()));
    };

    let noted = resolve_quantity_with_targets(state, value, ability);
    let noting_player = ability.original_controller.unwrap_or(ability.controller);
    if let Some(player) = state.players.iter_mut().find(|p| p.id == noting_player) {
        player.noted_number = Some(noted);
    }
    // Resolution-local provenance for sibling grants: upsert this noting
    // player's entry (per-player keying keeps fan-out iterations apart).
    // Cleared at every top-level resolution, so a grant reads only a note
    // its own resolution wrote.
    match state
        .noted_numbers_this_resolution
        .iter_mut()
        .find(|(player, _)| *player == noting_player)
    {
        Some(slot) => slot.1 = noted,
        None => state
            .noted_numbers_this_resolution
            .push((noting_player, noted)),
    }

    Ok(())
}
