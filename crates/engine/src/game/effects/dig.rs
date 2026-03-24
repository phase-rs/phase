use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 701.16a: Dig — look at top N cards, select some to keep, rest go elsewhere.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (dig_num, keep_num, is_up_to, filter, kept_dest, rest_dest) = match &ability.effect {
        Effect::Dig {
            count,
            keep_count,
            up_to,
            filter,
            destination,
            rest_destination,
        } => (
            *count as usize,
            keep_count.unwrap_or(1) as usize,
            *up_to,
            filter.clone(),
            *destination,
            *rest_destination,
        ),
        _ => (1, 1, false, TargetFilter::Any, None, None),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    // CR 401.5: If a library has fewer cards than required, use as many as available.
    let count = dig_num.min(player.library.len());
    if count == 0 {
        return Ok(());
    }

    let cards: Vec<_> = player.library[..count].to_vec();
    let keep_count = keep_num.min(cards.len());

    // Pre-compute selectable cards by evaluating the filter against each card.
    let selectable_cards = if matches!(filter, TargetFilter::Any) {
        cards.clone()
    } else {
        cards
            .iter()
            .filter(|&&card_id| {
                crate::game::filter::matches_target_filter(
                    state,
                    card_id,
                    &filter,
                    ability.source_id,
                )
            })
            .copied()
            .collect()
    };

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        selectable_cards,
        cards,
        keep_count,
        up_to: is_up_to,
        kept_destination: kept_dest,
        rest_destination: rest_dest,
        source_id: Some(ability.source_id),
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_dig_ability(dig_num: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Dig {
                count: dig_num,
                destination: None,
                keep_count: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_dig_5_keep_1_sets_waiting_for_dig_choice() {
        let mut state = GameState::new_two_player(42);
        for i in 0..7 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_5: Vec<_> = state.players[0].library[..5].to_vec();

        let ability = make_dig_ability(5);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 5);
                assert_eq!(*cards, top_5);
                assert_eq!(*keep_count, 1);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_dig_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_dig_ability(3);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
