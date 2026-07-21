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

    // CR 501.1 + CR 500.8: "there is an additional beginning phase after this
    // phase." The beginning phase (untap/upkeep/draw, CR 501.1) is inserted after
    // the phase this ability resolves in and the turn then resumes at that
    // phase's natural successor. Marker: `phase == Untap` (no other AdditionalPhase
    // producer emits `phase: Untap`).
    //
    // CR 500.10a's "you get" controller-restriction is limited to the "you get"
    // wording (grep-verified) and does NOT apply to the "there is an additional …
    // phase" wording, so this branch is placed BEFORE the CR 500.10a gate below.
    // The phase is added to the turn in progress — the active player's turn —
    // regardless of who controls the source. This is required by Shadow of the
    // Second Sun (an aura on another player: the enchanted/active player gets the
    // phase) and correct for Temple/Sphinx/Cyclonus (controller == active player).
    if phase == Phase::Untap {
        // Count defaults to Fixed(1); reuse the shared quantity resolver so any
        // future "N additional beginning phases" wording is covered for free.
        let count = resolve_quantity(state, count_expr, ability.controller, ability.source_id)
            .max(0) as usize;
        let anchor = crate::game::turns::last_step_of_phase(state.phase);
        for _ in 0..count {
            state.extra_phases.push(ExtraPhase {
                anchor,
                phase: Phase::Untap,
                attacker_restriction: None,
                attacker_restriction_source: None,
            });
        }
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AdditionalPhase,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

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
            subject: None,
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
            subject: None,
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
        subject: None,
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
            trigger_source: None,
            trigger_definition_ref: None,
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
            selected_mode_labels: Vec::new(),
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
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
            parent_target_missing_reason: None,
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

    /// CR 501.1 + CR 500.8: an inserted beginning phase runs untap → upkeep →
    /// draw, then the turn resumes at the anchor's natural successor. The anchor
    /// phase (PostCombatMain) is never re-entered, so its beginning-of-phase
    /// trigger does not re-fire, and `extra_phase_resume` empties.
    #[test]
    fn additional_beginning_phase_runs_then_resumes_after_anchor() {
        use crate::game::turns::advance_phase;

        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Untap,
            Phase::PostCombatMain,
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.extra_phases.len(), 1);

        // Leaving PostCombatMain enters the inserted beginning phase.
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::Untap, "inserted beginning phase starts");
        assert_eq!(
            state.extra_phase_resume,
            vec![Phase::PostCombatMain],
            "resume anchor recorded"
        );

        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::Upkeep);
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::Draw);

        // Leaving the inserted draw step resumes after PostCombatMain → End.
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::End, "resumes after the anchor phase");
        assert!(
            state.extra_phase_resume.is_empty(),
            "resume stack empties once the inserted phase completes"
        );
        assert!(state.extra_phases.is_empty());
    }

    /// CR 500.8: two "additional beginning phase" effects after the same anchor
    /// run two full beginning phases in succession before the turn resumes.
    #[test]
    fn two_additional_beginning_phases_run_in_succession() {
        use crate::game::turns::advance_phase;

        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Untap,
            Phase::PostCombatMain,
            vec![],
            PlayerId(0),
        );
        // Two separate resolutions (e.g. two Sphinxes of the Second Sun).
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.extra_phases.len(), 2);

        let mut sequence = Vec::new();
        // Drive to the resumed End step, recording each phase entered.
        for _ in 0..8 {
            advance_phase(&mut state, &mut events);
            sequence.push(state.phase);
            if state.phase == Phase::End {
                break;
            }
        }
        assert_eq!(
            sequence,
            vec![
                Phase::Untap,
                Phase::Upkeep,
                Phase::Draw,
                Phase::Untap,
                Phase::Upkeep,
                Phase::Draw,
                Phase::End,
            ],
            "two full beginning phases then resume after the anchor"
        );
        assert!(state.extra_phase_resume.is_empty());
        assert!(state.extra_phases.is_empty());
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

    /// CR 501.1 + CR 500.8: "additional beginning phase after this phase"
    /// resolving in a postcombat main phase schedules a beginning phase
    /// (`phase: Untap`) anchored to that main phase (`last_step_of_phase`).
    #[test]
    fn additional_beginning_phase_anchors_to_resolving_main_phase() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Untap,
            // `after`/`followed_by` are don't-cares for the beginning-phase shape.
            Phase::PostCombatMain,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.extra_phases,
            vec![ep(Phase::PostCombatMain, Phase::Untap)]
        );
    }

    /// CR 501.1 + CR 500.8: Cyclonus resolves during the combat damage step, so
    /// the inserted beginning phase anchors to `EndCombat`
    /// (`last_step_of_phase(CombatDamage)`).
    #[test]
    fn additional_beginning_phase_from_combat_anchors_to_end_combat() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::CombatDamage,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Untap,
            Phase::PostCombatMain,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_phases, vec![ep(Phase::EndCombat, Phase::Untap)]);
    }

    /// CR 500.10a: the "you get" controller-restriction does NOT gate the "there
    /// is an additional … phase" wording. Shadow of the Second Sun enchants
    /// another player, so its controller differs from the active player, yet the
    /// beginning phase is still added to the active player's turn in progress.
    #[test]
    fn additional_beginning_phase_ignores_cr_500_10a_controller_gate() {
        let mut state = GameState {
            active_player: PlayerId(1),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        // Controller (0) != active player (1) — the CR 500.10a gate would drop an
        // ordinary "you get" phase, but the beginning-phase branch schedules it.
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Untap,
            Phase::PostCombatMain,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.extra_phases,
            vec![ep(Phase::PostCombatMain, Phase::Untap)]
        );
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
