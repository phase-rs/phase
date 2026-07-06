use crate::game::quantity::resolve_quantity;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{ExtraPhase, GameState};
use crate::types::phase::Phase;

/// CR 500.8: Add extra phases to the current turn via a LIFO stack.
/// CR 500.10a: Only adds phases to the affected player's own turn.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, phase, after, followed_by, count_expr, attacker_restriction) =
        match &ability.effect {
            Effect::AdditionalPhase {
                target,
                phase,
                after,
                followed_by,
                count,
                attacker_restriction,
            } => (
                target,
                *phase,
                *after,
                followed_by,
                count,
                attacker_restriction,
            ),
            _ => return Err(EffectError::MissingParam("expected AdditionalPhase".into())),
        };

    // CR 500.8: Resolve the target to a PlayerId.
    let player = match target {
        TargetFilter::Controller | TargetFilter::SelfRef => ability.controller,
        TargetFilter::TriggeringPlayer => state
            .current_trigger_event
            .as_ref()
            .and_then(|event| crate::game::targeting::extract_player_from_event(event, state))
            .unwrap_or(ability.controller),
        _ => {
            if let Some(TargetRef::Player(pid)) = ability.targets.first() {
                *pid
            } else {
                ability.controller
            }
        }
    };

    // CR 500.8 (Full Throttle): "After this main phase, there are N additional
    // combat phases" anchors to whichever main phase the spell resolves in.
    // The parser emits `after: PreCombatMain` as a sentinel for this wording.
    let after = if after == Phase::PreCombatMain
        && matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
    {
        state.phase
    } else {
        after
    };

    // CR 500.10a: "If an effect that says 'you get' an additional step or phase
    // would add a step or phase to a turn other than that player's, no steps
    // or phases are added."
    if player != state.active_player {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AdditionalPhase,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 500.8 + CR 510.2: Resolve the count against the triggering combat
    // damage event so Obeka, Splitter of Seconds (and any future "for that
    // many additional <step>" wording) pushes N copies of the extra phase
    // bundle instead of one. Fixed quantities preserve legacy single-push.
    let count =
        resolve_quantity(state, count_expr, ability.controller, ability.source_id).max(0) as usize;
    if count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AdditionalPhase,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 115.1 + CR 601.2c + CR 608.2c: "the chosen creatures" (Last Night
    // Together) are this spell's chosen targets — the parser emits
    // `ParentTarget`, which `resolve_ability_chain` has already propagated down
    // to this sub-ability (`ability.targets == [obj1, obj2]`). CR 608.2h: the
    // affected set is information determined once, at resolution — snapshot the
    // target object IDs into a fixed tracked set so the restriction membership
    // can't drift. `SelfRef` (Throat Wolf) resolves to the source object. All
    // other filters (e.g. `Typed(land creature)` for Bumi) ride through
    // unchanged and are re-evaluated continuously at each declaration
    // (CR 611.2c, rules-modifying continuous effect).
    let resolved_restriction: Option<TargetFilter> = match attacker_restriction {
        // CR 608.2c + CR 608.2h: "the chosen creatures" (`ParentTarget`) and the
        // "those creatures" sentinel (`TrackedSet { id: 0 }`, which `parse_target`
        // emits before any runtime set exists) both refer to THIS spell's chosen
        // targets. Snapshot the propagated target object IDs into a fresh fixed
        // tracked set so the restriction membership can't drift.
        Some(TargetFilter::ParentTarget)
        | Some(TargetFilter::TrackedSet {
            id: crate::types::identifiers::TrackedSetId(0),
        }) => {
            let ids: Vec<crate::types::identifiers::ObjectId> = ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect();
            let set_id = crate::game::effects::publish_fresh_tracked_set(state, ids);
            Some(TargetFilter::TrackedSet { id: set_id })
        }
        Some(TargetFilter::SelfRef) => Some(TargetFilter::SpecificObject {
            id: ability.source_id,
        }),
        // CR 608.2h: an already-concrete `TrackedSet`/`SpecificObject` references
        // a set published elsewhere — pass it through unchanged rather than
        // overwriting it with this spell's own targets.
        other => other.clone(),
    };

    // CR 500.8: Push follow-up phases before the primary phase so the
    // `advance_phase` LIFO scan consumes the primary phase first. Repeat
    // the bundle `count` times so each scheduled occurrence still fires
    // its own anchor → primary → follow_up sequence.
    //
    // When `count > 1` inserts multiple combat phases after a main-phase
    // anchor, only the first bundle may anchor to that main phase — the
    // turn never returns there. Chain subsequent combat bundles to
    // `EndCombat` so each extra combat is reachable (Full Throttle).
    // Repeating the same phase/step (Obeka upkeep) keeps the original anchor.
    for i in 0..count {
        let bundle_anchor = if i == 0 || phase == after {
            after
        } else if phase == Phase::BeginCombat {
            Phase::EndCombat
        } else {
            after
        };
        for &follow_up in followed_by.iter().rev() {
            state.extra_phases.push(ExtraPhase {
                anchor: bundle_anchor,
                phase: follow_up,
                attacker_restriction: None,
                attacker_restriction_source: None,
            });
        }
        // CR 508.1c: Only the scheduled combat phase carries the attacker
        // restriction; follow-up main/upkeep phases never restrict attacks.
        // CR 611.2c: Record the scheduling spell's source ObjectId so that
        // `passes_combat_attacker_restriction` can evaluate source-relative
        // filter predicates against the actual source rather than ObjectId(0).
        let restriction = if phase == Phase::BeginCombat {
            resolved_restriction.clone()
        } else {
            None
        };
        state.extra_phases.push(ExtraPhase {
            anchor: bundle_anchor,
            phase,
            attacker_restriction_source: if restriction.is_some() {
                Some(ability.source_id)
            } else {
                None
            },
            attacker_restriction: restriction,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::AdditionalPhase,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, TargetFilter};
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn make_ability(
        target: TargetFilter,
        phase: Phase,
        after: Phase,
        followed_by: Vec<Phase>,
        controller: PlayerId,
    ) -> ResolvedAbility {
        make_ability_with_count(
            target,
            phase,
            after,
            followed_by,
            controller,
            QuantityExpr::Fixed { value: 1 },
        )
    }

    fn make_ability_with_count(
        target: TargetFilter,
        phase: Phase,
        after: Phase,
        followed_by: Vec<Phase>,
        controller: PlayerId,
        count: QuantityExpr,
    ) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::AdditionalPhase {
                target,
                phase,
                after,
                followed_by,
                count,
                attacker_restriction: None,
            },
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(1),
            source_incarnation: None,
            source_card_id: None,
            targets: vec![],
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            effect_context_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            dig_found_nothing_for_parent_target: false,
        }
    }

    /// Test helper: an ordinary (unrestricted) `ExtraPhase`.
    fn ep(anchor: Phase, phase: Phase) -> ExtraPhase {
        ExtraPhase {
            anchor,
            phase,
            attacker_restriction: None,
            attacker_restriction_source: None,
        }
    }

    #[test]
    fn additional_phase_after_this_main_phase_uses_active_main_as_anchor() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_phases.len(), 2);
        assert_eq!(
            state.extra_phases,
            vec![
                ep(Phase::PostCombatMain, Phase::BeginCombat),
                ep(Phase::EndCombat, Phase::BeginCombat),
            ]
        );
    }

    #[test]
    fn additional_phase_pushes_begin_combat() {
        let mut state = GameState {
            active_player: PlayerId(0),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::EndCombat,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 500.8: anchor = EndCombat so consumption happens after the
        // current combat phase ends (not mid-combat).
        assert_eq!(
            state.extra_phases,
            vec![ep(Phase::EndCombat, Phase::BeginCombat)]
        );
    }

    #[test]
    fn additional_phase_with_main_pushes_both() {
        let mut state = GameState {
            active_player: PlayerId(0),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::EndCombat,
            vec![Phase::PostCombatMain],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // LIFO: PostCombatMain pushed first, BeginCombat on top → on the
        // first EndCombat encountered, BeginCombat (the more recent entry)
        // is consumed; the second EndCombat consumes PostCombatMain.
        assert_eq!(
            state.extra_phases,
            vec![
                ep(Phase::EndCombat, Phase::PostCombatMain),
                ep(Phase::EndCombat, Phase::BeginCombat),
            ]
        );
    }

    #[test]
    fn cr_500_8_lifo_ordering() {
        let mut state = GameState {
            active_player: PlayerId(0),
            ..Default::default()
        };
        let mut events = Vec::new();

        // First effect: additional combat
        let ability1 = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::EndCombat,
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability1, &mut events).unwrap();

        // Second effect: another additional combat (most recent → first)
        let ability2 = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::EndCombat,
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability2, &mut events).unwrap();

        let begin_combat_after_end = ep(Phase::EndCombat, Phase::BeginCombat);
        assert_eq!(
            state.extra_phases,
            vec![
                begin_combat_after_end.clone(),
                begin_combat_after_end.clone()
            ]
        );

        // CR 500.8: Pop from end → most recent first
        assert_eq!(
            state.extra_phases.pop(),
            Some(begin_combat_after_end.clone())
        );
        assert_eq!(state.extra_phases.pop(), Some(begin_combat_after_end));
    }

    #[test]
    fn cr_500_10a_opponent_turn_no_phases_added() {
        // Active player is 1, but controller is 0
        let mut state = GameState {
            active_player: PlayerId(1),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::EndCombat,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 500.10a: No phases added on opponent's turn
        assert!(state.extra_phases.is_empty());
    }

    #[test]
    fn additional_upkeep_uses_triggering_player() {
        let mut state = GameState {
            active_player: PlayerId(1),
            current_trigger_event: Some(GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            }),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::TriggeringPlayer,
            Phase::Upkeep,
            Phase::Upkeep,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_phases, vec![ep(Phase::Upkeep, Phase::Upkeep)]);
    }

    /// CR 500.8 + CR 510.2: Obeka, Splitter of Seconds — "you get that many
    /// additional upkeep steps after this phase" must push one ExtraPhase per
    /// point of combat damage, not a single phase.
    #[test]
    fn additional_phase_count_from_event_context_amount_pushes_n_phases() {
        use crate::types::ability::QuantityRef;
        use crate::types::identifiers::ObjectId as Oid;

        let mut state = GameState {
            active_player: PlayerId(0),
            current_trigger_event: Some(GameEvent::DamageDealt {
                source_id: Oid(1),
                target: TargetRef::Player(PlayerId(1)),
                amount: 5,
                is_combat: true,
                excess: 0,
            }),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            Phase::Upkeep,
            Phase::Upkeep,
            vec![],
            PlayerId(0),
            crate::types::ability::QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let expected = ep(Phase::Upkeep, Phase::Upkeep);
        assert_eq!(
            state.extra_phases,
            vec![
                expected.clone(),
                expected.clone(),
                expected.clone(),
                expected.clone(),
                expected
            ],
            "5 combat damage should schedule 5 additional upkeep steps"
        );
    }

    /// CR 500.8 (Full Throttle): count>1 combat bundles after a main-phase
    /// anchor must chain through EndCombat — the turn never returns to the
    /// main phase between inserted combats.
    #[test]
    fn additional_combat_count_chains_after_end_combat() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.extra_phases,
            vec![
                ep(Phase::PreCombatMain, Phase::BeginCombat),
                ep(Phase::EndCombat, Phase::BeginCombat),
            ]
        );
    }

    #[test]
    fn additional_combat_count_advances_through_both_extra_phases() {
        use crate::game::turns::advance_phase;

        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::BeginCombat, "first extra combat");

        while state.phase != Phase::EndCombat {
            advance_phase(&mut state, &mut events);
        }
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::BeginCombat, "second extra combat");

        while state.phase != Phase::EndCombat {
            advance_phase(&mut state, &mut events);
        }
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(state.extra_phases.is_empty());
    }

    /// CR 608.2h + CR 611.2c: Last Night Together — "Only the chosen creatures
    /// can attack during that combat phase." The parser emits `ParentTarget`;
    /// the resolver must snapshot the spell's chosen targets into a fixed
    /// tracked set and stamp it onto the scheduled BeginCombat ExtraPhase.
    #[test]
    fn restricted_combat_concretizes_parent_target_to_tracked_set() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();

        let mut ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
            vec![],
            PlayerId(0),
        );
        // Stamp the restriction + chosen targets exactly as the parser fold and
        // `resolve_ability_chain` propagation would produce them.
        ability.effect = Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::BeginCombat,
            after: Phase::PreCombatMain,
            followed_by: vec![],
            count: QuantityExpr::Fixed { value: 1 },
            attacker_restriction: Some(TargetFilter::ParentTarget),
        };
        ability.targets = vec![
            TargetRef::Object(ObjectId(11)),
            TargetRef::Object(ObjectId(22)),
        ];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_phases.len(), 1);
        let scheduled = &state.extra_phases[0];
        assert_eq!(scheduled.phase, Phase::BeginCombat);
        let set_id = match &scheduled.attacker_restriction {
            Some(TargetFilter::TrackedSet { id }) => *id,
            other => panic!("expected concretized TrackedSet restriction, got {other:?}"),
        };
        let members = state
            .tracked_object_sets
            .get(&set_id)
            .expect("tracked set published at resolution");
        assert_eq!(members, &vec![ObjectId(11), ObjectId(22)]);
    }
}
