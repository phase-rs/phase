//! CR 103.1 + CR 101.4: Resolver for `Effect::ReverseTurnOrder` — flips the
//! game's turn-order direction (Aeon Engine, Time Distortion, Temple of Atropos).
//!
//! Physical seating is unchanged; only turn progression (CR 103.1), APNAP
//! ordering (CR 101.4), and priority passing (CR 117.3d) reverse, all keyed on
//! `state.turn_direction`. In a two-player game the reversal is a no-op (both
//! directions yield the same opponent); the observable effect is multiplayer.

use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::phase::TurnDirection;

/// CR 103.1: Toggle the game's turn-order direction. `turn_direction` is durable
/// state — it persists across turns until another reverse effect flips it back.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    state.turn_direction = match state.turn_direction {
        TurnDirection::Normal => TurnDirection::Reversed,
        TurnDirection::Reversed => TurnDirection::Normal,
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ReverseTurnOrder,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, Effect, SpellContext};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_ability() -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::ReverseTurnOrder,
            controller: PlayerId(0),
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(1),
            source_incarnation: None,
            source_card_id: None,
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
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            effect_context_object: None,
            amassed_army_object: None,
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
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            dig_found_nothing_for_parent_target: false,
        }
    }

    #[test]
    fn reverse_turn_order_toggles_direction_and_emits_event() {
        let mut state = GameState::default();
        assert_eq!(state.turn_direction, TurnDirection::Normal);
        let ability = make_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.turn_direction, TurnDirection::Reversed);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::ReverseTurnOrder,
                ..
            }
        )));

        // CR 103.1: a second reversal returns to the default direction.
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.turn_direction, TurnDirection::Normal);
    }
}
