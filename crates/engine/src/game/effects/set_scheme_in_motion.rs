//! CR 701.32b / CR 904.9: Resolver for `Effect::SetSchemeInMotion`.
//!
//! Every `Effect::SetSchemeInMotion` resolution routes through
//! `archenemy::set_in_motion_again`, which re-fires a face-up scheme's "when
//! you set this scheme in motion" abilities by emitting a fresh
//! `SchemeSetInMotion` event (My Laughter Echoes: "set that scheme in motion
//! again").

use crate::game::archenemy;
use crate::game::targeting::resolve_event_context_target;
use crate::types::ability::{Effect, EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.32b / CR 904.9: resolve a re-fire effect — emit a fresh
/// `SchemeSetInMotion` event for the event-context scheme ("that scheme" is
/// the scheme the triggering event set in motion, read via
/// `resolve_event_context_target`). No-op when the event names no scheme or
/// the scheme is no longer face up in the command zone.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::SetSchemeInMotion { target } = &ability.effect else {
        return Err(EffectError::MissingParam(
            "SetSchemeInMotion target".to_string(),
        ));
    };
    let scheme_id =
        resolve_event_context_target(state, target, ability.source_id).and_then(|target_ref| {
            match target_ref {
                TargetRef::Object(id) => Some(id),
                TargetRef::Player(_) => None,
            }
        });
    let Some(scheme_id) = scheme_id else {
        return Ok(());
    };
    archenemy::set_in_motion_again(state, scheme_id, ability.controller, events);
    Ok(())
}
