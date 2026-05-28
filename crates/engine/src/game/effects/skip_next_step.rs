use crate::game::quantity::resolve_quantity;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
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

    let n = resolve_quantity(state, count, ability.controller, ability.source_id).max(0) as u32;
    if n > 0 {
        let idx = player.0 as usize;
        if idx >= state.steps_to_skip.len() {
            state.steps_to_skip.resize_with(idx + 1, Default::default);
        }
        for step in step.constituent_steps() {
            *state.steps_to_skip[idx].entry(*step).or_default() += n;
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SkipNextStep,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, StepSkipTarget};
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn make_ability(step: StepSkipTarget, count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::SkipNextStep {
                target: TargetFilter::Controller,
                step,
                count,
            },
            controller: PlayerId(0),
            original_controller: None,
            scoped_player: None,
            source_id: ObjectId(1),
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
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
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
}
