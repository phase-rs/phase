use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
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
        // Fill in the source from the resolving ability's source_id
        fill_source(&mut restriction, ability.source_id);
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

/// Fill the source field of a restriction with the actual source object ID.
/// Parser may produce a default ObjectId; at resolution time we know the real source.
fn fill_source(
    restriction: &mut crate::types::ability::GameRestriction,
    source_id: crate::types::identifiers::ObjectId,
) {
    match restriction {
        crate::types::ability::GameRestriction::DamagePreventionDisabled { source, .. } => {
            *source = source_id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{GameRestriction, RestrictionExpiry};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

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
        match &state.restrictions[0] {
            GameRestriction::DamagePreventionDisabled { source, .. } => {
                assert_eq!(*source, ObjectId(5));
            }
        }

        // Should emit EffectResolved event
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::AddRestriction,
                ..
            }
        )));
    }
}
