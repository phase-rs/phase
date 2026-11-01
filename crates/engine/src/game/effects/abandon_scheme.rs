//! CR 701.33 / CR 904.11: Resolver for `Effect::AbandonScheme`.
//!
//! Every `Effect::AbandonScheme` resolution routes through
//! `archenemy::abandon`, which self-collects the scheme's "when you abandon
//! this scheme" trigger while the scheme is still face up (CR 314.4 /
//! CR 904.8) and then turns it face down onto the bottom of its owner's
//! scheme deck (CR 701.33b).

use crate::game::archenemy;
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.33a-b / CR 904.11: resolve an abandon effect — abandon the scheme
/// that sourced this ability. No-op unless the source is a face-up ongoing
/// scheme (`archenemy::abandon` enforces CR 701.33a).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    archenemy::abandon(state, ability.source_id, events);
    Ok(())
}
