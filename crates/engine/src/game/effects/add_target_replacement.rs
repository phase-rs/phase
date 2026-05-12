use crate::game::targeting::resolve_event_context_target;
use crate::types::ability::{
    DamageTargetFilter, DamageTargetPlayerScope, Duration, Effect, EffectError, EffectKind,
    ReplacementDefinition, ResolvedAbility, RestrictionExpiry, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

fn expiry_from_duration(
    duration: Option<&Duration>,
    controller: crate::types::player::PlayerId,
) -> Option<RestrictionExpiry> {
    match duration {
        Some(Duration::UntilEndOfTurn) => Some(RestrictionExpiry::EndOfTurn),
        Some(Duration::UntilEndOfCombat) => Some(RestrictionExpiry::EndOfCombat),
        Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        }) => Some(RestrictionExpiry::UntilPlayerNextTurn { player: controller }),
        _ => None,
    }
}

fn replacement_with_ability_expiry(
    replacement: &ReplacementDefinition,
    ability: &ResolvedAbility,
) -> ReplacementDefinition {
    let mut replacement = replacement.clone();
    if replacement.expiry.is_none() {
        replacement.expiry = expiry_from_duration(ability.duration.as_ref(), ability.controller);
    }
    replacement
}

fn replacement_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<TargetRef> {
    if matches!(target, TargetFilter::Any) {
        return ability.targets.clone();
    }

    resolve_event_context_target(state, target, ability.source_id)
        .into_iter()
        .collect()
}

/// CR 614.1a + CR 514.2: Push a replacement effect onto the parent
/// ability's target object or player at resolution time. Used by riders like
/// "If that creature would die this turn, exile it instead." attached to
/// damage-dealing spells/abilities. The carried `ReplacementDefinition`
/// is appended to each targeted object's `replacement_definitions`, or to
/// GameState pending damage replacements for player-scoped damage effects.
///
/// Multiple targets each receive their own copy of the replacement —
/// `valid_card: SelfRef` inside the carried definition naturally binds
/// to the carrying object, so each instance fires only for its host.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::AddTargetReplacement {
        replacement,
        target,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "AddTargetReplacement replacement".to_string(),
        ));
    };

    let mut attached = 0usize;

    // CR 614.1a: `TargetFilter::None` is the "no per-target binding" signal —
    // the carried replacement is self-contained (its own source/target filters
    // already constrain when it fires) and is pushed directly to the global
    // pending_damage_replacements. Used by triggered creation of turn-bound
    // damage-modification replacements (Rankle and Torbran's "If a source
    // would deal damage to a player or battle this turn..."; I Call for
    // Slaughter's "If a source you control would deal damage this turn,
    // it deals that much damage plus 1 instead.").
    if matches!(target, TargetFilter::None) {
        let replacement = replacement_with_ability_expiry(replacement, ability);
        state.pending_damage_replacements.push(replacement);
        attached += 1;
    } else {
        for resolved_target in replacement_targets(state, ability, target) {
            match resolved_target {
                TargetRef::Object(obj_id) => {
                    let replacement = replacement_with_ability_expiry(replacement, ability);
                    if let Some(obj) = state.objects.get_mut(&obj_id) {
                        obj.replacement_definitions.push(replacement);
                        attached += 1;
                    }
                }
                TargetRef::Player(player) => {
                    let mut replacement = replacement_with_ability_expiry(replacement, ability);
                    if matches!(
                        replacement.event,
                        crate::types::replacements::ReplacementEvent::DamageDone
                    ) && replacement.damage_target_filter.is_none()
                    {
                        replacement.damage_target_filter =
                            Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                                player: DamageTargetPlayerScope::Specific(player),
                            });
                    }
                    state.pending_damage_replacements.push(replacement);
                    attached += 1;
                }
            }
        }
    }

    if attached > 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AddTargetReplacement,
            source_id: ability.source_id,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::replacement::{replace_event, ReplacementResult};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        DamageModification, DamageTargetPlayerScope, Duration, ReplacementDefinition,
        RestrictionExpiry, TargetFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::ProposedEvent;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn damage_to(target: TargetRef, amount: u32) -> ProposedEvent {
        ProposedEvent::Damage {
            source_id: ObjectId(99),
            target,
            amount,
            is_combat: false,
            applied: Default::default(),
        }
    }

    #[test]
    fn pushes_eot_replacement_onto_target_object() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Graveyard);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(id)],
            ObjectId(0),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.replacement_definitions.iter_all().count(), 1);
        assert_eq!(
            obj.replacement_definitions[0].expiry,
            Some(RestrictionExpiry::EndOfTurn)
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::AddTargetReplacement,
                ..
            }
        )));
    }

    #[test]
    fn pushes_damage_replacement_for_triggering_player() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        });

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Double);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::TriggeringPlayer,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        let pending = &state.pending_damage_replacements[0];
        assert_eq!(
            pending.damage_target_filter,
            Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                player: DamageTargetPlayerScope::Specific(PlayerId(1))
            })
        );
        assert_eq!(
            pending.expiry,
            Some(RestrictionExpiry::UntilPlayerNextTurn {
                player: PlayerId(0)
            })
        );

        let proposed = damage_to(TargetRef::Player(PlayerId(1)), 2);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified damage event, got {result:?}");
        };
        assert_eq!(amount, 4);

        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Permanent".to_string(),
            Zone::Battlefield,
        );
        let proposed = damage_to(TargetRef::Object(permanent), 3);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified permanent damage event, got {result:?}");
        };
        assert_eq!(amount, 6);
    }

    #[test]
    fn pending_damage_replacement_expires_on_controllers_next_turn() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        });

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Double);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::TriggeringPlayer,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.pending_damage_replacements.len(), 1);

        crate::game::turns::execute_untap(&mut state, &mut events);
        assert!(state.pending_damage_replacements.is_empty());

        let proposed = damage_to(TargetRef::Player(PlayerId(1)), 2);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected unmodified damage event, got {result:?}");
        };
        assert_eq!(amount, 2);
    }

    #[test]
    fn target_filter_none_pushes_global_replacement_without_inference() {
        // CR 614.1a: `TargetFilter::None` is the no-binding mode used by
        // self-contained turn-bound damage-modification replacements
        // (Rankle and Torbran, I Call for Slaughter). The resolver must
        // push the carried replacement directly to
        // `pending_damage_replacements` WITHOUT inferring a
        // `damage_target_filter` from a player target — the carried
        // replacement's own source/target/scope filters are the source
        // of truth.
        let mut state = GameState::new_two_player(42);
        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Plus { value: 1 })
            .damage_source_filter(TargetFilter::Typed(
                crate::types::ability::TypedFilter::default()
                    .controller(crate::types::ability::ControllerRef::You),
            ));
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        let pending = &state.pending_damage_replacements[0];
        // Critical: damage_target_filter must remain None — no per-target
        // inference (which would scope to a specific player).
        assert_eq!(pending.damage_target_filter, None);
        assert_eq!(pending.expiry, Some(RestrictionExpiry::EndOfTurn));
    }
}
