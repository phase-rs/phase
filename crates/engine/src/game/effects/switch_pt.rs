use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CR 613.4d: Switch a creature's power and toughness. Registers a transient
/// continuous effect in layer 7d so the swap survives layer recalculation and
/// expires at the correct time.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::SwitchPT { target } => target,
        _ => return Ok(()),
    };

    let dur = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);
    let target_filter = super::resolved_object_filter(ability, target_filter);

    // SelfRef with no explicit targets → switch the source object itself.
    let ids: Vec<ObjectId> =
        if matches!(target_filter, TargetFilter::SelfRef) && ability.targets.is_empty() {
            vec![ability.source_id]
        } else {
            ability
                .targets
                .iter()
                .filter_map(|t| {
                    if let TargetRef::Object(id) = t {
                        Some(*id)
                    } else {
                        None
                    }
                })
                .collect()
        };

    for obj_id in ids {
        // CR 608.2b: If a target has left the battlefield, skip it.
        if !state.battlefield.contains(&obj_id) {
            continue;
        }
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            dur.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::SwitchPowerToughness],
            None,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}
