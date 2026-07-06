//! CR 311.7 / CR 901.9b: Resolver for `Effect::ChaosEnsues`.
//!
//! A resolving spell or ability that says "chaos ensues" makes the active
//! plane's "whenever chaos ensues" triggered ability trigger (CR 311.7). This
//! is the substitute the Fixed Point in Time planeswalk-replacement fires in
//! place of a planar-die planeswalk, but it is a first-class effect that any
//! resolving ability can invoke.

use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 311.7 / CR 901.9b: make chaos ensue for the active plane — delegates to
/// the shared `planechase::chaos_ensues` authority, which emits `ChaosEnsued`
/// keyed by the active plane so only its own chaos ability triggers.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    crate::game::planechase::chaos_ensues(state, events);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChaosEnsues,
        source_id: ability.source_id,
    });
    Ok(())
}
