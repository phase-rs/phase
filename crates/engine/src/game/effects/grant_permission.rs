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
            // CR 702.170a + CR 702.170d: `Plotted { turn_plotted }` is stamped
            // at grant-resolution time from `state.turn_number`, mirroring how
            // `PlayFromExile { granted_to }` is bound to a concrete `PlayerId`
            // above. Synthesized plot activations use placeholder `0`; the
            // real turn number is filled in here so the "later turn" gate in
            // `has_exile_cast_permission` reflects when the grant resolved.
            if let CastingPermission::Plotted { turn_plotted } = &mut granted {
                *turn_plotted = state.turn_number;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{CastingPermission, Duration, PlayerScope, TargetFilter};
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    /// CR 611.2a default: grantee defaults to the ability controller.
    #[test]
    fn grantee_ability_controller_binds_to_caster() {
        let mut state = GameState::new_two_player(1);
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::AbilityController,
            },
            vec![TargetRef::Object(target)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0), // caster
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&target];
        assert_eq!(obj.casting_permissions.len(), 1);
        match obj.casting_permissions[0] {
            CastingPermission::PlayFromExile { granted_to, .. } => {
                assert_eq!(granted_to, PlayerId(0), "granted_to should be the caster");
            }
            _ => panic!("expected PlayFromExile"),
        }
    }

    /// CR 108.3: `ObjectOwner` grants the permission to each iterated object's
    /// owner — powers Suspend Aggression's "its owner may play it".
    #[test]
    fn grantee_object_owner_binds_per_target_to_owner() {
        let mut state = GameState::new_two_player(1);
        let p0_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "MyCard".to_string(),
            Zone::Exile,
        );
        let p1_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "TheirCard".to_string(),
            Zone::Exile,
        );

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![TargetRef::Object(p0_card), TargetRef::Object(p1_card)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0), // caster — NOT the grantee
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for (obj_id, expected_owner) in [(p0_card, PlayerId(0)), (p1_card, PlayerId(1))] {
            let obj = &state.objects[&obj_id];
            assert_eq!(obj.casting_permissions.len(), 1);
            match obj.casting_permissions[0] {
                CastingPermission::PlayFromExile { granted_to, .. } => {
                    assert_eq!(
                        granted_to, expected_owner,
                        "granted_to should equal object owner"
                    );
                }
                _ => panic!("expected PlayFromExile"),
            }
        }
    }

    /// CR 109.4: `ParentTargetController` binds the grant to the first
    /// `TargetRef::Player` in the ability's targets — powers Expedited
    /// Inheritance's "its controller may ... they may play those cards".
    #[test]
    fn grantee_parent_target_controller_binds_to_player_target() {
        let mut state = GameState::new_two_player(1);
        let exiled = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Exile,
        );

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::ParentTargetController,
            },
            vec![TargetRef::Player(PlayerId(1)), TargetRef::Object(exiled)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&exiled];
        assert_eq!(obj.casting_permissions.len(), 1);
        match obj.casting_permissions[0] {
            CastingPermission::PlayFromExile { granted_to, .. } => {
                assert_eq!(
                    granted_to,
                    PlayerId(1),
                    "granted_to should equal the player target"
                );
            }
            _ => panic!("expected PlayFromExile"),
        }
    }
}
