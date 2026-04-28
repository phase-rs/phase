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
        | GameRestriction::CastOnlyFromZones { source, .. }
        | GameRestriction::CantCastSpells { source, .. } => {
            *source = ability.source_id;
        }
    }

    match restriction {
        GameRestriction::CastOnlyFromZones { expiry, .. }
        | GameRestriction::CantCastSpells { expiry, .. } => {
            if let Some(crate::types::ability::Duration::UntilNextTurnOf {
                player: crate::types::ability::PlayerScope::Controller,
            }) = ability.duration.as_ref()
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
        Duration, GameRestriction, RestrictionExpiry, RestrictionPlayerScope,
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
                restriction: GameRestriction::CastOnlyFromZones {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
                    allowed_zones: vec![Zone::Hand],
                    expiry: RestrictionExpiry::EndOfTurn,
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
            GameRestriction::CastOnlyFromZones {
                source: ObjectId(9),
                affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
                allowed_zones,
                expiry: RestrictionExpiry::UntilPlayerNextTurn { player: PlayerId(1) },
            } if allowed_zones == &vec![Zone::Hand]
        ));
    }
}
