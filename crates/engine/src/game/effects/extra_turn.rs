use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 500.7: Grant an extra turn to the resolved target player.
/// Extra turns are stored as a LIFO stack — push to end, pop from end.
/// The most recently created extra turn is taken first.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ExtraTurn { target } = &ability.effect else {
        return Err(EffectError::MissingParam(
            "expected ExtraTurn effect".into(),
        ));
    };

    // CR 500.7: Resolve the target to a PlayerId.
    let player = match target {
        TargetFilter::Controller | TargetFilter::SelfRef => ability.controller,
        _ => {
            // Targeted variant: resolve from ability.targets
            if let Some(TargetRef::Player(pid)) = ability.targets.first() {
                *pid
            } else {
                // Fallback to controller if no target resolved
                ability.controller
            }
        }
    };

    // CR 500.7: Push to end of Vec (LIFO — pop from end takes most recent first)
    state.extra_turns.push(player);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExtraTurn,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, SpellContext, TargetRef};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_ability(target: TargetFilter, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::ExtraTurn { target },
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
            cost_paid_object_mana_value: None,
            ability_index: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
        }
    }

    #[test]
    fn extra_turn_pushes_controller_to_stack() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![PlayerId(0)]);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::ExtraTurn,
                ..
            }
        )));
    }

    #[test]
    fn extra_turn_lifo_ordering() {
        let mut state = GameState::default();
        let mut events = Vec::new();

        // Player A takes an extra turn
        let ability_a = make_ability(TargetFilter::Controller, PlayerId(0));
        resolve(&mut state, &ability_a, &mut events).unwrap();

        // Player B takes an extra turn (most recent)
        let ability_b = make_ability(TargetFilter::Controller, PlayerId(1));
        resolve(&mut state, &ability_b, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![PlayerId(0), PlayerId(1)]);

        // CR 500.7: Pop from end → most recent (Player B) first
        assert_eq!(state.extra_turns.pop(), Some(PlayerId(1)));
        assert_eq!(state.extra_turns.pop(), Some(PlayerId(0)));
    }

    #[test]
    fn extra_turn_targeted_player() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let mut ability = make_ability(TargetFilter::Any, PlayerId(0));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![PlayerId(1)]);
    }
}
