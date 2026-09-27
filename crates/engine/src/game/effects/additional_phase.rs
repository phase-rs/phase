use crate::game::quantity::resolve_quantity;
use crate::game::turns::{first_phase_of_turn_has_ended, last_step_of_phase};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ExtraPhaseAnchor, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{ExtraPhase, GameState};
use crate::types::phase::{Phase, PhaseGroup, TurnSegment};

/// CR 500.8 + CR 500.9 + CR 500.10: the step an added phase or step follows.
/// "This step/phase" is the step/phase in which the effect resolves. `None`:
/// there is no such phase to add after (CR 500.8), because the text names a
/// kind of phase (CR 505.1 "this main phase") that the effect is not resolving
/// in, or the first phase of a kind this turn and that phase has already ended
/// (CR 505.1b).
fn anchor_step(after: &ExtraPhaseAnchor, state: &GameState) -> Option<Phase> {
    let current = state.phase;
    match after {
        ExtraPhaseAnchor::Step(step) => Some(*step),
        ExtraPhaseAnchor::ThisStep => Some(current),
        ExtraPhaseAnchor::ThisPhase { named: None } => Some(last_step_of_phase(current)),
        ExtraPhaseAnchor::ThisPhase {
            named: Some(groups),
        } => groups
            .contains(&current.group())
            .then(|| last_step_of_phase(current)),
        ExtraPhaseAnchor::FirstOfTurn(group) => {
            (!first_phase_of_turn_has_ended(state, *group)).then(|| group.last_step())
        }
    }
}

/// CR 500.8 + CR 500.9 + CR 500.10: what an `Effect::AdditionalPhase` adds,
/// from the step it names and the kind of its anchor.
fn added_segment(step: Phase, after: &ExtraPhaseAnchor) -> TurnSegment {
    match step {
        // CR 501.1 + CR 506.1 + CR 505.2: `Untap` names an added beginning
        // phase by its first step, and `BeginCombat` an added combat phase; a
        // main phase has no steps. Each is a whole phase (CR 500.8).
        Phase::Untap | Phase::BeginCombat | Phase::PreCombatMain | Phase::PostCombatMain => {
            TurnSegment::Phase(step.group())
        }
        Phase::Upkeep
        | Phase::Draw
        | Phase::DeclareAttackers
        | Phase::DeclareBlockers
        | Phase::CombatDamage
        | Phase::EndCombat
        | Phase::End
        | Phase::Cleanup => match after {
            // CR 500.9: a step added after a step joins the phase in progress.
            ExtraPhaseAnchor::ThisStep | ExtraPhaseAnchor::Step(_) => TurnSegment::Step(step),
            // CR 500.10 + CR 500.11: a step added after a phase sits in a phase
            // created to hold only that step.
            ExtraPhaseAnchor::ThisPhase { .. } | ExtraPhaseAnchor::FirstOfTurn(_) => {
                TurnSegment::CreatedPhase(step)
            }
        },
    }
}

/// CR 500.8: Add extra phases to the current turn via a LIFO stack.
/// CR 500.10a: a phase an effect says "you get" is added only to its
/// controller's own turn; an expletive "there is" phase is added to the turn in
/// progress.
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
                after,
                followed_by,
                count,
                attacker_restriction,
            ),
            _ => return Err(EffectError::MissingParam("expected AdditionalPhase".into())),
        };

    // CR 603.7a + CR 608.2c: this instruction replaces what an earlier one of the
    // same resolution published, so a return below that adds nothing publishes
    // that nothing was added, and "that combat" then names no phase.
    state.last_added_phase_ids.clear();

    // CR 500.8 + CR 505.1: an "after this main phase" insert resolving outside a
    // main phase has no phase to follow, so no phases are added, and no copy of
    // the bundle is (Relentless Assault / Full Throttle rulings). CR 505.1b: nor
    // has an insert after the first phase of a kind this turn once that phase
    // has ended (World at War / Swinging Ship rulings).
    let Some(anchor) = anchor_step(after, state) else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AdditionalPhase,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    // CR 500.10a: a step or phase an effect says "you get" is added only if it
    // would be added to its controller's own turn; the engine applies the same
    // own-turn gate to a player the text names ("that player gets", Paradox
    // Haze, whose trigger resolves only on that player's turn).
    // "There is / are an additional …" (`TargetFilter::None`) names no player:
    // the step or phase is added to the turn in progress, whoever controls the
    // source (Take the Bait and Full Throttle on an opponent's turn; Shadow of
    // the Second Sun on the enchanted player's turn).
    let recipient = match target {
        TargetFilter::None => None,
        TargetFilter::Controller | TargetFilter::SelfRef => Some(ability.controller),
        TargetFilter::TriggeringPlayer => Some(
            state
                .current_trigger_event
                .as_ref()
                .and_then(|event| crate::game::targeting::extract_player_from_event(event, state))
                .unwrap_or(ability.controller),
        ),
        // CR 500.10a: an inherited wildcard over the rest of `TargetFilter`,
        // most of whose variants name objects rather than players. It
        // treats every other non-`None` filter as a grant to a player (the
        // first player target, else the controller) and so applies the
        // own-turn gate to it. A new filter that names no player lands here
        // with no compile error, so route it to its own arm above.
        _ => Some(match ability.targets.first() {
            Some(TargetRef::Player(pid)) => *pid,
            _ => ability.controller,
        }),
    };
    if recipient.is_some_and(|player| player != state.active_player) {
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
    // bundle instead of one; each bundle is anchored at the step `anchor_step`
    // resolves from `after` (for Obeka's `ThisPhase` resolving in combat
    // damage, EndCombat).
    // Fixed quantities preserve legacy single-push.
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
    // `take_scheduled_successor` LIFO scan takes the primary phase first. Repeat
    // the bundle `count` times; every copy keeps the resolved anchor.
    // CR 500.8 + CR 500.10: when a copy's unit ends, the turn continues as though
    // the anchor had just ended, so `turns::take_scheduled_successor` runs the
    // copies back to back, newest first (Full Throttle: two combats with no main
    // phase between them; Obeka's upkeeps).
    // CR 500.8: every scheduled entry carries its own minted identity, so
    // entries that share an anchor and a phase stay distinct.
    let segment = added_segment(phase, after);
    for _ in 0..count {
        for &follow_up in followed_by.iter().rev() {
            let id = state.mint_extra_phase_id();
            state.extra_phases.push(ExtraPhase {
                anchor,
                segment: added_segment(follow_up, after),
                attacker_restriction: None,
                attacker_restriction_source: None,
                id,
            });
        }
        // CR 508.1c: Only the scheduled combat phase carries the attacker
        // restriction; follow-up main/upkeep phases never restrict attacks.
        // CR 611.2c: Record the scheduling spell's source ObjectId so that
        // `passes_combat_attacker_restriction` can evaluate source-relative
        // filter predicates against the actual source rather than ObjectId(0).
        let restriction = if segment == TurnSegment::Phase(PhaseGroup::Combat) {
            resolved_restriction.clone()
        } else {
            None
        };
        let id = state.mint_extra_phase_id();
        // CR 603.7a: publish the primary phase for a following "at the
        // beginning of that combat"; a follow-up main phase is not "that
        // combat".
        state.last_added_phase_ids.push(id);
        state.extra_phases.push(ExtraPhase {
            anchor,
            segment,
            attacker_restriction_source: if restriction.is_some() {
                Some(ability.source_id)
            } else {
                None
            },
            attacker_restriction: restriction,
            id,
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
    use crate::types::ability::{
        AbilityKind, ExtraPhaseAnchor, QuantityExpr, QuantityRef, SpellContext, TargetFilter,
    };
    use crate::types::game_state::InsertedPhaseResume;
    use crate::types::identifiers::{ExtraPhaseId, ObjectId};
    use crate::types::phase::{Phase, PhaseGroup};
    use crate::types::player::PlayerId;
    use std::collections::BTreeSet;

    fn make_ability(
        target: TargetFilter,
        phase: Phase,
        after: ExtraPhaseAnchor,
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
        after: ExtraPhaseAnchor,
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
            cast_occurrence: None,
            source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            force_block_attacker: None,
            target_incarnations: Vec::new(),
            selected_target_incarnations: Vec::new(),
            illegal_target_slots: Vec::new(),
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
            cost_paid_objects: Vec::new(),
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

    /// Test helper: `entry` with its minted `id` cleared, for assertions about
    /// anchors, phases and restrictions. The ids themselves are pinned by
    /// `each_scheduled_entry_is_minted_a_distinct_nonzero_id`.
    fn unminted(entry: &ExtraPhase) -> ExtraPhase {
        ExtraPhase {
            id: ExtraPhaseId::default(),
            ..entry.clone()
        }
    }

    /// Test helper: the scheduled entries, each `unminted`.
    fn scheduled(state: &GameState) -> Vec<ExtraPhase> {
        state.extra_phases.iter().map(unminted).collect()
    }

    /// Test helper: an ordinary (unrestricted) `ExtraPhase`.
    fn ep(anchor: Phase, segment: TurnSegment) -> ExtraPhase {
        ExtraPhase {
            anchor,
            segment,
            attacker_restriction: None,
            attacker_restriction_source: None,
            id: ExtraPhaseId::default(),
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
            ExtraPhaseAnchor::this_main_phase(),
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![
                ep(
                    Phase::PostCombatMain,
                    TurnSegment::Phase(PhaseGroup::Combat)
                );
                2
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
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 500.8: anchor = EndCombat so consumption happens after the
        // current combat phase ends (not mid-combat).
        assert_eq!(
            scheduled(&state),
            vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))]
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
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![Phase::PostCombatMain],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // LIFO: PostCombatMain pushed first, BeginCombat on top → on the
        // first EndCombat encountered, BeginCombat (the more recent entry)
        // is consumed; the second EndCombat consumes PostCombatMain.
        assert_eq!(
            scheduled(&state),
            vec![
                ep(
                    Phase::EndCombat,
                    TurnSegment::Phase(PhaseGroup::PostcombatMain)
                ),
                ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat)),
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
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability1, &mut events).unwrap();

        // Second effect: another additional combat (most recent → first)
        let ability2 = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability2, &mut events).unwrap();

        let begin_combat_after_end = ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat));
        assert_eq!(
            scheduled(&state),
            vec![
                begin_combat_after_end.clone(),
                begin_combat_after_end.clone()
            ]
        );

        // CR 500.8: Pop from end → most recent first
        assert_eq!(
            state.extra_phases.pop().as_ref().map(unminted),
            Some(begin_combat_after_end.clone())
        );
        assert_eq!(
            state.extra_phases.pop().as_ref().map(unminted),
            Some(begin_combat_after_end)
        );
    }

    /// A bundle of an added combat followed by an added main phase, after the
    /// precombat main phase the effect resolves in.
    fn combat_then_main_bundle() -> ResolvedAbility {
        make_ability(
            TargetFilter::None,
            Phase::BeginCombat,
            ExtraPhaseAnchor::this_main_phase(),
            vec![Phase::PostCombatMain],
            PlayerId(0),
        )
    }

    /// CR 500.8: every entry a resolution schedules is minted its own nonzero
    /// identity, and a second resolution of the same bundle mints new ones, so
    /// entries that agree in anchor and phase stay distinct.
    #[test]
    fn each_scheduled_entry_is_minted_a_distinct_nonzero_id() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        resolve(&mut state, &combat_then_main_bundle(), &mut events).unwrap();
        resolve(&mut state, &combat_then_main_bundle(), &mut events).unwrap();
        assert_eq!(
            scheduled(&state),
            vec![
                ep(
                    Phase::PreCombatMain,
                    TurnSegment::Phase(PhaseGroup::PostcombatMain)
                ),
                ep(Phase::PreCombatMain, TurnSegment::Phase(PhaseGroup::Combat)),
                ep(
                    Phase::PreCombatMain,
                    TurnSegment::Phase(PhaseGroup::PostcombatMain)
                ),
                ep(Phase::PreCombatMain, TurnSegment::Phase(PhaseGroup::Combat)),
            ],
            "reach guard: both resolutions scheduled the whole bundle"
        );

        let ids: Vec<ExtraPhaseId> = state.extra_phases.iter().map(|entry| entry.id).collect();
        assert!(
            ids.iter().all(|id| *id != ExtraPhaseId::default()),
            "no scheduled entry carries the unminted default: {ids:?}"
        );
        assert_eq!(
            ids.iter().collect::<BTreeSet<_>>().len(),
            ids.len(),
            "every scheduled entry has its own id: {ids:?}"
        );
    }

    /// CR 500.8 + CR 500.10: when the turn takes a scheduled entry, the unit in
    /// progress it starts records that entry's identity, for every entry of
    /// every copy of the bundle.
    #[test]
    fn each_taken_entry_records_its_id_on_its_unit() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        resolve(&mut state, &combat_then_main_bundle(), &mut events).unwrap();
        resolve(&mut state, &combat_then_main_bundle(), &mut events).unwrap();

        let mut taken = 0;
        for _ in 0..64 {
            if state.extra_phases.is_empty() {
                break;
            }
            let before = state.extra_phases.clone();
            crate::game::turns::advance_phase(&mut state, &mut events);
            let Some(entry) = before
                .iter()
                .find(|entry| !state.extra_phases.contains(entry))
            else {
                continue;
            };
            taken += 1;
            assert_eq!(
                state.phase,
                entry.segment.first_step(),
                "the taken entry's unit begins"
            );
            assert_eq!(
                state.extra_phase_resume.last().map(|unit| unit.entry),
                Some(entry.id),
                "the unit in progress records the entry it was taken from"
            );
        }
        assert_eq!(taken, 4, "reach guard: every scheduled entry was taken");
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
            ExtraPhaseAnchor::Step(Phase::EndCombat),
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
            phase: Phase::Upkeep,
            current_trigger_event: Some(GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            }),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::TriggeringPlayer,
            Phase::Upkeep,
            ExtraPhaseAnchor::ThisStep,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(Phase::Upkeep, TurnSegment::Step(Phase::Upkeep))]
        );
    }

    /// CR 500.8 + CR 500.10 + CR 510.2: Obeka, Splitter of Seconds — "you get
    /// that many additional upkeep steps after this phase" resolving in the
    /// combat damage step pushes one upkeep per point of combat damage, each
    /// anchored at the combat phase's final step (EndCombat).
    #[test]
    fn additional_phase_count_from_event_context_amount_pushes_n_phases() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::CombatDamage,
            current_trigger_event: Some(GameEvent::DamageDealt {
                source_id: ObjectId(1),
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
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(Phase::EndCombat, TurnSegment::CreatedPhase(Phase::Upkeep)); 5],
            "5 combat damage should schedule 5 additional upkeep steps after combat"
        );
    }

    /// CR 500.8 (Full Throttle): every copy of a counted combat bundle keeps the
    /// resolved main-phase anchor. The turn machine runs them back to back
    /// (`additional_combat_count_advances_through_both_extra_phases`).
    #[test]
    fn additional_combat_count_shares_the_main_phase_anchor() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::this_main_phase(),
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(Phase::PreCombatMain, TurnSegment::Phase(PhaseGroup::Combat)); 2]
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
            ExtraPhaseAnchor::ThisPhase { named: None },
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
            vec![InsertedPhaseResume {
                anchor: Phase::PostCombatMain,
                segment: TurnSegment::Phase(PhaseGroup::Beginning),
                entry: ExtraPhaseId(1),
            }],
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
            ExtraPhaseAnchor::ThisPhase { named: None },
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
            ExtraPhaseAnchor::this_main_phase(),
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
        assert_eq!(
            state.phase,
            Phase::BeginCombat,
            "the natural combat follows (CR 500.8; Moraug ruling)"
        );
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());

        while state.phase != Phase::EndCombat {
            advance_phase(&mut state, &mut events);
        }
        advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::PostCombatMain);
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
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(
                Phase::PostCombatMain,
                TurnSegment::Phase(PhaseGroup::Beginning)
            )]
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
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(
                Phase::EndCombat,
                TurnSegment::Phase(PhaseGroup::Beginning)
            )]
        );
    }

    /// CR 500.10a: the "you get" restriction does not gate the "there is an
    /// additional … phase" wording (`TargetFilter::None`). Shadow of the Second
    /// Sun enchants another player, so its controller differs from the active
    /// player, yet the beginning phase is added to the turn in progress.
    #[test]
    fn expletive_beginning_phase_is_added_to_an_opponents_turn() {
        let mut state = GameState {
            active_player: PlayerId(1),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::None,
            Phase::Untap,
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(
                Phase::PostCombatMain,
                TurnSegment::Phase(PhaseGroup::Beginning)
            )]
        );
    }

    /// CR 500.10a: a beginning phase granted to the controller ("you get") on
    /// another player's turn adds nothing — the phase kind grants no exemption.
    /// Reach guard: the same ability on its controller's own turn adds it.
    #[test]
    fn granted_beginning_phase_is_gated_like_any_granted_phase() {
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Untap,
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );
        for (active, expected) in [
            (
                PlayerId(0),
                vec![ep(
                    Phase::PostCombatMain,
                    TurnSegment::Phase(PhaseGroup::Beginning),
                )],
            ),
            (PlayerId(1), vec![]),
        ] {
            let mut state = GameState {
                active_player: active,
                phase: Phase::PostCombatMain,
                ..Default::default()
            };
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            assert_eq!(scheduled(&state), expected, "active player {active:?}");
            assert!(events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::AdditionalPhase,
                    ..
                }
            )));
        }
    }

    /// CR 500.10a: an expletive combat phase ("there is an additional combat
    /// phase", `TargetFilter::None`) resolving on an opponent's turn is added
    /// to that turn (Take the Bait); the same text granted to the controller
    /// ("you get") adds nothing there.
    #[test]
    fn expletive_combat_phase_on_an_opponents_turn_is_added_to_that_turn() {
        for (target, expected) in [
            (
                TargetFilter::None,
                vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))],
            ),
            (TargetFilter::Controller, vec![]),
        ] {
            let mut state = GameState {
                active_player: PlayerId(1),
                phase: Phase::DeclareBlockers,
                ..Default::default()
            };
            let mut events = Vec::new();
            let ability = make_ability(
                target.clone(),
                Phase::BeginCombat,
                ExtraPhaseAnchor::ThisPhase { named: None },
                vec![],
                PlayerId(0),
            );
            resolve(&mut state, &ability, &mut events).unwrap();
            assert_eq!(scheduled(&state), expected, "target {target:?}");
        }
    }

    /// CR 500.8: a combat phase added "after this phase" follows the phase the
    /// effect resolves in, whichever it is: after a main phase the new combat
    /// comes before the next natural phase (Moraug in the precombat main phase);
    /// during combat it follows end of combat. "After this combat phase"
    /// (Raphael) and "after this one" (Save Point) add nothing outside combat
    /// (CR 506.1).
    #[test]
    fn this_phase_combat_anchor_follows_the_resolving_phase() {
        let combat = ExtraPhaseAnchor::ThisPhase {
            named: Some(vec![PhaseGroup::Combat]),
        };
        for (after, resolving, expected) in [
            (
                ExtraPhaseAnchor::ThisPhase { named: None },
                Phase::PreCombatMain,
                vec![ep(
                    Phase::PreCombatMain,
                    TurnSegment::Phase(PhaseGroup::Combat),
                )],
            ),
            (
                ExtraPhaseAnchor::ThisPhase { named: None },
                Phase::PostCombatMain,
                vec![ep(
                    Phase::PostCombatMain,
                    TurnSegment::Phase(PhaseGroup::Combat),
                )],
            ),
            (
                ExtraPhaseAnchor::ThisPhase { named: None },
                Phase::CombatDamage,
                vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))],
            ),
            (
                combat.clone(),
                Phase::CombatDamage,
                vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))],
            ),
            (combat.clone(), Phase::PreCombatMain, vec![]),
        ] {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase: resolving,
                ..Default::default()
            };
            let mut events = Vec::new();
            let ability = make_ability(
                TargetFilter::None,
                Phase::BeginCombat,
                after.clone(),
                vec![],
                PlayerId(0),
            );
            resolve(&mut state, &ability, &mut events).unwrap();
            assert_eq!(
                scheduled(&state),
                expected,
                "{after:?} resolving in {resolving:?}"
            );
        }
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
            ExtraPhaseAnchor::this_main_phase(),
            vec![],
            PlayerId(0),
        );
        // Stamp the restriction + chosen targets exactly as the parser fold and
        // `resolve_ability_chain` propagation would produce them.
        ability.effect = Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::BeginCombat,
            after: ExtraPhaseAnchor::this_main_phase(),
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
        assert_eq!(scheduled.segment, TurnSegment::Phase(PhaseGroup::Combat));
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

    /// CR 500.9: "an additional upkeep step after this step" resolving in the
    /// upkeep anchors at that upkeep (Paradox Haze).
    #[test]
    fn this_step_anchor_resolves_to_the_current_step() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::Upkeep,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Upkeep,
            ExtraPhaseAnchor::ThisStep,
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(Phase::Upkeep, TurnSegment::Step(Phase::Upkeep))]
        );
    }

    /// CR 500.8: a fixed step anchor ignores the step the effect resolves in.
    #[test]
    fn fixed_step_anchor_ignores_the_resolving_phase() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            scheduled(&state),
            vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))]
        );
    }

    /// CR 500.8 + CR 505.1: "after this main phase" resolving outside a main
    /// phase adds no phases — neither the first combat nor its later copies
    /// nor a `followed_by` main phase. Relentless Assault ruling: "creates an
    /// additional combat and main phase only if it resolves during a main
    /// phase"; Full Throttle ruling: "there are no additional combat phases
    /// this turn". Reach guard: the same shape resolving in a postcombat main
    /// phase adds two combats
    /// (`additional_phase_after_this_main_phase_uses_active_main_as_anchor`).
    #[test]
    fn this_main_phase_anchor_outside_a_main_phase_adds_nothing() {
        let full_throttle = make_ability_with_count(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::this_main_phase(),
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );
        let relentless_assault = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::this_main_phase(),
            vec![Phase::PostCombatMain],
            PlayerId(0),
        );
        let created_upkeep = vec![InsertedPhaseResume {
            anchor: Phase::EndCombat,
            segment: TurnSegment::CreatedPhase(Phase::Upkeep),
            entry: ExtraPhaseId::default(),
        }];
        let rows = [
            (&full_throttle, Phase::Upkeep, vec![]),
            (&full_throttle, Phase::Draw, vec![]),
            (&full_throttle, Phase::DeclareBlockers, vec![]),
            (&full_throttle, Phase::EndCombat, vec![]),
            (&full_throttle, Phase::End, vec![]),
            (&full_throttle, Phase::Upkeep, created_upkeep),
            (&relentless_assault, Phase::Upkeep, vec![]),
        ];
        for (ability, phase, extra_phase_resume) in rows {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase,
                extra_phase_resume,
                ..Default::default()
            };
            let mut events = Vec::new();

            resolve(&mut state, ability, &mut events).unwrap();

            assert!(
                state.extra_phases.is_empty(),
                "resolving in {phase:?} must add no phases, got {:?}",
                state.extra_phases
            );
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::AdditionalPhase,
                        ..
                    }
                )),
                "resolving in {phase:?} still reports the effect as resolved"
            );
        }
    }

    /// Obeka ruling + CR 500.8: a combat added during end of combat, after
    /// Obeka's trigger resolved, is the most recently created insert at the
    /// same anchor, so it runs before the upkeeps (CR 500.10); the turn then
    /// continues to the postcombat main phase.
    #[test]
    fn combat_added_at_end_of_combat_runs_before_obeka_upkeeps() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::CombatDamage,
            ..Default::default()
        };
        let mut events = Vec::new();
        let obeka = make_ability_with_count(
            TargetFilter::Controller,
            Phase::Upkeep,
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );
        resolve(&mut state, &obeka, &mut events).unwrap();
        state.phase = Phase::EndCombat;
        let combat = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &combat, &mut events).unwrap();

        let mut sequence = Vec::new();
        for _ in 0..16 {
            crate::game::turns::advance_phase(&mut state, &mut events);
            sequence.push(state.phase);
            if state.phase == Phase::PostCombatMain {
                break;
            }
        }

        assert_eq!(sequence.first(), Some(&Phase::BeginCombat), "{sequence:?}");
        assert!(
            sequence.ends_with(&[
                Phase::EndCombat,
                Phase::Upkeep,
                Phase::Upkeep,
                Phase::PostCombatMain
            ]),
            "added combat first, then both upkeeps, then postcombat main: {sequence:?}"
        );
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());
    }

    /// CR 500.10 + CR 500.11: "an additional upkeep step after this phase"
    /// resolving in the precombat main phase (Untap, Upkeep, Draw mode 2)
    /// creates a beginning phase holding only that upkeep, after which the turn
    /// continues to the natural combat.
    #[test]
    fn this_phase_upkeep_in_precombat_main_runs_before_the_natural_combat() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            TargetFilter::Controller,
            Phase::Upkeep,
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            scheduled(&state),
            vec![ep(
                Phase::PreCombatMain,
                TurnSegment::CreatedPhase(Phase::Upkeep)
            )]
        );

        crate::game::turns::advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::Upkeep);
        crate::game::turns::advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::BeginCombat);
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());
    }

    /// CR 500.10a: Obeka's "you get" upkeeps resolving on another player's turn
    /// add nothing. Reach guard:
    /// `additional_phase_count_from_event_context_amount_pushes_n_phases`.
    #[test]
    fn obeka_upkeeps_on_an_opponents_turn_add_nothing() {
        let mut state = GameState {
            active_player: PlayerId(1),
            phase: Phase::CombatDamage,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            Phase::Upkeep,
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.extra_phases.is_empty());
    }

    /// World at War's shape: "After the second main phase this turn, there's an
    /// additional combat phase followed by an additional main phase."
    fn world_at_war() -> ResolvedAbility {
        make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::FirstOfTurn(PhaseGroup::PostcombatMain),
            vec![Phase::PostCombatMain],
            PlayerId(0),
        )
    }

    /// Swinging Ship's shape: "After the first combat phase this turn, there's
    /// an additional combat phase."
    fn swinging_ship() -> ResolvedAbility {
        make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::FirstOfTurn(PhaseGroup::Combat),
            vec![],
            PlayerId(0),
        )
    }

    /// Drives `advance_phase` until `stop` is entered, returning every step
    /// that begins a combat phase, a postcombat main phase or the end step.
    fn milestones_until(state: &mut GameState, stop: Phase) -> Vec<Phase> {
        let mut events = Vec::new();
        let mut milestones = Vec::new();
        for _ in 0..64 {
            crate::game::turns::advance_phase(state, &mut events);
            if matches!(
                state.phase,
                Phase::BeginCombat | Phase::PostCombatMain | Phase::End
            ) {
                milestones.push(state.phase);
            }
            if state.phase == stop {
                return milestones;
            }
        }
        panic!("{stop:?} was never entered; milestones {milestones:?}");
    }

    /// CR 500.8 + CR 505.1b: a first-of-turn anchor resolves to the last step
    /// of that phase while it has not ended, and adds nothing once it has
    /// (World at War ruling: "if it's cast later than that, it won't create
    /// any new phases"; Swinging Ship ruling: "If you somehow visit Swinging
    /// Ship after the first combat phase of a turn has ended, it won't have any
    /// effect"). Each empty row differs from a non-empty row only in the phase
    /// or the tally.
    #[test]
    fn first_of_turn_anchor_adds_nothing_once_that_phase_has_ended() {
        let waw = world_at_war();
        let ship = swinging_ship();
        let after_second_main = vec![
            ep(
                Phase::PostCombatMain,
                TurnSegment::Phase(PhaseGroup::PostcombatMain),
            ),
            ep(
                Phase::PostCombatMain,
                TurnSegment::Phase(PhaseGroup::Combat),
            ),
        ];
        let after_first_combat = vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))];
        let rows: [(&ResolvedAbility, Phase, &[Phase], Vec<ExtraPhase>); 8] = [
            (&waw, Phase::PreCombatMain, &[], after_second_main.clone()),
            (
                &waw,
                Phase::PostCombatMain,
                &[Phase::PostCombatMain],
                after_second_main,
            ),
            (
                &waw,
                Phase::PostCombatMain,
                &[Phase::PostCombatMain, Phase::PostCombatMain],
                vec![],
            ),
            (&waw, Phase::End, &[Phase::PostCombatMain], vec![]),
            (&ship, Phase::PreCombatMain, &[], after_first_combat.clone()),
            (
                &ship,
                Phase::DeclareAttackers,
                &[Phase::BeginCombat],
                after_first_combat,
            ),
            (&ship, Phase::PostCombatMain, &[Phase::BeginCombat], vec![]),
            (
                &ship,
                Phase::BeginCombat,
                &[Phase::BeginCombat, Phase::BeginCombat],
                vec![],
            ),
        ];
        for (ability, phase, begun, expected) in rows {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase,
                ..Default::default()
            };
            for &step in begun {
                state.steps_started_this_turn.record(step);
            }
            let mut events = Vec::new();

            resolve(&mut state, ability, &mut events).unwrap();

            assert_eq!(scheduled(&state), expected, "{phase:?} after {begun:?}");
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::AdditionalPhase,
                        ..
                    }
                )),
                "{phase:?} after {begun:?}: the effect still resolves"
            );
        }
    }

    /// CR 505.1a + CR 505.1b: the second main phase is the first postcombat
    /// main phase to occur, even when it is a main phase another effect added
    /// before the natural combat (Relentless Assault resolving in the precombat
    /// main phase). World at War's phases follow that added main phase, in
    /// either creation order, and the natural combat and main phase follow.
    #[test]
    fn second_main_phase_can_be_an_added_main_phase() {
        let relentless_assault = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::this_main_phase(),
            vec![Phase::PostCombatMain],
            PlayerId(0),
        );
        let waw = world_at_war();
        for order in [[&relentless_assault, &waw], [&waw, &relentless_assault]] {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase: Phase::PreCombatMain,
                ..Default::default()
            };
            let mut events = Vec::new();
            for ability in order {
                resolve(&mut state, ability, &mut events).unwrap();
            }

            let milestones = milestones_until(&mut state, Phase::End);

            assert_eq!(
                milestones,
                vec![
                    Phase::BeginCombat,
                    Phase::PostCombatMain,
                    Phase::BeginCombat,
                    Phase::PostCombatMain,
                    Phase::BeginCombat,
                    Phase::PostCombatMain,
                    Phase::End,
                ],
                "added combat, added (second) main, World at War's combat and \
                 main, natural combat and main"
            );
            assert_eq!(state.steps_started_this_turn.count(Phase::BeginCombat), 3);
            assert!(state.extra_phases.is_empty());
            assert!(state.extra_phase_resume.is_empty());
        }
    }

    /// CR 500.8 (ordinal counted as CR 505.1b counts main phases): Swinging
    /// Ship's combat follows the first combat phase of the turn. Visited twice
    /// before that phase ends, it adds two combats, one at a time (ruling).
    /// When a combat added after the precombat main phase runs before the
    /// natural combat (Moraug ruling), that added combat is the first combat
    /// phase, so Swinging Ship's combat follows it and the natural combat comes
    /// after both.
    #[test]
    fn first_combat_phase_is_the_first_combat_to_occur() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        resolve(&mut state, &swinging_ship(), &mut events).unwrap();
        resolve(&mut state, &swinging_ship(), &mut events).unwrap();
        assert_eq!(
            milestones_until(&mut state, Phase::PostCombatMain),
            vec![
                Phase::BeginCombat,
                Phase::BeginCombat,
                Phase::BeginCombat,
                Phase::PostCombatMain,
            ]
        );
        assert_eq!(state.steps_started_this_turn.count(Phase::BeginCombat), 3);
        assert!(state.extra_phases.is_empty());

        let moraug = make_ability(
            TargetFilter::Controller,
            Phase::BeginCombat,
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        resolve(&mut state, &swinging_ship(), &mut events).unwrap();
        resolve(&mut state, &moraug, &mut events).unwrap();
        assert_eq!(
            scheduled(&state),
            vec![
                ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat)),
                ep(Phase::PreCombatMain, TurnSegment::Phase(PhaseGroup::Combat)),
            ]
        );
        assert_eq!(
            milestones_until(&mut state, Phase::PostCombatMain),
            vec![
                Phase::BeginCombat,
                Phase::BeginCombat,
                Phase::BeginCombat,
                Phase::PostCombatMain,
            ]
        );
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());
    }

    /// CR 500.8 + CR 505.1b: World at War ruling: "Multiple World at War
    /// effects are cumulative, as long as they're cast early enough. … Each
    /// subsequent one inserts another combat phase and main phase into the turn
    /// after the original postcombat main phase and before the newest combat
    /// phase."
    #[test]
    fn world_at_war_twice_before_the_second_main_phase_ends_is_cumulative() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        resolve(&mut state, &world_at_war(), &mut events).unwrap();
        resolve(&mut state, &world_at_war(), &mut events).unwrap();
        assert!(
            state
                .extra_phases
                .iter()
                .all(|entry| entry.anchor == Phase::PostCombatMain),
            "{:?}",
            state.extra_phases
        );
        assert_eq!(state.extra_phases.len(), 4);

        assert_eq!(
            milestones_until(&mut state, Phase::End),
            vec![
                Phase::BeginCombat,
                Phase::PostCombatMain,
                Phase::BeginCombat,
                Phase::PostCombatMain,
                Phase::BeginCombat,
                Phase::PostCombatMain,
                Phase::End,
            ],
            "the natural combat and postcombat main phase, then each World at \
             War's combat and main phase"
        );
        assert_eq!(
            state.steps_started_this_turn.count(Phase::PostCombatMain),
            3
        );
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());
    }

    /// CR 500.8 + CR 500.9 + CR 500.10 + CR 500.11 + CR 505.1a: the segment
    /// each `Effect::AdditionalPhase` shape the grammar emits adds. A whole
    /// phase is named by its first step (`Untap`, `BeginCombat`) or is a main
    /// phase; an upkeep or end step added after a step joins the phase in
    /// progress; an upkeep added after a phase sits in a phase created to hold
    /// only it. Follow-up phases are pushed before the primary one.
    #[test]
    fn each_emitted_shape_adds_its_segment() {
        let mains = || ExtraPhaseAnchor::ThisPhase {
            named: Some(vec![PhaseGroup::PrecombatMain, PhaseGroup::PostcombatMain]),
        };
        let any_phase = || ExtraPhaseAnchor::ThisPhase { named: None };
        let combat = TurnSegment::Phase(PhaseGroup::Combat);
        let postcombat_main = TurnSegment::Phase(PhaseGroup::PostcombatMain);
        let rows = [
            // Aurelia, the Warleader.
            (
                Phase::BeginCombat,
                any_phase(),
                vec![],
                Phase::DeclareAttackers,
                vec![combat],
            ),
            // Relentless Assault.
            (
                Phase::BeginCombat,
                mains(),
                vec![Phase::PostCombatMain],
                Phase::PreCombatMain,
                vec![postcombat_main, combat],
            ),
            // Full Throttle.
            (
                Phase::BeginCombat,
                mains(),
                vec![],
                Phase::PreCombatMain,
                vec![combat],
            ),
            // All-Out Assault.
            (
                Phase::BeginCombat,
                any_phase(),
                vec![Phase::PostCombatMain],
                Phase::PreCombatMain,
                vec![postcombat_main, combat],
            ),
            // Raphael, Tag Team Tough.
            (
                Phase::BeginCombat,
                ExtraPhaseAnchor::ThisPhase {
                    named: Some(vec![PhaseGroup::Combat]),
                },
                vec![],
                Phase::DeclareAttackers,
                vec![combat],
            ),
            // Swinging Ship.
            (
                Phase::BeginCombat,
                ExtraPhaseAnchor::FirstOfTurn(PhaseGroup::Combat),
                vec![],
                Phase::PreCombatMain,
                vec![combat],
            ),
            // World at War.
            (
                Phase::BeginCombat,
                ExtraPhaseAnchor::FirstOfTurn(PhaseGroup::PostcombatMain),
                vec![Phase::PostCombatMain],
                Phase::PreCombatMain,
                vec![postcombat_main, combat],
            ),
            // Temple of Atropos.
            (
                Phase::Untap,
                any_phase(),
                vec![],
                Phase::PreCombatMain,
                vec![TurnSegment::Phase(PhaseGroup::Beginning)],
            ),
            // Obeka, Splitter of Seconds.
            (
                Phase::Upkeep,
                any_phase(),
                vec![],
                Phase::CombatDamage,
                vec![TurnSegment::CreatedPhase(Phase::Upkeep)],
            ),
            // Paradox Haze.
            (
                Phase::Upkeep,
                ExtraPhaseAnchor::ThisStep,
                vec![],
                Phase::Upkeep,
                vec![TurnSegment::Step(Phase::Upkeep)],
            ),
            // Y'shtola Rhul.
            (
                Phase::End,
                ExtraPhaseAnchor::ThisStep,
                vec![],
                Phase::End,
                vec![TurnSegment::Step(Phase::End)],
            ),
        ];
        for (phase, after, followed_by, resolving, expected) in rows {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase: resolving,
                ..Default::default()
            };
            let ability = make_ability(
                TargetFilter::None,
                phase,
                after.clone(),
                followed_by.clone(),
                PlayerId(0),
            );

            resolve(&mut state, &ability, &mut Vec::new()).unwrap();

            let segments: Vec<TurnSegment> = state
                .extra_phases
                .iter()
                .map(|entry| entry.segment)
                .collect();
            assert_eq!(
                segments, expected,
                "{phase:?} after {after:?} followed by {followed_by:?}"
            );
        }
    }
}
