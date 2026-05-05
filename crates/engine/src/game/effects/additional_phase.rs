use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{ExtraPhase, GameState};

/// CR 500.8: Add extra phases to the current turn via a LIFO stack.
/// CR 500.10a: Only adds phases to the affected player's own turn.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, phase, after, followed_by) = match &ability.effect {
        Effect::AdditionalPhase {
            target,
            phase,
            after,
            followed_by,
        } => (target, *phase, *after, followed_by),
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

    // CR 500.8: Push follow-up phases before the primary phase so the
    // `advance_phase` LIFO scan consumes the primary phase first.
    for &follow_up in followed_by.iter().rev() {
        state.extra_phases.push(ExtraPhase {
            anchor: after,
            phase: follow_up,
        });
    }
    state.extra_phases.push(ExtraPhase {
        anchor: after,
        phase,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::AdditionalPhase,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, SpellContext, TargetFilter};
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
        ResolvedAbility {
            effect: Effect::AdditionalPhase {
                target,
                phase,
                after,
                followed_by,
            },
            controller,
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
            description: None,
            player_scope: None,
            chosen_x: None,
            cost_paid_object: None,
            ability_index: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
        }
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
            vec![ExtraPhase {
                anchor: Phase::EndCombat,
                phase: Phase::BeginCombat,
            }]
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
                ExtraPhase {
                    anchor: Phase::EndCombat,
                    phase: Phase::PostCombatMain,
                },
                ExtraPhase {
                    anchor: Phase::EndCombat,
                    phase: Phase::BeginCombat,
                },
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

        let begin_combat_after_end = ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
        };
        assert_eq!(
            state.extra_phases,
            vec![begin_combat_after_end, begin_combat_after_end]
        );

        // CR 500.8: Pop from end → most recent first
        assert_eq!(state.extra_phases.pop(), Some(begin_combat_after_end));
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

        assert_eq!(
            state.extra_phases,
            vec![ExtraPhase {
                anchor: Phase::Upkeep,
                phase: Phase::Upkeep,
            }]
        );
    }
}
