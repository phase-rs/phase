use std::collections::HashSet;

use crate::game::life_costs::{can_pay_life_cost, pay_life_as_cost};
use crate::game::quantity::resolve_quantity;
use crate::game::targeting::resolve_effect_player_ref;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, life_payment, player_filter) = match &ability.effect {
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            player,
        } => (count, life_payment, player),
        _ => {
            return Err(EffectError::MissingParam(
                "ChooseDrawnThisTurnPayOrTopdeck".to_string(),
            ));
        }
    };
    let Some(player) = resolve_effect_player_ref(state, ability, player_filter) else {
        return Ok(());
    };
    let count =
        resolve_quantity(state, count, ability.controller, ability.source_id).max(0) as usize;
    let life_payment =
        resolve_quantity(state, life_payment, ability.controller, ability.source_id).max(0) as u32;
    let eligible = eligible_drawn_hand_cards(state, player);
    let count = count.min(eligible.len());
    if count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseDrawnThisTurnPayOrTopdeck,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let min_count = minimum_topdeck_count(state, player, count, life_payment);
    state.waiting_for = WaitingFor::DrawnThisTurnTopdeckChoice {
        player,
        cards: eligible,
        count,
        min_count,
        life_payment,
        source_id: ability.source_id,
    };
    Ok(())
}

pub fn record_drawn_card(state: &mut GameState, player: PlayerId, object_id: ObjectId) {
    state
        .cards_drawn_this_turn
        .entry(player)
        .or_default()
        .push(object_id);
}

pub struct TopdeckChoice<'a> {
    pub player: PlayerId,
    pub eligible: &'a [ObjectId],
    pub count: usize,
    pub min_count: usize,
    pub life_payment: u32,
    pub source_id: ObjectId,
    pub chosen_to_topdeck: &'a [ObjectId],
}

pub fn handle_topdeck_choice(
    state: &mut GameState,
    choice: TopdeckChoice<'_>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if choice.chosen_to_topdeck.len() > choice.count {
        return Err(EffectError::InvalidParam(
            "too many cards selected".to_string(),
        ));
    }
    if choice.chosen_to_topdeck.len() < choice.min_count {
        return Err(EffectError::InvalidParam(
            "not enough cards selected".to_string(),
        ));
    }

    let eligible_set: HashSet<ObjectId> = choice.eligible.iter().copied().collect();
    if choice
        .chosen_to_topdeck
        .iter()
        .any(|object_id| !eligible_set.contains(object_id))
    {
        return Err(EffectError::InvalidParam(
            "selected card was not drawn this turn".to_string(),
        ));
    }

    let payment_count = choice.count.saturating_sub(choice.chosen_to_topdeck.len());
    let total_life = choice.life_payment.saturating_mul(payment_count as u32);
    if !can_pay_life_cost(state, choice.player, total_life) {
        return Err(EffectError::InvalidParam(
            "not enough life to keep selected cards".to_string(),
        ));
    }

    for object_id in choice.chosen_to_topdeck.iter().rev() {
        zones::move_to_library_at_index(state, *object_id, Some(0), events);
    }
    for _ in 0..payment_count {
        if pay_life_as_cost(state, choice.player, choice.life_payment, events).is_unpayable() {
            return Err(EffectError::InvalidParam("life payment failed".to_string()));
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseDrawnThisTurnPayOrTopdeck,
        source_id: choice.source_id,
        subject: None,
    });
    Ok(())
}

fn eligible_drawn_hand_cards(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .cards_drawn_this_turn
        .get(&player)
        .into_iter()
        .flatten()
        .copied()
        .filter(|object_id| {
            state
                .objects
                .get(object_id)
                .is_some_and(|object| object.owner == player && object.zone == Zone::Hand)
        })
        .collect()
}

fn minimum_topdeck_count(
    state: &GameState,
    player: PlayerId,
    count: usize,
    life_payment: u32,
) -> usize {
    if life_payment == 0 {
        return 0;
    }
    let max_payments = (0..=count)
        .take_while(|payments| {
            can_pay_life_cost(state, player, life_payment.saturating_mul(*payments as u32))
        })
        .last()
        .unwrap_or(0);
    count.saturating_sub(max_payments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
    use crate::types::identifiers::CardId;

    fn make_ability() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChooseDrawnThisTurnPayOrTopdeck {
                count: QuantityExpr::Fixed { value: 2 },
                life_payment: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        )
    }

    #[test]
    fn resolve_prompts_for_drawn_hand_cards_only() {
        let mut state = GameState::new_two_player(42);
        let drawn = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Drawn".to_string(),
            Zone::Hand,
        );
        let not_drawn = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Not Drawn".to_string(),
            Zone::Hand,
        );
        let moved = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Moved".to_string(),
            Zone::Graveyard,
        );
        state
            .cards_drawn_this_turn
            .insert(PlayerId(0), vec![drawn, moved]);

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(), &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DrawnThisTurnTopdeckChoice { cards, count, .. } => {
                assert_eq!(cards, &vec![drawn]);
                assert_eq!(*count, 1);
            }
            other => panic!("expected DrawnThisTurnTopdeckChoice, got {other:?}"),
        }
        assert!(state.players[0].hand.contains(&not_drawn));
    }

    #[test]
    fn choice_topdecks_selected_and_pays_for_unselected() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Hand,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Hand,
        );
        let eligible = vec![first, second];
        let mut events = Vec::new();

        handle_topdeck_choice(
            &mut state,
            TopdeckChoice {
                player: PlayerId(0),
                eligible: &eligible,
                count: 2,
                min_count: 0,
                life_payment: 4,
                source_id: ObjectId(99),
                chosen_to_topdeck: &[first],
            },
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].life, 16);
        assert_eq!(state.players[0].library[0], first);
        assert!(state.players[0].hand.contains(&second));
    }

    #[test]
    fn choice_rejects_unpayable_kept_cards() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 3;
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Hand,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        let result = handle_topdeck_choice(
            &mut state,
            TopdeckChoice {
                player: PlayerId(0),
                eligible: &[first, second],
                count: 2,
                min_count: 2,
                life_payment: 4,
                source_id: ObjectId(99),
                chosen_to_topdeck: &[first],
            },
            &mut events,
        );

        assert!(result.is_err());
        assert!(state.players[0].hand.contains(&first));
        assert!(state.players[0].hand.contains(&second));
    }

    #[test]
    fn resolve_uses_owner_for_drawn_hand_cards() {
        let mut state = GameState::new_two_player(42);
        let drawn = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Drawn".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&drawn).unwrap().controller = PlayerId(1);
        state.cards_drawn_this_turn.insert(PlayerId(0), vec![drawn]);

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(), &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DrawnThisTurnTopdeckChoice { cards, .. } => {
                assert_eq!(cards, &vec![drawn]);
            }
            other => panic!("expected DrawnThisTurnTopdeckChoice, got {other:?}"),
        }
    }

    #[test]
    fn resolve_requires_topdecking_cards_when_life_cannot_be_paid() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 3;
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Hand,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Hand,
        );
        state
            .cards_drawn_this_turn
            .insert(PlayerId(0), vec![first, second]);

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(), &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DrawnThisTurnTopdeckChoice {
                count, min_count, ..
            } => {
                assert_eq!(*count, 2);
                assert_eq!(*min_count, 2);
            }
            other => panic!("expected DrawnThisTurnTopdeckChoice, got {other:?}"),
        }
    }
}
