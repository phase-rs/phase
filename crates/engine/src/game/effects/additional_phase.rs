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

    // CR 608.2c + CR 500.8: `Phase::PreCombatMain` in `after` is the parser's
    // sentinel for "after this [main|combat] phase" — the anchor is the LAST STEP
    // of the phase this effect is resolving in, resolved here because only the
    // resolver can see `state.phase`. CR 608.2c ("read the whole text and apply
    // the rules of English") is the authority for "this phase" denoting the
    // resolving phase; CR 500.8 then adds the insertion directly after it.
    // Mirrors the beginning-phase branch above. Combat-resolved sources (Aurelia,
    // Godo, Combat Celebrant, Najeela, Scourge of the Throne, Great Train Heist,
    // …) yield `EndCombat`, identical to the pre-sentinel default. A literal
    // `after` (Upkeep, End, EndCombat) passes through untouched.
    //
    // This remap is deliberately class-agnostic. The "after this MAIN phase"
    // wording additionally creates nothing at all outside a main phase (Gatherer,
    // Fury of the Horde: "No new phases are created."), but that precondition is
    // information only the PARSER holds — it is attached to the clause as an
    // `AbilityCondition::CurrentPhaseIs` gate by
    // `oracle_effect::imperative::this_phase_anchor_gate`, so this effect never
    // resolves at all in that case. Re-deriving it here is impossible: `after`
    // carries no qualifier.
    let after = if after == Phase::PreCombatMain {
        crate::game::turns::last_step_of_phase(state.phase)
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
    // CR 500.8: every bundle anchors at the SAME insertion point.
    // `advance_phase_once` chains them through the resume frame ("another phase
    // queued after the same anchor runs next"), so Full Throttle's two combats
    // run back to back BEFORE the turn resumes at the anchor's natural
    // successor, per "the most recently created phase will occur first". The
    // former `EndCombat` re-anchor for `i > 0` is removed: it made the second
    // combat reachable only by consuming the natural combat's slot, which lost a
    // whole combat phase. CR 500.8 is the authority — an extra phase is ADDED
    // directly after the specified phase, so it never displaces a phase the turn
    // already has (CR 505.1a corroborates by contemplating "a turn in which an
    // effect has caused an additional combat phase and an additional main phase
    // to be created", but it only classifies which main is precombat).
    for _ in 0..count {
        for &follow_up in followed_by.iter().rev() {
            state.extra_phases.push(ExtraPhase {
                anchor: after,
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
            anchor: after,
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
            detached_remainder: crate::types::ability::DetachedRemainder::NoProducer,
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
            force_block_attacker: None,
            target_incarnations: Vec::new(),
            selected_target_incarnations: Vec::new(),
            targets: vec![],
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_player: None,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            modal_instruction_ordinal: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            noted_mana_payment: None,
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
            distribute: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            sibling_condition: crate::types::ability::SiblingCondition::Dependent,
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

    /// CR 500.8 + CR 608.2c: the `after: PreCombatMain` sentinel resolves to the
    /// LAST STEP of the phase the effect resolves in — here the postcombat main
    /// phase. CR 500.8: every bundle of a `count > 1` effect anchors at that same
    /// insertion point, so both extra combats run before the turn resumes.
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
                ep(Phase::PostCombatMain, Phase::BeginCombat),
            ]
        );
    }

    /// CR 500.8 + CR 506.1: the Group B wording ("after this phase", resolving
    /// during combat — Aurelia, Port Razer, Najeela, …) reaches the resolver as
    /// the `PreCombatMain` sentinel and must resolve to an `EndCombat` anchor,
    /// exactly as the pre-sentinel literal default did.
    #[test]
    fn additional_phase_pushes_begin_combat() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::DeclareAttackers,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
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
            phase: Phase::DeclareAttackers,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
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
            phase: Phase::DeclareAttackers,
            ..Default::default()
        };
        let mut events = Vec::new();

        // First effect: additional combat
        let ability1 = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability1, &mut events).unwrap();

        // Second effect: another additional combat (most recent → first)
        let ability2 = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
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
            phase: Phase::DeclareAttackers,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            Phase::PreCombatMain,
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

    /// CR 500.8 (Full Throttle): every bundle of a `count > 1` effect anchors at
    /// the SAME insertion point — the phase the effect resolved in. The former
    /// `EndCombat` re-anchor for `i > 0` was wrong: the second combat was then
    /// inserted after the FIRST inserted combat's end-of-combat step, which is
    /// where the turn's own natural combat phase would otherwise resume, so the
    /// extra combat consumed the natural one (CR 500.8 — an extra phase is ADDED
    /// directly after the specified phase, so it never displaces one the turn
    /// already has).
    #[test]
    fn additional_combat_count_anchors_every_bundle_at_the_insertion_point() {
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
                ep(Phase::PreCombatMain, Phase::BeginCombat),
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
            vec![crate::types::game_state::ExtraPhaseResume {
                anchor: Phase::PostCombatMain,
                inserted: Phase::Untap,
            }],
            "resume frame records the anchor and the inserted beginning phase"
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

    /// CR 500.8 (Full Throttle, cast in a precombat main phase): an extra phase is
    /// ADDED directly after the specified phase, so "there are two additional
    /// combat phases" grants two combats ADDITIONAL to
    /// the turn's own combat phase, so the turn runs three. The old assertion
    /// (two combats, then straight to the postcombat main) encoded the swallowed
    /// natural combat: the second bundle was anchored at the first inserted
    /// combat's `EndCombat`, which is exactly where the turn would otherwise have
    /// resumed at its natural `BeginCombat`.
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

        let mut sequence = Vec::new();
        for _ in 0..24 {
            advance_phase(&mut state, &mut events);
            sequence.push(state.phase);
            if state.phase == Phase::End {
                break;
            }
        }

        let combat = || {
            [
                Phase::BeginCombat,
                Phase::DeclareAttackers,
                Phase::DeclareBlockers,
                Phase::CombatDamage,
                Phase::EndCombat,
            ]
        };
        let mut expected: Vec<Phase> = Vec::new();
        expected.extend(combat()); // first inserted combat
        expected.extend(combat()); // second inserted combat
        expected.extend(combat()); // the turn's own natural combat
        expected.push(Phase::PostCombatMain);
        expected.push(Phase::End);
        assert_eq!(sequence, expected);
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());
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

    /// CR 500.8 + CR 506.1 + CR 608.2c: the "after this phase" sentinel resolving
    /// anywhere inside the combat phase anchors at that phase's LAST step, so
    /// the inserted combat begins only once the current combat is over. This is
    /// the building-block proof for the whole "after this phase" class that
    /// resolves during combat (Aurelia, Godo, Combat Celebrant, Najeela, Port
    /// Razer, Scourge of the Throne, Hellkite Charger, … — 30+ cards): reverting
    /// the sentinel remap makes every row anchor at `PreCombatMain` and the
    /// extra combat becomes unreachable.
    #[test]
    fn current_phase_sentinel_resolves_to_end_combat_from_every_combat_step() {
        for resolving_in in [
            Phase::BeginCombat,
            Phase::DeclareAttackers,
            Phase::DeclareBlockers,
            Phase::CombatDamage,
            Phase::EndCombat,
        ] {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase: resolving_in,
                ..Default::default()
            };
            let mut events = Vec::new();
            let ability = make_ability(
                TargetFilter::Controller,
                Phase::BeginCombat,
                Phase::PreCombatMain,
                vec![],
                PlayerId(0),
            );

            resolve(&mut state, &ability, &mut events).unwrap();

            assert_eq!(
                state.extra_phases,
                vec![ep(Phase::EndCombat, Phase::BeginCombat)],
                "resolving in {resolving_in:?} must anchor the extra combat at end of combat",
            );
        }
    }

    /// CR 500.8 + CR 608.2c: the same building block across EVERY phase and step
    /// of a turn — the sentinel always resolves to the LAST STEP of the phase the
    /// effect is resolving in (CR 501.1 beginning phase → draw step; CR 505.1
    /// main phases have no steps; CR 506.1 combat → end of combat; CR 512.1
    /// ending phase → cleanup step).
    ///
    /// The `End` / `Cleanup` rows are the ones that matter for the turn
    /// boundary: they produce a `Cleanup` anchor, i.e. a phase inserted directly
    /// after the cleanup step. `turns::advance_phase_once` must still start the
    /// next turn once that insertion is exhausted — see
    /// `turns::tests::cleanup_anchored_insertion_still_ends_the_turn`.
    #[test]
    fn current_phase_sentinel_resolves_to_the_last_step_of_every_phase() {
        for (resolving_in, expected_anchor) in [
            (Phase::Untap, Phase::Draw),
            (Phase::Upkeep, Phase::Draw),
            (Phase::Draw, Phase::Draw),
            (Phase::PreCombatMain, Phase::PreCombatMain),
            (Phase::BeginCombat, Phase::EndCombat),
            (Phase::DeclareAttackers, Phase::EndCombat),
            (Phase::DeclareBlockers, Phase::EndCombat),
            (Phase::CombatDamage, Phase::EndCombat),
            (Phase::EndCombat, Phase::EndCombat),
            (Phase::PostCombatMain, Phase::PostCombatMain),
            (Phase::End, Phase::Cleanup),
            (Phase::Cleanup, Phase::Cleanup),
        ] {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase: resolving_in,
                ..Default::default()
            };
            let mut events = Vec::new();
            let ability = make_ability(
                TargetFilter::Controller,
                Phase::BeginCombat,
                Phase::PreCombatMain,
                vec![],
                PlayerId(0),
            );

            resolve(&mut state, &ability, &mut events).unwrap();

            assert_eq!(
                state.extra_phases,
                vec![ep(expected_anchor, Phase::BeginCombat)],
                "resolving in {resolving_in:?} must anchor at {expected_anchor:?}",
            );
        }
    }

    /// CR 500.8 ("if multiple extra phases are created after the same phase, the
    /// most recently created phase will occur first"): two effects
    /// resolving in the SAME postcombat main phase — one with a follow-up main
    /// phase (Relentless Assault) and one without (Port Razer's wording) — both
    /// anchor at that main phase, and the more recent bundle runs first. The
    /// discriminating property is that neither bundle's follow-up main is
    /// consumed by the other bundle's combat: the follow-up main is anchored at
    /// the shared insertion point, not at the inserted combat's end.
    #[test]
    fn two_main_anchored_bundles_interleave_most_recent_first() {
        use crate::game::turns::advance_phase;

        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();

        // First resolution: extra combat followed by an extra main phase.
        resolve(
            &mut state,
            &make_ability(
                TargetFilter::Controller,
                Phase::BeginCombat,
                Phase::PreCombatMain,
                vec![Phase::PostCombatMain],
                PlayerId(0),
            ),
            &mut events,
        )
        .unwrap();
        // Second resolution: a bare extra combat (most recent → runs first).
        resolve(
            &mut state,
            &make_ability(
                TargetFilter::Controller,
                Phase::BeginCombat,
                Phase::PreCombatMain,
                vec![],
                PlayerId(0),
            ),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.extra_phases,
            vec![
                ep(Phase::PostCombatMain, Phase::PostCombatMain),
                ep(Phase::PostCombatMain, Phase::BeginCombat),
                ep(Phase::PostCombatMain, Phase::BeginCombat),
            ],
            "every entry anchors at the shared insertion point",
        );

        let mut sequence = Vec::new();
        for _ in 0..24 {
            advance_phase(&mut state, &mut events);
            sequence.push(state.phase);
            if state.phase == Phase::End {
                break;
            }
        }

        let combat = || {
            [
                Phase::BeginCombat,
                Phase::DeclareAttackers,
                Phase::DeclareBlockers,
                Phase::CombatDamage,
                Phase::EndCombat,
            ]
        };
        let mut expected: Vec<Phase> = Vec::new();
        expected.extend(combat()); // bare bundle (most recent) first
        expected.extend(combat()); // first bundle's combat
        expected.push(Phase::PostCombatMain); // first bundle's follow-up main
        expected.push(Phase::End); // resume after the anchor
        assert_eq!(sequence, expected);
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());
    }
}
