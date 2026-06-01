use crate::types::ability::{
    Effect, EffectError, EffectKind, GameRestriction, ResolvedAbility, RestrictionExpiry,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 614.16: Add a game-level restriction to the game state.
/// The restriction modifies how rules are applied (e.g., disabling damage prevention).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if let Effect::AddRestriction { restriction } = &ability.effect {
        let mut restriction = restriction.clone();
        fill_runtime_fields(&mut restriction, ability);
        state.restrictions.push(restriction);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AddRestriction,
            source_id: ability.source_id,
        });
        Ok(())
    } else {
        Err(EffectError::MissingParam(
            "AddRestriction restriction".to_string(),
        ))
    }
}

/// Fill runtime-bound fields of a restriction using the resolving ability context.
fn fill_runtime_fields(restriction: &mut GameRestriction, ability: &ResolvedAbility) {
    match restriction {
        GameRestriction::DamagePreventionDisabled { source, .. }
        | GameRestriction::ProhibitActivity { source, .. } => {
            *source = ability.source_id;
        }
    }

    let resolved_target_player = ability.target_player();

    match restriction {
        GameRestriction::ProhibitActivity {
            affected_players, ..
        } => {
            if matches!(
                affected_players,
                crate::types::ability::RestrictionPlayerScope::TargetedPlayer
                    | crate::types::ability::RestrictionPlayerScope::ParentTargetedPlayer
            ) {
                *affected_players = crate::types::ability::RestrictionPlayerScope::SpecificPlayer(
                    resolved_target_player,
                );
            }
        }
        GameRestriction::DamagePreventionDisabled { .. } => {}
    }

    match restriction {
        GameRestriction::ProhibitActivity { expiry, .. } => {
            // CR 514.2: "until [the end of] your next turn" restrictions expire
            // relative to the controller's next turn. The restriction-expiry
            // system tracks only `UntilPlayerNextTurn` (begin-of-next-turn), so
            // both readings map to it here — the precise end-of-next-turn timing
            // matters only for continuous effects / play-permissions (handled in
            // the layer prune); no printed restriction uses "end of next turn".
            if let Some(
                crate::types::ability::Duration::UntilNextTurnOf {
                    player: crate::types::ability::PlayerScope::Controller,
                }
                | crate::types::ability::Duration::UntilEndOfNextTurnOf {
                    player: crate::types::ability::PlayerScope::Controller,
                },
            ) = ability.duration.as_ref()
            {
                *expiry = RestrictionExpiry::UntilPlayerNextTurn {
                    player: ability.controller,
                };
            }
        }
        GameRestriction::DamagePreventionDisabled { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        Duration, GameRestriction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
        TargetRef,
    };
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn restriction_add_restriction_pushes_to_state() {
        let mut state = GameState::new_two_player(42);
        assert!(state.restrictions.is_empty());

        let ability = ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::DamagePreventionDisabled {
                    source: ObjectId(0), // placeholder
                    expiry: RestrictionExpiry::EndOfTurn,
                    scope: None,
                },
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );

        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert_eq!(state.restrictions.len(), 1);

        // Source should be filled from ability.source_id
        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::DamagePreventionDisabled {
                source: ObjectId(5),
                ..
            }
        ));

        // Should emit EffectResolved event
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::AddRestriction,
                ..
            }
        )));
    }

    #[test]
    fn cast_only_from_zones_uses_controllers_next_turn_for_expiry() {
        let mut state = GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::CastOnlyFromZones {
                        allowed_zones: vec![Zone::Hand],
                    },
                },
            },
            vec![],
            ObjectId(9),
            PlayerId(1),
        )
        .duration(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::ProhibitActivity {
                source: ObjectId(9),
                affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
                expiry: RestrictionExpiry::UntilPlayerNextTurn { player: PlayerId(1) },
                activity: ProhibitedActivity::CastOnlyFromZones { allowed_zones },
            } if allowed_zones == &vec![Zone::Hand]
        ));
    }

    #[test]
    fn targeted_player_scope_is_resolved_on_restrictions() {
        let mut state = GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::TargetedPlayer,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::ActivateAbilities {
                        exemption: crate::types::statics::ActivationExemption::ManaAbilities,
                    },
                },
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(7),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::ProhibitActivity {
                source: ObjectId(7),
                affected_players: RestrictionPlayerScope::SpecificPlayer(PlayerId(1)),
                activity: ProhibitedActivity::ActivateAbilities { .. },
                ..
            }
        ));
    }

    #[test]
    fn parent_targeted_player_scope_is_resolved_from_inherited_target() {
        let mut state = GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::ParentTargetedPlayer,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::ActivateAbilities {
                        exemption: crate::types::statics::ActivationExemption::ManaAbilities,
                    },
                },
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(7),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::ProhibitActivity {
                source: ObjectId(7),
                affected_players: RestrictionPlayerScope::SpecificPlayer(PlayerId(1)),
                activity: ProhibitedActivity::ActivateAbilities { .. },
                ..
            }
        ));
    }
}
