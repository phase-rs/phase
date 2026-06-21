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
        fill_runtime_fields(state, &mut restriction, ability);
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
fn fill_runtime_fields(
    state: &GameState,
    restriction: &mut GameRestriction,
    ability: &ResolvedAbility,
) {
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
            use crate::types::ability::RestrictionPlayerScope;
            match affected_players {
                RestrictionPlayerScope::TargetedPlayer
                | RestrictionPlayerScope::ParentTargetedPlayer => {
                    *affected_players =
                        RestrictionPlayerScope::SpecificPlayer(resolved_target_player);
                }
                // CR 508.5 / CR 508.5a: capture the defending player as the
                // restriction is created — they are fixed once attackers are
                // declared (Xantid Swarm's "defending player can't cast spells").
                // If the source has left combat before the trigger resolves,
                // read the trigger event per CR 508.5.
                RestrictionPlayerScope::DefendingPlayer => {
                    if let Some(defender) =
                        crate::game::combat::defending_player_for_attacker(state, ability.source_id)
                            .or_else(|| {
                                super::myriad::defending_player_from_attack_event(
                                    state.current_trigger_event.as_ref(),
                                    ability.source_id,
                                )
                            })
                    {
                        *affected_players = RestrictionPlayerScope::SpecificPlayer(defender);
                    }
                }
                RestrictionPlayerScope::AllPlayers
                | RestrictionPlayerScope::SpecificPlayer(_)
                | RestrictionPlayerScope::OpponentsOfSourceController => {}
            }
        }
        GameRestriction::DamagePreventionDisabled { .. } => {}
    }

    match restriction {
        GameRestriction::ProhibitActivity { expiry, .. } => {
            use crate::types::ability::{Duration, PlayerScope};
            match ability.duration.as_ref() {
                // CR 514.2 + CR 611.2a: "until your next turn" expires at the
                // *beginning* of the controller's next turn.
                Some(Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                }) => {
                    *expiry = RestrictionExpiry::UntilPlayerNextTurn {
                        player: ability.controller,
                    };
                }
                // CR 514.2 + CR 500.7: "during [the controller's] next turn …"
                // (Kang) persists through that entire turn and expires at its
                // cleanup. Lower to the pre-armed `UntilEndOfNextTurnOf` marker,
                // which the untap step converts to `EndOfTurn` (mirroring
                // `prune_until_next_turn_effects`) so the existing cleanup prune
                // ends it at THAT turn's cleanup.
                Some(Duration::UntilEndOfNextTurnOf {
                    player: PlayerScope::Controller,
                }) => {
                    *expiry = RestrictionExpiry::UntilEndOfNextTurnOf {
                        player: ability.controller,
                    };
                }
                _ => {}
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
                        only_tag: None,
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
                        only_tag: None,
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
