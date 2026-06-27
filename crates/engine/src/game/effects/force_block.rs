use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::statics::StaticMode;

/// CR 509.1c: Force block — the target creature must block this turn if able.
///
/// If the effect source is currently an attacker, this is the Provoke/source-
/// referential shape (CR 702.39a: "block this creature if able"), so grant
/// `MustBlockAttacker { attacker: source }`. Otherwise preserve the generic
/// attacker-agnostic `MustBlock` shape for "blocks this turn if able".
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
    let target_filter = match &ability.effect {
        Effect::ForceBlock { target } => target,
        _ => return Ok(()),
    };

    let source_is_active_attacker = state.combat.as_ref().is_some_and(|combat| {
        combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == ability.source_id)
    });
    let mode = if source_is_active_attacker {
        // CR 702.39a + CR 509.1c: Provoke/source-referential force-block
        // effects require blocking this specific attacking source.
        StaticMode::MustBlockAttacker {
            attacker: ability.source_id,
        }
    } else {
        // CR 509.1c: Generic "blocks this turn if able" effects only require
        // blocking some legal attacker.
        StaticMode::MustBlock
    };

    for obj_id in resolved_object_ids_for_filter(state, ability, target_filter) {
        // CR 509.1c: Requirements that creatures must block are checked during
        // the declare blockers step.
        if !state.objects.contains_key(&obj_id) {
            continue;
        }

        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::AddStaticMode { mode: mode.clone() }],
            None,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ForceBlock,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Effect, TargetRef, TypedFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_force_block_ability(source: ObjectId, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ForceBlock {
                target: TargetFilter::Any,
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

        let ability = make_force_block_ability(source, target);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.iter().any(|ce| {
                ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustBlockAttacker { attacker },
                        } if *attacker == source
                    )
                })
            }),
            "source-referential force block should bind to the active attacker"
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
