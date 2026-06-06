use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 606.3 + CR 606.1: Grant the resolved target player extra
/// loyalty-ability activations for the remainder of this turn. Each grant
/// raises the per-planeswalker CR 606.3 cap by `amount` for every
/// planeswalker the player controls.
///
/// The Chain Veil's printed class: "{4}, {T}: You may activate each
/// planeswalker's loyalty ability an additional time this turn." A future card
/// reading "an additional two times" or "an additional N times for each
/// [type]" lands here unchanged via `QuantityExpr::resolve`.
///
/// CR 514.2: The bonus is bound to the current turn; the counter is cleared
/// in `start_next_turn` alongside other per-turn history.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, target) = match &ability.effect {
        Effect::GrantExtraLoyaltyActivations { amount, target } => (amount, target),
        _ => {
            return Err(EffectError::MissingParam(
                "expected GrantExtraLoyaltyActivations effect".into(),
            ));
        }
    };

    // CR 109.5 / CR 113.6: "you" in an effect text resolves to the ability's
    // controller. Cards with `target: Controller` (the printed default) read
    // their grantee directly off `ability.controller`; targeted variants
    // resolve from the first player target.
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

    let amount = crate::game::quantity::resolve_quantity(
        state,
        amount,
        ability.controller,
        ability.source_id,
    );
    // CR 107.1b: A negative or zero count is a no-op (no cards in the printed
    // class ever go below 1, but the resolver must clamp defensively).
    let amount = u32::try_from(amount).unwrap_or(0);
    if amount > 0 {
        *state
            .extra_loyalty_activations_this_turn
            .entry(player)
            .or_insert(0) += amount;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GrantExtraLoyaltyActivations,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, SubAbilityLink};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_ability(amount: QuantityExpr, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::GrantExtraLoyaltyActivations {
                amount,
                target: TargetFilter::Controller,
            },
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(1),
            targets: vec![],
            kind: AbilityKind::Activated,
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
            sub_link: SubAbilityLink::ContinuationStep,
        }
    }

    #[test]
    fn grants_one_extra_loyalty_activation_to_controller() {
        let mut state = GameState::new_two_player(0);
        let ability = make_ability(QuantityExpr::Fixed { value: 1 }, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state
                .extra_loyalty_activations_this_turn
                .get(&PlayerId(0))
                .copied(),
            Some(1)
        );
    }

    /// CR 606.3: Three activations of The Chain Veil grant a cumulative +3 per
    /// planeswalker for the rest of the turn.
    #[test]
    fn stacks_across_multiple_activations() {
        let mut state = GameState::new_two_player(0);
        let ability = make_ability(QuantityExpr::Fixed { value: 1 }, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state
                .extra_loyalty_activations_this_turn
                .get(&PlayerId(0))
                .copied(),
            Some(3),
        );
    }

    /// CR 107.1b: A zero amount is a no-op — the map stays empty (so the
    /// cap-raise predicate short-circuits cleanly).
    #[test]
    fn zero_amount_is_no_op() {
        let mut state = GameState::new_two_player(0);
        let ability = make_ability(QuantityExpr::Fixed { value: 0 }, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.extra_loyalty_activations_this_turn.is_empty());
    }
}
