use crate::game::quantity::resolve_quantity;
use crate::game::targeting::extract_player_from_event;
use crate::game::turns::{final_step_of_phase_in_progress, first_phase_of_turn_has_ended};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ExtraPhaseAnchor, ExtraPhaseRecipient, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{ExtraPhase, GameState};
use crate::types::phase::{Phase, PhaseGroup, TurnSegment};

/// CR 500.8 + CR 500.9 + CR 500.10: the step an added phase or step follows.
/// "This step/phase" is the step/phase in which the effect resolves; inside a
/// phase an effect added, "this phase" is that phase, and inside a step an
/// effect added, the phase that step was added to
/// (`turns::final_step_of_phase_in_progress`). `None`:
/// there is no such phase to add after (CR 500.8), because the text names a
/// kind of phase (CR 505.1 "this main phase") that the phase in progress is
/// not, or the first phase of a kind this turn and that phase has already ended
/// (CR 505.1b).
fn anchor_step(after: &ExtraPhaseAnchor, state: &GameState) -> Option<Phase> {
    let current = state.phase;
    match after {
        ExtraPhaseAnchor::Step(step) => Some(*step),
        ExtraPhaseAnchor::ThisStep => Some(current),
        ExtraPhaseAnchor::ThisPhase { named: None } => Some(final_step_of_phase_in_progress(state)),
        ExtraPhaseAnchor::ThisPhase {
            named: Some(groups),
        } => {
            // CR 500.9: a step added to a phase is part of it, so the kind of
            // the phase in progress is read from its end, not from the step
            // the effect resolves in.
            let end = final_step_of_phase_in_progress(state);
            groups.contains(&end.group()).then_some(end)
        }
        ExtraPhaseAnchor::FirstOfTurn(group) => {
            (!first_phase_of_turn_has_ended(state, *group)).then(|| group.last_step())
        }
    }
}

/// CR 500.10a: whether an added step or phase goes to the turn in progress.
/// "There is / are an additional …" names no player, so it does, whoever
/// controls the source (Take the Bait and Full Throttle on an opponent's turn;
/// Shadow of the Second Sun on the enchanted player's turn). A step or phase an
/// effect says "you get" is added only if that turn is its controller's; the
/// engine applies the same own-turn gate to any other player the text names
/// ("that player gets", Paradox Haze; "target player gets"). A named player
/// the resolver cannot identify gets nothing: no trigger event, or no player
/// target (CR 608.2b: the part of the effect that needs the illegal or missing
/// target's information doesn't happen).
fn added_to_turn_in_progress(
    recipient: &ExtraPhaseRecipient,
    ability: &ResolvedAbility,
    state: &GameState,
) -> bool {
    let player = match recipient {
        ExtraPhaseRecipient::NoPlayer => return true,
        ExtraPhaseRecipient::Controller => Some(ability.controller),
        ExtraPhaseRecipient::TriggeringPlayer => state
            .current_trigger_event
            .as_ref()
            .and_then(|event| extract_player_from_event(event, state)),
        // CR 608.2b + CR 608.2c: "that player" names the declared player, which
        // gets nothing once it is illegal or was never announced.
        ExtraPhaseRecipient::TargetedPlayer(filter) if filter.names_one_player() => {
            super::resolve_player_for_context_ref(state, ability, filter)
        }
        ExtraPhaseRecipient::TargetedPlayer(_) => {
            // CR 115.1 + CR 608.2b: the embedded recipient filter assigned
            // this instruction its own player slot at announcement and the
            // chain's initial legality pass retained only its legal target. A
            // recipient pruned by that pass carries removal evidence and cannot
            // be refilled by an earlier instruction; legality is not checked
            // again after earlier instructions run.
            ability.targets.iter().find_map(|target| match target {
                TargetRef::Player(player) => Some(*player),
                TargetRef::Object(_) => None,
            })
        }
    };
    player == Some(state.active_player)
}

/// CR 500.8: Add extra phases to the current turn via a LIFO stack.
/// CR 500.10a: `added_to_turn_in_progress` decides whether the recipient's
/// grant reaches the turn in progress.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (recipient, segment, after, followed_by, count_expr, attacker_restriction) =
        match &ability.effect {
            Effect::AdditionalPhase {
                recipient,
                segment,
                after,
                followed_by,
                count,
                attacker_restriction,
            } => (
                recipient,
                *segment,
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

    if !added_to_turn_in_progress(recipient, ability, state) {
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
    for _ in 0..count {
        for &follow_up in followed_by.iter().rev() {
            let id = state.mint_extra_phase_id();
            state.extra_phases.push(ExtraPhase {
                anchor,
                segment: follow_up,
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
        recipient: ExtraPhaseRecipient,
        segment: TurnSegment,
        after: ExtraPhaseAnchor,
        followed_by: Vec<TurnSegment>,
        controller: PlayerId,
    ) -> ResolvedAbility {
        make_ability_with_count(
            recipient,
            segment,
            after,
            followed_by,
            controller,
            QuantityExpr::Fixed { value: 1 },
        )
    }

    fn make_ability_with_count(
        recipient: ExtraPhaseRecipient,
        segment: TurnSegment,
        after: ExtraPhaseAnchor,
        followed_by: Vec<TurnSegment>,
        controller: PlayerId,
        count: QuantityExpr,
    ) -> ResolvedAbility {
        ResolvedAbility {
            declares_chosen_group: None,
            reads_chosen_group: None,
            declares_return_result: None,
            reads_return_result: None,
            detached_remainder: crate::types::ability::DetachedRemainder::NoProducer,
            effect: Effect::AdditionalPhase {
                recipient,
                segment,
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
            illegal_local_target_slots: Vec::new(),
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
            illegal_targets_disposition: Default::default(),
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
            target_reads: Default::default(),
            sibling_condition: crate::types::ability::SiblingCondition::Dependent,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
            activation_cost_reduction: None,
            activation_record: None,
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![TurnSegment::Phase(PhaseGroup::PostcombatMain)],
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // LIFO: the follow-up main phase is pushed first and the combat on
        // top, so the added combat (the more recent entry) runs first after
        // EndCombat, and the main phase after the added combat ends.
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::Step(Phase::EndCombat),
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability1, &mut events).unwrap();

        // Second effect: another additional combat (most recent → first)
        let ability2 = make_ability(
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::NoPlayer,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::this_main_phase(),
            vec![TurnSegment::Phase(PhaseGroup::PostcombatMain)],
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::TriggeringPlayer,
            TurnSegment::Step(Phase::Upkeep),
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
                source_incarnation: None,
            }),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            ExtraPhaseRecipient::Controller,
            TurnSegment::CreatedPhase(Phase::Upkeep),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Beginning),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Beginning),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
    /// (`TurnSegment::Phase(Beginning)`) anchored to that main phase
    /// (`last_step_of_phase`).
    #[test]
    fn additional_beginning_phase_anchors_to_resolving_main_phase() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PostCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Beginning),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Beginning),
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
    /// additional … phase" wording (`ExtraPhaseRecipient::NoPlayer`). Shadow of the Second
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
            ExtraPhaseRecipient::NoPlayer,
            TurnSegment::Phase(PhaseGroup::Beginning),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Beginning),
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
    /// phase", `ExtraPhaseRecipient::NoPlayer`) resolving on an opponent's turn is added
    /// to that turn (Take the Bait); the same text granted to the controller
    /// ("you get") adds nothing there.
    #[test]
    fn expletive_combat_phase_on_an_opponents_turn_is_added_to_that_turn() {
        for (recipient, expected) in [
            (
                ExtraPhaseRecipient::NoPlayer,
                vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))],
            ),
            (ExtraPhaseRecipient::Controller, vec![]),
        ] {
            let mut state = GameState {
                active_player: PlayerId(1),
                phase: Phase::DeclareBlockers,
                ..Default::default()
            };
            let mut events = Vec::new();
            let ability = make_ability(
                recipient.clone(),
                TurnSegment::Phase(PhaseGroup::Combat),
                ExtraPhaseAnchor::ThisPhase { named: None },
                vec![],
                PlayerId(0),
            );
            resolve(&mut state, &ability, &mut events).unwrap();
            assert_eq!(scheduled(&state), expected, "recipient {recipient:?}");
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
                ExtraPhaseRecipient::NoPlayer,
                TurnSegment::Phase(PhaseGroup::Combat),
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
    /// tracked set and stamp it onto the scheduled combat phase's ExtraPhase.
    #[test]
    fn restricted_combat_concretizes_parent_target_to_tracked_set() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::PreCombatMain,
            ..Default::default()
        };
        let mut events = Vec::new();

        let mut ability = make_ability(
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::this_main_phase(),
            vec![],
            PlayerId(0),
        );
        // Stamp the restriction + chosen targets exactly as the parser fold and
        // `resolve_ability_chain` propagation would produce them.
        ability.effect = Effect::AdditionalPhase {
            recipient: ExtraPhaseRecipient::Controller,
            segment: TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Step(Phase::Upkeep),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::this_main_phase(),
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );
        let relentless_assault = make_ability(
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::this_main_phase(),
            vec![TurnSegment::Phase(PhaseGroup::PostcombatMain)],
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::CreatedPhase(Phase::Upkeep),
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );
        resolve(&mut state, &obeka, &mut events).unwrap();
        state.phase = Phase::EndCombat;
        let combat = make_ability(
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::CreatedPhase(Phase::Upkeep),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::CreatedPhase(Phase::Upkeep),
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.extra_phases.is_empty());
    }

    /// Test helper: the anchor "after this phase, there is an additional draw
    /// step" (Untap, Upkeep, Draw's draw mode) resolves to in the step
    /// `resolving`, with `units` the added units the step is inside, outermost
    /// first.
    fn this_phase_anchor_inside(units: &[(Phase, TurnSegment)], resolving: Phase) -> Phase {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: resolving,
            extra_phase_resume: units
                .iter()
                .map(|&(anchor, segment)| InsertedPhaseResume {
                    anchor,
                    segment,
                    entry: ExtraPhaseId::default(),
                })
                .collect(),
            ..Default::default()
        };
        let ability = make_ability(
            ExtraPhaseRecipient::NoPlayer,
            TurnSegment::CreatedPhase(Phase::Draw),
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        match state.extra_phases.as_slice() {
            [entry] => entry.anchor,
            other => panic!("expected one scheduled entry, got {other:?}"),
        }
    }

    /// CR 500.10 + CR 500.11: a phase created to hold only one step ends when
    /// that step ends, because its other steps are skipped, so "after this
    /// phase" resolving inside it anchors at that step (Untap, Upkeep, Draw
    /// cast inside one of Obeka's upkeep-only phases), not at the last step of
    /// the phase that normally holds it (draw, cleanup).
    #[test]
    fn this_phase_inside_a_created_phase_anchors_at_its_step() {
        assert_eq!(
            this_phase_anchor_inside(
                &[(Phase::EndCombat, TurnSegment::CreatedPhase(Phase::Upkeep))],
                Phase::Upkeep,
            ),
            Phase::Upkeep
        );
        assert_eq!(
            this_phase_anchor_inside(
                &[(Phase::PostCombatMain, TurnSegment::CreatedPhase(Phase::End))],
                Phase::End,
            ),
            Phase::End
        );
    }

    /// CR 500.9: a step added to a phase is part of that phase, so "after this
    /// phase" resolving in the added step anchors at the enclosing phase's last
    /// step: the turn's own beginning phase ends with its draw step, a created
    /// upkeep-only phase with its upkeep, and the combat phase upkeep steps
    /// were added to (one after beginning of combat, The Ninth Doctor, and one
    /// after that one) with end of combat.
    #[test]
    fn this_phase_inside_an_added_step_anchors_at_the_enclosing_phase() {
        assert_eq!(
            this_phase_anchor_inside(
                &[(Phase::Upkeep, TurnSegment::Step(Phase::Upkeep))],
                Phase::Upkeep,
            ),
            Phase::Draw
        );
        assert_eq!(
            this_phase_anchor_inside(
                &[
                    (Phase::EndCombat, TurnSegment::CreatedPhase(Phase::Upkeep)),
                    (Phase::Upkeep, TurnSegment::Step(Phase::Upkeep)),
                ],
                Phase::Upkeep,
            ),
            Phase::Upkeep
        );
        assert_eq!(
            this_phase_anchor_inside(
                &[(Phase::BeginCombat, TurnSegment::Step(Phase::Upkeep))],
                Phase::Upkeep,
            ),
            Phase::EndCombat
        );
        assert_eq!(
            this_phase_anchor_inside(
                &[
                    (Phase::BeginCombat, TurnSegment::Step(Phase::Upkeep)),
                    (Phase::Upkeep, TurnSegment::Step(Phase::Upkeep)),
                ],
                Phase::Upkeep,
            ),
            Phase::EndCombat
        );
    }

    /// CR 500.9 + CR 500.8: an upkeep step added after beginning of combat is
    /// part of that combat phase, so "after this combat phase" (Raphael, Save
    /// Point) resolving in it anchors at end of combat, and "after this main
    /// phase" adds nothing there (CR 505.1).
    #[test]
    fn named_phase_inside_an_added_step_reads_the_phase_it_was_added_to() {
        for (after, expected) in [
            (
                ExtraPhaseAnchor::ThisPhase {
                    named: Some(vec![PhaseGroup::Combat]),
                },
                vec![ep(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Combat))],
            ),
            (ExtraPhaseAnchor::this_main_phase(), vec![]),
        ] {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase: Phase::Upkeep,
                extra_phase_resume: vec![InsertedPhaseResume {
                    anchor: Phase::BeginCombat,
                    segment: TurnSegment::Step(Phase::Upkeep),
                    entry: ExtraPhaseId::default(),
                }],
                ..Default::default()
            };
            let ability = make_ability(
                ExtraPhaseRecipient::NoPlayer,
                TurnSegment::Phase(PhaseGroup::Combat),
                after.clone(),
                vec![],
                PlayerId(0),
            );
            resolve(&mut state, &ability, &mut Vec::new()).unwrap();
            assert_eq!(scheduled(&state), expected, "{after:?}");
        }
    }

    /// CR 500.8: inside a whole added phase, "after this phase" anchors at that
    /// phase's last step. The innermost added phase is the one in progress: a
    /// combat added after a created upkeep-only phase has ended runs inside
    /// that phase's record, and ends with end of combat.
    #[test]
    fn this_phase_inside_an_added_phase_anchors_at_its_last_step() {
        assert_eq!(
            this_phase_anchor_inside(
                &[(Phase::EndCombat, TurnSegment::Phase(PhaseGroup::Beginning))],
                Phase::Upkeep,
            ),
            Phase::Draw
        );
        assert_eq!(
            this_phase_anchor_inside(
                &[
                    (Phase::EndCombat, TurnSegment::CreatedPhase(Phase::Upkeep)),
                    (Phase::Upkeep, TurnSegment::Phase(PhaseGroup::Combat)),
                ],
                Phase::DeclareAttackers,
            ),
            Phase::EndCombat
        );
    }

    /// CR 500.8 + CR 500.10: a phase added "after this phase" from inside one of
    /// Obeka's upkeep-only phases runs directly after that phase, before
    /// Obeka's other upkeep-only phase, and the turn then continues to the
    /// postcombat main phase. Reach guard: the effect resolves with the turn
    /// machine inside the created phase.
    #[test]
    fn this_phase_inside_a_created_upkeep_phase_runs_directly_after_it() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::CombatDamage,
            ..Default::default()
        };
        let mut events = Vec::new();
        let obeka = make_ability_with_count(
            ExtraPhaseRecipient::Controller,
            TurnSegment::CreatedPhase(Phase::Upkeep),
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );
        resolve(&mut state, &obeka, &mut events).unwrap();
        crate::game::turns::advance_phase(&mut state, &mut events);
        crate::game::turns::advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::Upkeep);
        assert_eq!(
            state
                .extra_phase_resume
                .iter()
                .map(|unit| (unit.anchor, unit.segment))
                .collect::<Vec<_>>(),
            vec![(Phase::EndCombat, TurnSegment::CreatedPhase(Phase::Upkeep))]
        );

        let draw_mode = make_ability(
            ExtraPhaseRecipient::NoPlayer,
            TurnSegment::CreatedPhase(Phase::Draw),
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );
        resolve(&mut state, &draw_mode, &mut events).unwrap();

        let mut sequence = Vec::new();
        for _ in 0..8 {
            crate::game::turns::advance_phase(&mut state, &mut events);
            sequence.push(state.phase);
            if state.phase == Phase::PostCombatMain {
                break;
            }
        }
        assert_eq!(
            sequence,
            vec![Phase::Draw, Phase::Upkeep, Phase::PostCombatMain]
        );
        assert!(state.extra_phases.is_empty());
        assert!(state.extra_phase_resume.is_empty());
    }

    /// CR 500.9 + CR 500.8: inside one of Obeka's upkeep-only phases, an upkeep
    /// step added after this step (Paradox Haze's shape) is part of that phase,
    /// so a draw phase added after the phase, even when created later, runs
    /// after the added step, and Obeka's other upkeep-only phase after both.
    /// Each entered step is paired with the innermost added unit it runs in.
    #[test]
    fn an_added_step_runs_before_a_phase_added_after_its_phase() {
        let mut state = GameState {
            active_player: PlayerId(0),
            phase: Phase::CombatDamage,
            ..Default::default()
        };
        let mut events = Vec::new();
        let obeka = make_ability_with_count(
            ExtraPhaseRecipient::Controller,
            TurnSegment::CreatedPhase(Phase::Upkeep),
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
            QuantityExpr::Fixed { value: 2 },
        );
        resolve(&mut state, &obeka, &mut events).unwrap();
        crate::game::turns::advance_phase(&mut state, &mut events);
        crate::game::turns::advance_phase(&mut state, &mut events);
        assert_eq!(state.phase, Phase::Upkeep);
        assert_eq!(
            state.extra_phase_resume.len(),
            1,
            "inside the created upkeep"
        );

        for (segment, after) in [
            (TurnSegment::Step(Phase::Upkeep), ExtraPhaseAnchor::ThisStep),
            (
                TurnSegment::CreatedPhase(Phase::Draw),
                ExtraPhaseAnchor::ThisPhase { named: None },
            ),
        ] {
            let ability = make_ability(
                ExtraPhaseRecipient::Controller,
                segment,
                after,
                vec![],
                PlayerId(0),
            );
            resolve(&mut state, &ability, &mut events).unwrap();
        }

        let mut sequence = Vec::new();
        for _ in 0..8 {
            crate::game::turns::advance_phase(&mut state, &mut events);
            sequence.push((
                state.phase,
                state.extra_phase_resume.last().map(|unit| unit.segment),
            ));
            if state.phase == Phase::PostCombatMain {
                break;
            }
        }
        assert_eq!(
            sequence,
            vec![
                (Phase::Upkeep, Some(TurnSegment::Step(Phase::Upkeep))),
                (Phase::Draw, Some(TurnSegment::CreatedPhase(Phase::Draw))),
                (
                    Phase::Upkeep,
                    Some(TurnSegment::CreatedPhase(Phase::Upkeep))
                ),
                (Phase::PostCombatMain, None),
            ]
        );
        assert!(state.extra_phases.is_empty());
    }

    /// World at War's shape: "After the second main phase this turn, there's an
    /// additional combat phase followed by an additional main phase."
    fn world_at_war() -> ResolvedAbility {
        make_ability(
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::FirstOfTurn(PhaseGroup::PostcombatMain),
            vec![TurnSegment::Phase(PhaseGroup::PostcombatMain)],
            PlayerId(0),
        )
    }

    /// Swinging Ship's shape: "After the first combat phase this turn, there's
    /// an additional combat phase."
    fn swinging_ship() -> ResolvedAbility {
        make_ability(
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::this_main_phase(),
            vec![TurnSegment::Phase(PhaseGroup::PostcombatMain)],
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
            ExtraPhaseRecipient::Controller,
            TurnSegment::Phase(PhaseGroup::Combat),
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

    /// CR 500.8 + CR 500.9 + CR 500.10: each segment shape the grammar emits
    /// is scheduled as written, after the anchor the resolving step or phase
    /// gives, with follow-up phases pushed before the primary one so the
    /// primary runs first.
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
                combat,
                any_phase(),
                vec![],
                Phase::DeclareAttackers,
                vec![combat],
            ),
            // Relentless Assault.
            (
                combat,
                mains(),
                vec![postcombat_main],
                Phase::PreCombatMain,
                vec![postcombat_main, combat],
            ),
            // Full Throttle.
            (combat, mains(), vec![], Phase::PreCombatMain, vec![combat]),
            // All-Out Assault.
            (
                combat,
                any_phase(),
                vec![postcombat_main],
                Phase::PreCombatMain,
                vec![postcombat_main, combat],
            ),
            // Raphael, Tag Team Tough.
            (
                combat,
                ExtraPhaseAnchor::ThisPhase {
                    named: Some(vec![PhaseGroup::Combat]),
                },
                vec![],
                Phase::DeclareAttackers,
                vec![combat],
            ),
            // Swinging Ship.
            (
                combat,
                ExtraPhaseAnchor::FirstOfTurn(PhaseGroup::Combat),
                vec![],
                Phase::PreCombatMain,
                vec![combat],
            ),
            // World at War.
            (
                combat,
                ExtraPhaseAnchor::FirstOfTurn(PhaseGroup::PostcombatMain),
                vec![postcombat_main],
                Phase::PreCombatMain,
                vec![postcombat_main, combat],
            ),
            // Temple of Atropos.
            (
                TurnSegment::Phase(PhaseGroup::Beginning),
                any_phase(),
                vec![],
                Phase::PreCombatMain,
                vec![TurnSegment::Phase(PhaseGroup::Beginning)],
            ),
            // Obeka, Splitter of Seconds.
            (
                TurnSegment::CreatedPhase(Phase::Upkeep),
                any_phase(),
                vec![],
                Phase::CombatDamage,
                vec![TurnSegment::CreatedPhase(Phase::Upkeep)],
            ),
            // Paradox Haze.
            (
                TurnSegment::Step(Phase::Upkeep),
                ExtraPhaseAnchor::ThisStep,
                vec![],
                Phase::Upkeep,
                vec![TurnSegment::Step(Phase::Upkeep)],
            ),
            // Y'shtola Rhul.
            (
                TurnSegment::Step(Phase::End),
                ExtraPhaseAnchor::ThisStep,
                vec![],
                Phase::End,
                vec![TurnSegment::Step(Phase::End)],
            ),
        ];
        for (segment, after, followed_by, resolving, expected) in rows {
            let mut state = GameState {
                active_player: PlayerId(0),
                phase: resolving,
                ..Default::default()
            };
            let ability = make_ability(
                ExtraPhaseRecipient::NoPlayer,
                segment,
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
                "{segment:?} after {after:?} followed by {followed_by:?}"
            );
        }
    }

    /// Test helper: resolve a combat phase added after this phase for
    /// `recipient`, controlled by player 0, on `active`'s turn, with the given
    /// trigger event and chosen targets; returns whether a phase was added.
    /// Reach guard for every caller: `resolve` published `EffectResolved`, which
    /// it does on every return. Each caller's paired positive row on the same
    /// board shows the anchor resolves, so a row that adds nothing was stopped
    /// at the gate.
    fn adds_a_phase(
        recipient: ExtraPhaseRecipient,
        active: PlayerId,
        trigger_event: Option<GameEvent>,
        targets: Vec<TargetRef>,
    ) -> bool {
        let mut state = GameState {
            active_player: active,
            phase: Phase::PreCombatMain,
            current_trigger_event: trigger_event,
            ..Default::default()
        };
        let mut ability = make_ability(
            recipient,
            TurnSegment::Phase(PhaseGroup::Combat),
            ExtraPhaseAnchor::ThisPhase { named: None },
            vec![],
            PlayerId(0),
        );
        ability.targets = targets;
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::AdditionalPhase,
                    ..
                }
            )),
            "reach guard: the resolver published its resolution"
        );
        !state.extra_phases.is_empty()
    }

    /// CR 500.10a: each recipient kind gates on the player it names, on that
    /// player's turn and on another's. The controller is player 0 throughout,
    /// and the triggering and targeted players are player 1, so each kind's
    /// rows disagree with the controller's.
    #[test]
    fn each_recipient_kind_gates_on_the_player_it_names() {
        let drew = |player_id| {
            Some(GameEvent::CardsDrawn {
                player_id,
                count: 1,
            })
        };
        let targeted = ExtraPhaseRecipient::TargetedPlayer(TargetFilter::Player);
        let rows = [
            (
                ExtraPhaseRecipient::NoPlayer,
                None,
                vec![],
                PlayerId(0),
                true,
            ),
            (
                ExtraPhaseRecipient::NoPlayer,
                None,
                vec![],
                PlayerId(1),
                true,
            ),
            (
                ExtraPhaseRecipient::Controller,
                None,
                vec![],
                PlayerId(0),
                true,
            ),
            (
                ExtraPhaseRecipient::Controller,
                None,
                vec![],
                PlayerId(1),
                false,
            ),
            (
                ExtraPhaseRecipient::TriggeringPlayer,
                drew(PlayerId(1)),
                vec![],
                PlayerId(1),
                true,
            ),
            (
                ExtraPhaseRecipient::TriggeringPlayer,
                drew(PlayerId(1)),
                vec![],
                PlayerId(0),
                false,
            ),
            (
                targeted.clone(),
                None,
                vec![TargetRef::Player(PlayerId(1))],
                PlayerId(1),
                true,
            ),
            (
                targeted,
                None,
                vec![TargetRef::Player(PlayerId(1))],
                PlayerId(0),
                false,
            ),
        ];
        for (recipient, event, targets, active, expected) in rows {
            assert_eq!(
                adds_a_phase(recipient.clone(), active, event, targets),
                expected,
                "{recipient:?} on {active:?}'s turn"
            );
        }
    }

    /// CR 115.1 + CR 608.2b: "target player gets" with no player target adds
    /// nothing, even on its controller's turn: the part of the effect that
    /// needs the target doesn't happen, and the controller is not a stand-in.
    /// Paired positive: the same ability targeting its controller adds the
    /// phase. Hostile: an object target is not a player target.
    #[test]
    fn targeted_player_recipient_without_a_player_target_adds_nothing() {
        let targeted = || ExtraPhaseRecipient::TargetedPlayer(TargetFilter::Player);
        assert!(
            adds_a_phase(
                targeted(),
                PlayerId(0),
                None,
                vec![TargetRef::Player(PlayerId(0))]
            ),
            "paired positive: the targeted controller on their own turn"
        );
        assert!(!adds_a_phase(targeted(), PlayerId(0), None, vec![]));
        assert!(!adds_a_phase(
            targeted(),
            PlayerId(0),
            None,
            vec![TargetRef::Object(ObjectId(5))]
        ));
    }

    /// CR 500.10a: "that player gets" with no trigger event names no player
    /// the resolver can identify, so it adds nothing, even on its controller's
    /// turn. Paired positive: an event naming the controller adds the phase.
    #[test]
    fn triggering_player_recipient_without_an_event_adds_nothing() {
        assert!(
            adds_a_phase(
                ExtraPhaseRecipient::TriggeringPlayer,
                PlayerId(0),
                Some(GameEvent::CardsDrawn {
                    player_id: PlayerId(0),
                    count: 1,
                }),
                vec![]
            ),
            "paired positive: the event names the controller on their own turn"
        );
        assert!(!adds_a_phase(
            ExtraPhaseRecipient::TriggeringPlayer,
            PlayerId(0),
            None,
            vec![]
        ));
    }
}
