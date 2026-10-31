//! Turtle Team-Up (Savanti Romero, Temporal Rogue): "the bosses play a card".
//!
//! Reveal and resolve the top `count` cards of the Boss event deck immediately,
//! outside the wave (Savanti triggers on combat damage, mid-combat). Not a
//! cast: the shared single-card wave driver (`horde::play_top_cards`) resolves
//! each card exactly as the Action-phase wave would — creatures enter,
//! Events resolve one-shot — so this resolver is a thin entry shim.
use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::HordePlayCard { count } = &ability.effect else {
        return Err(EffectError::MissingParam("HordePlayCard count".to_string()));
    };
    crate::game::horde::play_top_cards(state, ability.controller, *count, events);
    Ok(())
}
