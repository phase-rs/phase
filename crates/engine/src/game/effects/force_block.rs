use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    ContinuousModification, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, TransientContinuousEffectBindings};
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::statics::StaticMode;

/// CR 509.1c: Force block — the target creature must block if able.
///
/// Note: `MustBlock` (creature must block any attacker), `MustBlockAttacker`
/// (creature must block one specific attacker), and `MustBeBlocked` (creature
/// must be blocked by others) are three distinct requirements (CR 509.1c).
///
/// The requirement applies to every creature the effect's `target` filter
/// resolves to — a single chosen target ("target creature blocks this turn if
/// able") or an entire non-targeted set ("each creature your opponents control
/// blocks this turn if able", Predatory Rampage). `resolved_object_ids_for_filter`
/// returns the explicit chosen target(s) when present and otherwise expands the
/// filter across the battlefield, mirroring `force_attack` (CR 508.1d).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, named_attacker, has_named_attacker, duration) = match &ability.effect {
        Effect::ForceBlock {
            target,
            attacker,
            duration,
            ..
        } => (
            target,
            ability.force_block_attacker.or_else(|| match attacker {
                Some(crate::types::ability::ForceBlockAttackerRef::Source) => ability
                    .source_incarnation
                    .map(|incarnation| ObjectIncarnationRef {
                        object_id: ability.source_id,
                        incarnation,
                    }),
                _ => None,
            }),
            attacker.is_some(),
            duration,
        ),
        _ => return Ok(()),
    };

    let mode = match named_attacker {
        // CR 611.2a + CR 400.7: record the requirement now, for the effect's
        // stated duration, against the exact incarnation the ability named. A
        // departed/re-entered object became a new object and cannot be
        // rediscovered from its raw id, so `is_current` is the whole
        // resolution-time question.
        //
        // CR 509.1c: whether that attacker is attacking is a DECLARE-BLOCKERS
        // question, asked again at each declare blockers step in the turn —
        // deliberately not asked here. `combat::BlockDeclarationConstraints::build`
        // re-checks `combat.attackers` membership, the defending player, and this
        // same incarnation pin before emitting a `BlockDeclarationRequirement::Exact`,
        // so a requirement recorded outside combat is inert until it applies.
        // Mirrors `force_attack::resolve` (CR 508.1d), which likewise never reads
        // `state.combat`.
        Some(attacker) if attacker.is_current(state) => StaticMode::MustBlockAttacker { attacker },
        // CR 400.7: a stale or absent referent names no object, so there is
        // nothing to require a block against. It must NOT degrade into a generic
        // `StaticMode::MustBlock` — that would force the target to block any
        // attacker, the bug issue #1836 closed.
        //
        // The ability still resolved, so the game log must say so. Same shape as
        // `remove_from_combat::resolve`'s stale-referent arm; distinct from the
        // `Effect` variant-mismatch guard at the top of this function, which stays
        // silent because no ForceBlock ever reached it.
        Some(_) => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ForceBlock,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
        // The effect names an attacker but no referent resolved at all — an
        // unbound `EventSource`, or a missing `source_incarnation`. Nothing to
        // record, and the same #1836 reason as above forbids falling through to
        // generic `MustBlock`.
        None if has_named_attacker => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ForceBlock,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
        None => StaticMode::MustBlock,
    };

    for obj_id in resolved_object_ids_for_filter(state, ability, target_filter) {
        // CR 509.1c: Requirements that creatures must block are checked during
        // the declare blockers step.
        if !state.objects.contains_key(&obj_id) {
            continue;
        }

        let recipient = ObjectIncarnationRef::from_object(&state.objects[&obj_id]);
        state.add_transient_continuous_effect_with_bindings(
            ability.source_id,
            ability.controller,
            duration.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::AddStaticMode { mode: mode.clone() }],
            None,
            TransientContinuousEffectBindings {
                affected_recipient: Some(recipient),
                duration_subject: None,
            },
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ForceBlock,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, Effect, ForceBlockAttackerRef, TargetRef, TypedFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_force_block_ability(source: ObjectId, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ForceBlock {
                target: TargetFilter::Any,
                attacker: None,
                duration: crate::types::ability::Duration::UntilEndOfTurn,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        )
    }

    #[test]
    fn force_block_without_active_source_attacker_grants_generic_must_block() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spell Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_force_block_ability(source, target);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.iter().any(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlock,
                        }
                    )
                })
            }),
            "generic force block should grant attacker-agnostic MustBlock"
        );

        // Verify EffectResolved emitted
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::ForceBlock,
                ..
            }
        )));
    }

    #[test]
    fn force_block_active_source_attacker_grants_must_block_attacker() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Provocateur".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(source, PlayerId(1))],
            ..Default::default()
        });

        let mut ability = make_force_block_ability(source, target);
        ability.effect = Effect::ForceBlock {
            target: TargetFilter::Any,
            attacker: Some(ForceBlockAttackerRef::Source),
            duration: crate::types::ability::Duration::UntilEndOfTurn,
        };
        ability.force_block_attacker =
            Some(ObjectIncarnationRef::from_object(&state.objects[&source]));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.iter().any(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlockAttacker { attacker },
                        } if attacker.object_id == source
                    )
                })
            }),
            "source-referential force block should bind to the active attacker"
        );
    }

    #[test]
    fn force_block_named_attacker_not_yet_attacking_records_the_requirement() {
        // CR 611.2a + CR 400.7: a named attacker that is alive and current but
        // not yet declared as an attacker still gets its requirement recorded
        // at resolution. CR 509.1c's "is it attacking" question is asked later,
        // at declare blockers, by `combat::BlockDeclarationConstraints::build`
        // — not here, and `state.combat` is `None` in this fixture to prove it.
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tangle Angler".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut ability = make_force_block_ability(source, target);
        ability.effect = Effect::ForceBlock {
            target: TargetFilter::Any,
            attacker: Some(ForceBlockAttackerRef::Source),
            duration: crate::types::ability::Duration::UntilEndOfTurn,
        };
        let pin = ObjectIncarnationRef::from_object(&state.objects[&source]);
        ability.force_block_attacker = Some(pin);
        assert!(
            pin.is_current(&state),
            "reach guard: the named referent must be live and current to reach arm 1's alive branch"
        );
        assert!(state.combat.is_none(), "reach guard: no combat exists yet");

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.iter().any(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlockAttacker { attacker },
                        } if attacker.object_id == source
                    )
                })
            }),
            "a live, current named attacker must record MustBlockAttacker even with no active combat"
        );
    }

    #[test]
    fn force_block_stale_named_attacker_records_nothing_but_logs() {
        // CR 400.7: a stale referent (same ObjectId, bumped incarnation) names
        // no live object, so nothing is recorded. Must not degrade to generic
        // MustBlock — the bug issue #1836 closed.
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tangle Angler".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut ability = make_force_block_ability(source, target);
        ability.effect = Effect::ForceBlock {
            target: TargetFilter::Any,
            attacker: Some(ForceBlockAttackerRef::Source),
            duration: crate::types::ability::Duration::UntilEndOfTurn,
        };
        let pin = ObjectIncarnationRef::from_object(&state.objects[&source]);
        ability.force_block_attacker = Some(pin);
        state.objects.get_mut(&source).unwrap().incarnation += 1;
        assert!(
            !pin.is_current(&state),
            "reach guard: the pinned referent must be stale relative to the bumped incarnation"
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.is_empty(),
            "a stale named attacker must record no requirement at all"
        );
        assert!(
            !state.transient_continuous_effects.iter().any(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlock,
                        }
                    )
                })
            }),
            "issue #1836: a stale named attacker must NOT degrade to generic MustBlock"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ForceBlock,
                    ..
                }
            )),
            "the ability still resolved and must log EffectResolved even though it affected nothing"
        );
    }

    #[test]
    fn force_block_unresolved_named_attacker_records_nothing_but_logs() {
        // The effect names an attacker (EventSource) but nothing ever bound
        // `force_block_attacker` — an unresolved event-source referent. CR
        // 400.7: nothing to record, and the #1836 reason forbids falling
        // through to generic MustBlock.
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Tolsimir, Midnight's Light".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::ForceBlock {
                target: TargetFilter::Any,
                attacker: Some(ForceBlockAttackerRef::EventSource),
                duration: crate::types::ability::Duration::UntilEndOfTurn,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        assert!(
            ability.force_block_attacker.is_none(),
            "reach guard: no binding was ever attached to this ability"
        );
        assert!(
            matches!(
                &ability.effect,
                Effect::ForceBlock {
                    attacker: Some(_),
                    ..
                }
            ),
            "reach guard: the effect itself names an attacker"
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.is_empty(),
            "an unresolved named attacker must record no requirement at all"
        );
        assert!(
            !state.transient_continuous_effects.iter().any(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlock,
                        }
                    )
                })
            }),
            "issue #1836: an unresolved named attacker must NOT degrade to generic MustBlock"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ForceBlock,
                    ..
                }
            )),
            "the ability still resolved and must log EffectResolved even though it affected nothing"
        );
    }

    #[test]
    fn tolsimir_attack_trigger_binds_the_wolf_not_tolsimir() {
        // Regression: the old resolver inferred the named attacker from the
        // triggered ability's source. Tolsimir is not the attacker named by
        // "that Wolf"; the event Wolf is. Reverting the pending-ability
        // provenance binding makes this assertion select Tolsimir or generic
        // MustBlock instead.
        let mut state = GameState::new_two_player(42);
        let tolsimir = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Tolsimir, Midnight's Light".to_string(),
            Zone::Battlefield,
        );
        let wolf = create_object(
            &mut state,
            CardId(418),
            PlayerId(0),
            "Voja Fenstalker".to_string(),
            Zone::Battlefield,
        );
        let blocker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Bear".to_string(),
            Zone::Battlefield,
        );
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(wolf, PlayerId(1))],
            ..Default::default()
        });

        let mut ability = ResolvedAbility::new(
            Effect::ForceBlock {
                target: TargetFilter::Any,
                attacker: Some(ForceBlockAttackerRef::EventSource),
                duration: crate::types::ability::Duration::UntilEndOfCombat,
            },
            vec![TargetRef::Object(blocker)],
            tolsimir,
            PlayerId(0),
        );
        ability.bind_force_block_attacker_recursive(Some(ObjectIncarnationRef::from_object(
            &state.objects[&wolf],
        )));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let effect = state
            .transient_continuous_effects
            .iter()
            .find(|effect| effect.affected_recipient.is_some())
            .cloned()
            .expect("targeted force block installs an exact-recipient TCE");
        assert_eq!(
            effect.affected_recipient,
            Some(ObjectIncarnationRef::from_object(&state.objects[&blocker]))
        );
        assert!(effect.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustBlockAttacker { attacker },
                } if attacker.object_id == wolf
            )
        }));

        state.objects.get_mut(&blocker).unwrap().incarnation += 1;
        assert!(
            !crate::game::layers::transient_effect_is_live(&state, &effect),
            "the targeted force-block TCE must not apply to a later blocker incarnation"
        );
    }

    #[test]
    fn force_block_multiple_targets() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target1 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Bear1".to_string(),
            Zone::Battlefield,
        );
        let target2 = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Bear2".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::ForceBlock {
                target: TargetFilter::Any,
                attacker: None,
                duration: crate::types::ability::Duration::UntilEndOfTurn,
            },
            vec![TargetRef::Object(target1), TargetRef::Object(target2)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let must_block_count = state
            .transient_continuous_effects
            .iter()
            .filter(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlock,
                        }
                    )
                })
            })
            .count();
        assert_eq!(must_block_count, 2, "Should create one effect per target");
    }

    /// CR 509.1c (issue #4233): a non-targeted mass force-block — Predatory
    /// Rampage's "Each creature your opponents control blocks this turn if able"
    /// — carries no chosen targets; the requirement must be applied to every
    /// creature its `target` filter resolves to, not silently to no one (the
    /// resolver previously only walked the empty `ability.targets`).
    #[test]
    fn force_block_mass_filter_applies_to_all_matching_creatures() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Predatory Rampage".to_string(),
            Zone::Battlefield,
        );
        let opp_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear A".to_string(),
            Zone::Battlefield,
        );
        let opp_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Bear B".to_string(),
            Zone::Battlefield,
        );
        let own = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "My Bear".to_string(),
            Zone::Battlefield,
        );
        for id in [opp_a, opp_b, own] {
            state.objects.get_mut(&id).unwrap().card_types.core_types =
                vec![crate::types::card_type::CoreType::Creature];
        }

        // Non-targeted: filter = "creatures your opponents control", no targets.
        let ability = ResolvedAbility::new(
            Effect::ForceBlock {
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::Opponent),
                ),
                attacker: None,
                duration: crate::types::ability::Duration::UntilEndOfTurn,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let forced: std::collections::HashSet<_> = state
            .transient_continuous_effects
            .iter()
            .filter(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlock,
                        }
                    )
                })
            })
            .filter_map(|ce| match ce.affected {
                TargetFilter::SpecificObject { id } => Some(id),
                _ => None,
            })
            .collect();

        assert!(
            forced.contains(&opp_a) && forced.contains(&opp_b),
            "both opponents' creatures must be forced to block, got {forced:?}"
        );
        assert!(
            !forced.contains(&own),
            "the caster's own creature must not be forced to block"
        );
    }
}
