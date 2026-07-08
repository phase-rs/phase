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
        for mut restriction in expand_per_opponent_next_turn(state, restriction.clone(), ability) {
            fill_runtime_fields(state, &mut restriction, ability);
            state.restrictions.push(restriction);
        }
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

/// CR 500.7 + CR 514.2 + CR 109.5: Fan out an "each opponent can't … during that
/// player's next turn" prohibition (Sphinx's Decree, Azor) into one
/// `SpecificPlayer` restriction per opponent, each anchored on that opponent's
/// OWN next turn.
///
/// A single `OpponentsOfSourceController` restriction carrying the pre-armed
/// `UntilEndOfNextTurnOf` marker cannot express this: `fill_runtime_fields` has
/// no single restricted player to anchor on, so it falls back to the controller,
/// and a controller-anchored marker stays dormant through every opponent's turn
/// (`casting.rs` skips a still-pre-armed `UntilEndOfNextTurnOf`) and only arms on
/// the controller's own next turn — when no opponent has a turn — so the ban
/// never takes force. Splitting per opponent lets each marker arm on its own
/// player's untap step (`turns.rs`).
///
/// Only the `OpponentsOfSourceController` + next-turn combination is fanned out;
/// every other shape (including Kang's `AllPlayers` self-anchored power-up ban,
/// which correctly anchors on the controller's own extra turn) passes through as
/// a single-element vec.
fn expand_per_opponent_next_turn(
    state: &GameState,
    restriction: GameRestriction,
    ability: &ResolvedAbility,
) -> Vec<GameRestriction> {
    use crate::types::ability::{Duration, PlayerScope, RestrictionPlayerScope};

    let is_next_turn = matches!(
        ability.duration,
        Some(Duration::UntilEndOfNextTurnOf {
            player: PlayerScope::Controller,
        })
    );
    if is_next_turn {
        if let GameRestriction::ProhibitActivity {
            affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
            ..
        } = &restriction
        {
            let opponents = crate::game::players::opponents(state, ability.controller);
            if !opponents.is_empty() {
                return opponents
                    .into_iter()
                    .map(|opponent| {
                        let mut per_opponent = restriction.clone();
                        if let GameRestriction::ProhibitActivity {
                            affected_players, ..
                        } = &mut per_opponent
                        {
                            *affected_players = RestrictionPlayerScope::SpecificPlayer(opponent);
                        }
                        per_opponent
                    })
                    .collect();
            }
        }
    }
    vec![restriction]
}

/// Fill runtime-bound fields of a restriction using the resolving ability context.
fn fill_runtime_fields(
    state: &GameState,
    restriction: &mut GameRestriction,
    ability: &ResolvedAbility,
) {
    match restriction {
        GameRestriction::DamagePreventionDisabled { source, .. }
        | GameRestriction::ProhibitActivity { source, .. }
        | GameRestriction::CantEnterBattlefieldFrom { source, .. } => {
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
                        crate::game::combat::resolve_defending_player(state, ability.source_id)
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
                // CR 109.5 + CR 118.12: lower the per-iteration scoped player
                // ("each opponent who does …" — The Second Doctor, City Hall) to
                // a concrete id so the "during their next turn" expiry block below
                // reads it as a `SpecificPlayer` and anchors on the restricted
                // opponent, not the controller.
                RestrictionPlayerScope::ScopedPlayer => {
                    *affected_players = RestrictionPlayerScope::SpecificPlayer(
                        ability.scoped_player.unwrap_or(ability.controller),
                    );
                }
                RestrictionPlayerScope::AllPlayers
                | RestrictionPlayerScope::SpecificPlayer(_)
                | RestrictionPlayerScope::OpponentsOfSourceController => {}
            }
        }
        // CantEnterBattlefieldFrom has no acting-player scope (it prohibits an
        // object zone transition, CR 614.1d), so there is nothing to lower here.
        GameRestriction::DamagePreventionDisabled { .. }
        | GameRestriction::CantEnterBattlefieldFrom { .. } => {}
    }

    match restriction {
        GameRestriction::ProhibitActivity {
            expiry,
            affected_players,
            ..
        } => {
            use crate::types::ability::{Duration, PlayerScope, RestrictionPlayerScope};
            // CR 109.5 + CR 514.2: when the restriction targets a specific player
            // ("that player can't attack …"), a "during their next turn" duration
            // must expire at the RESTRICTED player's next turn — not the grant
            // controller's. The affected-player resolution above already lowered a
            // `TargetedPlayer`/`ParentTargetedPlayer` scope to `SpecificPlayer(p)`,
            // so read that resolved player here (Willie Lumpkin).
            let restricted_player = match affected_players {
                RestrictionPlayerScope::SpecificPlayer(p) => Some(*p),
                _ => None,
            };
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
                        // CR 109.5 + CR 514.2: a player-targeted prohibition
                        // ("during their next turn") anchors on the restricted
                        // player; fall back to the controller for grants with no
                        // resolved specific player (Kang's self-controller form).
                        player: restricted_player.unwrap_or(ability.controller),
                    };
                }
                _ => {}
            }
        }
        // CR 611.2a: the parser hardcodes `EndOfTurn` ("this turn") for
        // CantEnterBattlefieldFrom, so there is no duration to lower — same as
        // DamagePreventionDisabled.
        GameRestriction::DamagePreventionDisabled { .. }
        | GameRestriction::CantEnterBattlefieldFrom { .. } => {}
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

    /// CR 109.5 + CR 514.2 + CR 500.7: The Second Doctor / City Hall — the
    /// per-iteration `ScopedPlayer` restriction resolves to the scoped opponent,
    /// and a "during their next turn" duration anchors its expiry on THAT
    /// opponent (the restricted player), not on the ability controller.
    #[test]
    fn scoped_player_scope_resolves_and_anchors_next_turn_on_scoped_player() {
        use crate::types::triggers::AttackTargetFilter;
        let mut state = GameState::new_two_player(42);

        let mut ability = ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::ScopedPlayer,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::Attack {
                        defended: AttackTargetFilter::PlayerOrPermanents,
                    },
                },
            },
            vec![],
            ObjectId(7),
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });
        // The per-player iteration bound the scoped player to the opponent.
        ability.scoped_player = Some(PlayerId(1));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::ProhibitActivity {
                source: ObjectId(7),
                affected_players: RestrictionPlayerScope::SpecificPlayer(PlayerId(1)),
                // CR 109.5: the "during their next turn" expiry anchors on the
                // restricted (scoped) opponent, not the controller (PlayerId(0)).
                expiry: RestrictionExpiry::UntilEndOfNextTurnOf {
                    player: PlayerId(1)
                },
                activity: ProhibitedActivity::Attack {
                    defended: AttackTargetFilter::PlayerOrPermanents,
                },
            }
        ));
    }

    /// CR 109.5: a `ScopedPlayer` restriction with no bound `scoped_player`
    /// falls back to the controller — the same defensive fallback the parser
    /// lowering relies on (`scoped_player.unwrap_or(controller)`).
    #[test]
    fn scoped_player_scope_falls_back_to_controller_when_unbound() {
        use crate::types::triggers::AttackTargetFilter;
        let mut state = GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::ScopedPlayer,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::Attack {
                        defended: AttackTargetFilter::Player,
                    },
                },
            },
            vec![],
            ObjectId(7),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            &state.restrictions[0],
            GameRestriction::ProhibitActivity {
                affected_players: RestrictionPlayerScope::SpecificPlayer(PlayerId(0)),
                ..
            }
        ));
    }
}
