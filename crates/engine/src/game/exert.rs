//! Exert bookkeeping shared by every way a permanent is exerted (CR 701.43a).

use crate::types::ability::{
    ContinuousModification, Duration, PlayerScope, TargetFilter, TriggerDefinitionRef,
};
use crate::types::events::{ExertOrigin, GameEvent};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::resolved_commands::ResolvedObjectStatus;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::engine::EngineError;

/// CR 701.43a: Exert `object_id` on behalf of `actor` - the player who chose to
/// exert it, whether as an optional attack cost (CR 701.43d) or as part of an
/// activated ability's cost (CR 602.2b, CR 605.3b). Records the permanent as
/// exerted this turn, adds the "doesn't untap during your next untap step"
/// effect, and emits `CreatureExerted` so "whenever you exert" observers and the
/// linked "when you do" triggers see it.
///
/// CR 701.43b: a permanent that is already exerted can be exerted again; the
/// effects each last until the same untap step. The status is idempotent, so its
/// "changed" result is deliberately ignored: the effect and the event happen on
/// every exert (this engine reads "exert" in CR 701.43a as an event each time).
///
/// `origin` says which exert this was, so the attack-linked "when you do"
/// trigger (CR 607.2h) can refuse an exert paid as an activated ability's cost.
///
/// CR 701.43c: returns `Ok(None)` without effect if the permanent is not on the
/// battlefield.
pub(crate) fn exert_permanent(
    state: &mut GameState,
    object_id: ObjectId,
    actor: PlayerId,
    origin: ExertOrigin,
    attack_permission: Option<TriggerDefinitionRef>,
    events: &mut Vec<GameEvent>,
) -> Result<Option<GameEvent>, EngineError> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Ok(None);
    };
    if obj.zone != Zone::Battlefield {
        return Ok(None);
    }
    let controller = obj.controller;
    crate::game::object_state::resolve_and_apply_object_edit(
        state,
        object_id,
        ResolvedObjectStatus::Exerted,
        true,
    )
    .map_err(|err| EngineError::InvalidAction(err.to_string()))?;
    state.add_transient_continuous_effect(
        object_id,
        controller,
        Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: PlayerScope::Controller,
        },
        TargetFilter::SpecificObject { id: object_id },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantUntap,
        }],
        None,
    );
    let event = GameEvent::CreatureExerted {
        object_id,
        player: Some(actor),
        origin,
        attack_permission,
    };
    events.push(event.clone());
    Ok(Some(event))
}
