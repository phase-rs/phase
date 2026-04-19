use crate::types::ability::{
    CastingPermission, Effect, EffectError, EffectKind, PermissionGrantee, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::TrackedSetId;
use crate::types::player::PlayerId;

/// Grant a CastingPermission to the target object (CR 604.6).
///
/// Implements static abilities that modify where/how a card can be cast, such as
/// "You may cast this card from exile" (CR 604.6: static abilities that apply while
/// a card is in a zone you could cast it from). Building block for Airbending,
/// Foretell, Suspend, and similar "cast from exile" mechanics.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (permission, target_filter, grantee) = match &ability.effect {
        Effect::GrantCastingPermission {
            permission,
            target,
            grantee,
        } => (permission.clone(), target, *grantee),
        _ => return Err(EffectError::MissingParam("permission".to_string())),
    };

    let target_ids: Vec<_> = if ability.targets.is_empty() {
        match target_filter {
            TargetFilter::SelfRef | TargetFilter::Any | TargetFilter::None => {
                vec![ability.source_id]
            }
            TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            } => state
                .tracked_object_sets
                .iter()
                .max_by_key(|(id, _)| id.0)
                .map(|(_, objects)| objects.clone())
                .unwrap_or_default(),
            TargetFilter::TrackedSet { id } => state
                .tracked_object_sets
                .get(id)
                .cloned()
                .unwrap_or_default(),
            other => {
                // CR 107.3a + CR 601.2b: ability-context filter evaluation.
                let ctx = crate::game::filter::FilterContext::from_ability(ability);
                state
                    .objects
                    .keys()
                    .copied()
                    .filter(|obj_id| {
                        crate::game::filter::matches_target_filter(state, *obj_id, other, &ctx)
                    })
                    .collect()
            }
        }
    } else {
        ability
            .targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(obj_id) => Some(*obj_id),
                TargetRef::Player(_) => None,
            })
            .collect()
    };

    // CR 611.2a/b + CR 108.3: Resolve `grantee` to the `PlayerId` that a
    // `PlayFromExile` permission's `granted_to` should bind to. For
    // `ObjectOwner`, this varies per iterated object and is computed inside
    // the loop. For the other variants it is constant across iterations.
    let constant_grantee: Option<PlayerId> = match grantee {
        PermissionGrantee::AbilityController => Some(ability.controller),
        PermissionGrantee::ParentTargetController => ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(_) => None,
            })
            .or(Some(ability.controller)),
        PermissionGrantee::ObjectOwner => None, // per-iteration
    };

    for obj_id in target_ids {
        // Compute `granted_to` for this object. For `ObjectOwner` we read the
        // object's owner here so each iteration binds independently (CR 108.3).
        let granted_to_pid = constant_grantee.unwrap_or_else(|| {
            state
                .objects
                .get(&obj_id)
                .map(|o| o.owner)
                .unwrap_or(ability.controller)
        });
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            let mut granted = permission.clone();
            if let CastingPermission::PlayFromExile { granted_to, .. } = &mut granted {
                *granted_to = granted_to_pid;
            }
            obj.casting_permissions.push(granted);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}
