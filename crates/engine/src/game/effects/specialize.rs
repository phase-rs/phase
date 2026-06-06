use crate::game::specialize::{eligible_specialize_colors, specialize_permanent};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;

/// Digital-only Specialize: after the activation cost (including discard) is paid,
/// choose an eligible color and apply the matching specialized face.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(ability.effect, Effect::Specialize) {
        return Err(EffectError::InvalidParam(
            "expected Specialize effect".into(),
        ));
    }

    let source_id = ability.source_id;
    let player = ability.controller;

    let Some(snapshot) = ability.cost_paid_object.clone() else {
        return Err(EffectError::InvalidParam(
            "Specialize requires a discarded card snapshot".into(),
        ));
    };

    let available = state
        .objects
        .get(&source_id)
        .and_then(|o| o.specialize_faces.clone())
        .ok_or_else(|| {
            EffectError::InvalidParam("Permanent has no specialize faces loaded".into())
        })?;

    let options = eligible_specialize_colors(&snapshot.lki, &available);
    if options.is_empty() {
        return Err(EffectError::InvalidParam(
            "No legal specialization for the discarded card".into(),
        ));
    }

    if options.len() == 1 {
        specialize_permanent(state, source_id, options[0], events)
            .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Specialize,
            source_id,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::SpecializeColor {
        player,
        object_id: source_id,
        options,
    };
    Ok(())
}

/// Complete a `WaitingFor::SpecializeColor` prompt.
pub fn handle_choose_specialize_color(
    state: &mut GameState,
    _player: PlayerId,
    object_id: crate::types::identifiers::ObjectId,
    options: &[ManaColor],
    chosen: ManaColor,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::game::engine::EngineError> {
    if !options.contains(&chosen) {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Chosen color is not a legal specialization".into(),
        ));
    }
    specialize_permanent(state, object_id, chosen, events)?;
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Specialize,
        source_id: object_id,
    });
    Ok(())
}
