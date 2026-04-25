use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::phase::Phase;

/// CR 500.8: Add extra phases to the current turn via a LIFO stack.
/// CR 500.10a: Only adds phases to the controller's own turn.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, with_main_phase) = match &ability.effect {
        Effect::AdditionalCombatPhase {
            target,
            with_main_phase,
        } => (target, *with_main_phase),
        _ => {
            return Err(EffectError::MissingParam(
                "expected AdditionalCombatPhase".into(),
            ))
        }
    };

    // CR 500.8: Resolve the target to a PlayerId.
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

    // CR 500.10a: "If an effect that says 'you get' an additional step or phase
    // would add a step or phase to a turn other than its controller's, no steps
    // or phases are added."
    if player != state.active_player {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AdditionalCombatPhase,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 500.8: Push phases in LIFO order — most recently created occurs first.
    // For "additional combat phase followed by additional main phase":
    // push PostCombatMain first, then BeginCombat, so combat executes first.
    // Note: If the extra combat is skipped (no attackers), the no-attackers path in
    // turns.rs sets phase = PostCombatMain directly. The stacked PostCombatMain
    // still fires as an additional main phase — this is arguably correct per CR 505.1a.
    if with_main_phase {
        state.extra_phases.push(Phase::PostCombatMain);
    }
    state.extra_phases.push(Phase::BeginCombat);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::AdditionalCombatPhase,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, SpellContext, TargetFilter};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_ability(
        target: TargetFilter,
        with_main_phase: bool,
        controller: PlayerId,
    ) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::AdditionalCombatPhase {
                target,
                with_main_phase,
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
            ability_index: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
        }
    }

    #[test]
    fn additional_combat_pushes_begin_combat() {
        let mut state = GameState {
            active_player: PlayerId(0),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, false, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_phases, vec![Phase::BeginCombat]);
    }

    #[test]
    fn additional_combat_with_main_pushes_both() {
        let mut state = GameState {
            active_player: PlayerId(0),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, true, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();

        // LIFO: PostCombatMain pushed first, BeginCombat on top → combat executes first
        assert_eq!(
            state.extra_phases,
            vec![Phase::PostCombatMain, Phase::BeginCombat]
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
        let ability1 = make_ability(TargetFilter::Controller, false, PlayerId(0));
        resolve(&mut state, &ability1, &mut events).unwrap();

        // Second effect: another additional combat (most recent → first)
        let ability2 = make_ability(TargetFilter::Controller, false, PlayerId(0));
        resolve(&mut state, &ability2, &mut events).unwrap();

        assert_eq!(
            state.extra_phases,
            vec![Phase::BeginCombat, Phase::BeginCombat]
        );

        // CR 500.8: Pop from end → most recent first
        assert_eq!(state.extra_phases.pop(), Some(Phase::BeginCombat));
        assert_eq!(state.extra_phases.pop(), Some(Phase::BeginCombat));
    }

    #[test]
    fn cr_500_10a_opponent_turn_no_phases_added() {
        // Active player is 1, but controller is 0
        let mut state = GameState {
            active_player: PlayerId(1),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, false, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 500.10a: No phases added on opponent's turn
        assert!(state.extra_phases.is_empty());
    }
}
