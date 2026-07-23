use crate::game::quantity::resolve_quantity;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, SkipScope, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 614.10a: "Skip your next N [step] steps." — increments the per-player
/// step-skip counter for the named step. The turn system consumes the counter
/// only when that step would otherwise occur.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::SkipNextStep {
        target,
        step,
        count,
        scope,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "expected SkipNextStep effect".into(),
        ));
    };

    let player = match target {
        TargetFilter::Controller | TargetFilter::SelfRef => ability.controller,
        _ => {
            if let Some(TargetRef::Player(pid)) = ability.targets.first() {
                *pid
            } else {
                ability.controller
            }
        }
    };

    let idx = player.0 as usize;
    match scope {
        // CR 614.10 + CR 614.10a: turn-scoped skip (False Peace / Empty City
        // Ruse). Each resolution arms one independent skip, so increment the
        // per-player `pending` count rather than overwriting — two skips aimed
        // at the same player before their next turn must skip combat on their
        // next *two* non-skipped turns. It ignores `count` (turn-binding subsumes
        // counting) and does NOT touch `steps_to_skip`. Each pending skip waits
        // past skipped turns and binds to a non-skipped turn in `start_next_turn`.
        SkipScope::AllOfNextTurn => {
            if idx >= state.combat_phase_skip_next_turn.len() {
                state
                    .combat_phase_skip_next_turn
                    .resize_with(idx + 1, Default::default);
            }
            state.combat_phase_skip_next_turn[idx].pending += 1;
        }
        // CR 614.10a: occurrence-scoped skip — increment the per-player step
        // counter; the turn system consumes it when the step would occur.
        SkipScope::NextOccurrence => {
            let n =
                resolve_quantity(state, count, ability.controller, ability.source_id).max(0) as u32;
            if n > 0 {
                if idx >= state.steps_to_skip.len() {
                    state.steps_to_skip.resize_with(idx + 1, Default::default);
                }
                for step in step.constituent_steps() {
                    *state.steps_to_skip[idx].entry(*step).or_default() += n;
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SkipNextStep,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, StepSkipTarget};
    use crate::types::game_state::CombatPhaseSkipState;
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn make_ability(step: StepSkipTarget, count: QuantityExpr) -> ResolvedAbility {
        make_ability_scoped(step, count, SkipScope::NextOccurrence)
    }

    fn make_ability_scoped(
        step: StepSkipTarget,
        count: QuantityExpr,
        scope: SkipScope,
    ) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::SkipNextStep {
                target: TargetFilter::Controller,
                step,
                count,
                scope,
            },
            controller: PlayerId(0),
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

    #[test]
    fn skip_next_step_increments_step_counter() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(
            StepSkipTarget::Step(Phase::Untap),
            QuantityExpr::Fixed { value: 1 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.steps_to_skip[0].get(&Phase::Untap), Some(&1));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::SkipNextStep,
                ..
            }
        )));
    }

    /// CR 500.11: Skipping a phase means all steps within that phase are skipped.
    /// Cards: Stonehorn Dignitary, Blinding Angel, Revenant Patriarch, Moment of
    /// Silence.
    #[test]
    fn skip_combat_phase_expands_to_all_five_combat_steps() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(
            StepSkipTarget::CombatPhase,
            QuantityExpr::Fixed { value: 1 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 500.11: all five combat steps must be in steps_to_skip.
        let skips = &state.steps_to_skip[0];
        assert_eq!(skips.get(&Phase::BeginCombat), Some(&1), "BeginCombat");
        assert_eq!(
            skips.get(&Phase::DeclareAttackers),
            Some(&1),
            "DeclareAttackers"
        );
        assert_eq!(
            skips.get(&Phase::DeclareBlockers),
            Some(&1),
            "DeclareBlockers"
        );
        assert_eq!(skips.get(&Phase::CombatDamage), Some(&1), "CombatDamage");
        assert_eq!(skips.get(&Phase::EndCombat), Some(&1), "EndCombat");
        // Non-combat steps must not be touched.
        assert!(skips.get(&Phase::Untap).is_none(), "Untap must not be set");
        assert!(skips.get(&Phase::Draw).is_none(), "Draw must not be set");
    }

    /// CR 614.10 + CR 614.10a: an `AllOfNextTurn` combat skip arms one pending
    /// turn-scoped skip (no turn bound yet) and must NOT write the per-step
    /// `steps_to_skip` counter (turn-binding subsumes counting).
    #[test]
    fn all_of_next_turn_arms_pending_and_leaves_steps_empty() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability_scoped(
            StepSkipTarget::CombatPhase,
            QuantityExpr::Fixed { value: 1 },
            SkipScope::AllOfNextTurn,
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.combat_phase_skip_next_turn[0],
            CombatPhaseSkipState {
                pending: 1,
                active: false
            },
            "AllOfNextTurn must arm exactly one pending skip and bind no turn yet"
        );
        // steps_to_skip must stay empty — turn-scope does not use the counter.
        assert!(
            state.steps_to_skip.is_empty() || state.steps_to_skip[0].is_empty(),
            "AllOfNextTurn must not write steps_to_skip"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::SkipNextStep,
                ..
            }
        )));
    }

    /// CR 614.10a: two `AllOfNextTurn` skips aimed at the same player before
    /// their next turn are independent — they accumulate to `pending: 2` rather
    /// than collapsing to one (each will bind to a separate non-skipped turn).
    #[test]
    fn stacked_all_of_next_turn_skips_accumulate_pending() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability_scoped(
            StepSkipTarget::CombatPhase,
            QuantityExpr::Fixed { value: 1 },
            SkipScope::AllOfNextTurn,
        );

        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.combat_phase_skip_next_turn[0],
            CombatPhaseSkipState {
                pending: 2,
                active: false
            },
            "two stacked turn-scoped skips must accumulate, not overwrite"
        );
    }
}
