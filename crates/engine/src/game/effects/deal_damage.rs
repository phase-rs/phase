use std::collections::HashSet;

use crate::game::filter;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    effect_variant_name, DamageAmount, Effect, EffectError, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;

/// Deal damage to each target.
/// Reads amount from `Effect::DealDamage { amount }`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_dmg: u32 = match &ability.effect {
        Effect::DealDamage {
            amount: DamageAmount::Fixed(n),
            ..
        } => *n as u32,
        Effect::DealDamage {
            amount: DamageAmount::Variable(_),
            ..
        } => 0,
        _ => return Err(EffectError::MissingParam("DealDamage amount".to_string())),
    };

    for target in &ability.targets {
        let proposed = ProposedEvent::Damage {
            source_id: ability.source_id,
            target: target.clone(),
            amount: num_dmg,
            is_combat: false,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Damage {
                    target: ref t,
                    amount,
                    ..
                } = event
                {
                    match t {
                        TargetRef::Object(obj_id) => {
                            let obj = state
                                .objects
                                .get_mut(obj_id)
                                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
                            if obj
                                .card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Planeswalker)
                            {
                                // Damage to planeswalker removes loyalty counters
                                let current = obj.loyalty.unwrap_or(0);
                                obj.loyalty = Some(current.saturating_sub(amount));
                            } else {
                                obj.damage_marked += amount;
                            }
                        }
                        TargetRef::Player(player_id) => {
                            let player = state
                                .players
                                .iter_mut()
                                .find(|p| p.id == *player_id)
                                .ok_or(EffectError::PlayerNotFound)?;
                            player.life -= amount as i32;
                        }
                    }
                    events.push(GameEvent::DamageDealt {
                        source_id: ability.source_id,
                        target: t.clone(),
                        amount,
                    });
                }
            }
            ReplacementResult::Prevented => {
                // Damage was prevented, skip
            }
            ReplacementResult::NeedsChoice(player) => {
                let candidate_count = state
                    .pending_replacement
                    .as_ref()
                    .map(|p| p.candidates.len())
                    .unwrap_or(0);
                state.waiting_for = crate::types::game_state::WaitingFor::ReplacementChoice {
                    player,
                    candidate_count,
                };
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Deal damage to all permanents (and optionally players) matching the filter.
/// Reads amount and filter from `Effect::DamageAll { amount, target }`.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_dmg, target_filter): (u32, TargetFilter) = match &ability.effect {
        Effect::DamageAll { amount, target } => {
            let dmg = match amount {
                DamageAmount::Fixed(n) => *n as u32,
                DamageAmount::Variable(_) => 0,
            };
            (dmg, target.clone())
        }
        _ => return Err(EffectError::MissingParam("DamageAll amount".to_string())),
    };

    // Collect matching object IDs
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            filter::matches_target_filter_controlled(
                state,
                **id,
                &target_filter,
                ability.source_id,
                ability.controller,
            )
        })
        .copied()
        .collect();

    for obj_id in matching {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.damage_marked += num_dmg;
        }
        events.push(GameEvent::DamageDealt {
            source_id: ability.source_id,
            target: TargetRef::Object(obj_id),
            amount: num_dmg,
        });
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_ability(num_dmg: u32, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::DealDamage {
                amount: DamageAmount::Fixed(num_dmg as i32),
                target: TargetFilter::Any,
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn deal_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability(3, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].damage_marked, 3);
    }

    #[test]
    fn deal_damage_to_player() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 15);
    }

    #[test]
    fn deal_damage_emits_events() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(2, vec![TargetRef::Player(PlayerId(0))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { amount: 2, .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn damage_all_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: DamageAmount::Fixed(2),
                target: TargetFilter::Typed {
                    card_type: Some(crate::types::ability::TypeFilter::Creature),
                    subtype: None,
                    controller: None,
                    properties: vec![],
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear1].damage_marked, 2);
        assert_eq!(state.objects[&bear2].damage_marked, 2);
    }

    #[test]
    fn damage_to_planeswalker_removes_loyalty() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(5);
        }
        let ability = make_ability(3, vec![TargetRef::Object(pw_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Damage removes loyalty, not damage_marked
        assert_eq!(state.objects[&pw_id].loyalty, Some(2)); // 5 - 3
        assert_eq!(state.objects[&pw_id].damage_marked, 0);
    }

    #[test]
    fn lethal_damage_to_planeswalker_sets_loyalty_zero() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Liliana".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(2);
        }
        let ability = make_ability(5, vec![TargetRef::Object(pw_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Damage exceeds loyalty: clamped to 0 via saturating_sub
        assert_eq!(state.objects[&pw_id].loyalty, Some(0));
    }
}
