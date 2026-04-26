use crate::game::quantity::resolve_quantity;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 614.10: "Skip your next N turns." — increments the per-player `turns_to_skip`
/// counter by the resolved quantity (`count` defaults to 1). The turn system checks
/// this counter during `start_next_turn` and skips the turn if non-zero.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::SkipNextTurn { target, count } = &ability.effect else {
        return Err(EffectError::MissingParam(
            "expected SkipNextTurn effect".into(),
        ));
    };

    // Resolve the target to a PlayerId.
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

    // CR 107.1: resolve `count` in the ability's context; clamp at zero.
    let n = resolve_quantity(state, count, ability.controller, ability.source_id).max(0) as u32;
    if n == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SkipNextTurn,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Ensure the turns_to_skip vector is large enough.
    let idx = player.0 as usize;
    if idx >= state.turns_to_skip.len() {
        state.turns_to_skip.resize(idx + 1, 0);
    }
    state.turns_to_skip[idx] += n;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SkipNextTurn,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, TargetRef};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_ability(target: TargetFilter, controller: PlayerId) -> ResolvedAbility {
        make_ability_with_count(target, controller, QuantityExpr::Fixed { value: 1 })
    }

    fn make_ability_with_count(
        target: TargetFilter,
        controller: PlayerId,
        count: QuantityExpr,
    ) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::SkipNextTurn { target, count },
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
    fn skip_next_turn_increments_counter() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.turns_to_skip[0], 1);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::SkipNextTurn,
                ..
            }
        )));
    }

    #[test]
    fn skip_next_turn_stacks() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.turns_to_skip[0], 2);
    }

    #[test]
    fn skip_next_turn_targeted_player() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let mut ability = make_ability(TargetFilter::Any, PlayerId(0));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.turns_to_skip[1], 1);
    }

    #[test]
    fn skip_next_turn_count_greater_than_one() {
        // CR 614.10: Ral Zarek [-7] "skip their next X turns" — count > 1.
        let mut state = GameState::default();
        let mut events = Vec::new();
        let mut ability = make_ability_with_count(
            TargetFilter::Any,
            PlayerId(0),
            QuantityExpr::Fixed { value: 3 },
        );
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.turns_to_skip[1], 3);
    }

    #[test]
    fn skip_next_turn_zero_count_is_noop() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            PlayerId(0),
            QuantityExpr::Fixed { value: 0 },
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // No side-effect on turns_to_skip (vector may stay empty).
        assert!(state.turns_to_skip.is_empty() || state.turns_to_skip[0] == 0);
    }
}
