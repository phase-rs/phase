use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    ContinuousModification, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
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
        // CR 400.7 + CR 509.1c: Only a still-live exact incarnation that is
        // currently attacking is a legal named attacker. A departed/re-entered
        // object cannot be rediscovered from its raw id.
        Some(attacker)
            if state.combat.as_ref().is_some_and(|combat| {
                combat.attackers.iter().any(|info| {
                    info.object_id == attacker.object_id
                        && state
                            .objects
                            .get(&attacker.object_id)
                            .is_some_and(|object| {
                                ObjectIncarnationRef::from_object(object) == attacker
                            })
                })
            }) =>
        {
            StaticMode::MustBlockAttacker { attacker }
        }
        // An unavailable named referent cannot become a generic requirement.
        // The instruction has no attacker it can require a block against.
        None if has_named_attacker => return Ok(()),
        None => StaticMode::MustBlock,
    };

    for obj_id in resolved_object_ids_for_filter(state, ability, target_filter) {
        // CR 509.1c: Requirements that creatures must block are checked during
        // the declare blockers step.
        if !state.objects.contains_key(&obj_id) {
            continue;
        }

        let recipient = ObjectIncarnationRef::from_object(&state.objects[&obj_id]);
        let effect_id = state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            duration.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::AddStaticMode { mode: mode.clone() }],
            None,
        );
        state.set_transient_affected_recipient(effect_id, recipient);
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
