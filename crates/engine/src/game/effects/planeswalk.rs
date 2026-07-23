//! CR 701.31 / CR 901.8: Resolver for `Effect::Planeswalk`.
//!
//! Every `Effect::Planeswalk` resolution routes through
//! `planechase::resolve_planeswalk_via_replacements`. Phenomenon encounter and
//! SBA planeswalks use the same authority with [`PlaneswalkCause::RulesProcess`].

use crate::game::planechase::{self, PlaneswalkCause};
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 901.8 / CR 901.9c / CR 701.31: resolve a planeswalk effect — route through
/// the replacement pipeline, then perform the zone rotation on `Execute`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let cause = if planechase::is_planar_ability_source(ability.source_id) {
        PlaneswalkCause::PlanarDie
    } else {
        PlaneswalkCause::Instruction
    };
    let _ = planechase::resolve_planeswalk_via_replacements(
        state,
        ability.controller,
        cause,
        ability.replacement_applied.clone(),
        events,
    );
    Ok(())
}
