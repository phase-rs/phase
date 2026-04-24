use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.20e: Reveal the top card(s) of a player's library.
///
/// Resolves the `player` target filter (typically `DefendingPlayer` or `Controller`)
/// into a PlayerId, then takes the top `count` cards from that player's library,
/// marks them as revealed, and emits `CardsRevealed`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count = match &ability.effect {
        Effect::RevealTop { count, .. } => *count as usize,
        _ => return Err(EffectError::MissingParam("RevealTop count".to_string())),
    };

    // Resolve target player from the ability's resolved targets
    let target_player = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            _ => None,
        })
        .unwrap_or(ability.controller);

    let library = &state.players[target_player.0 as usize].library;
    if library.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Take the top `count` cards (library[0] = top, per zones.rs convention)
    let count = count.min(library.len());
    let revealed_ids: Vec<_> = library.iter().take(count).copied().collect();

    // CR 701.20b: Revealing a card doesn't cause it to leave the zone it's in.
    for &card_id in &revealed_ids {
        state.revealed_cards.insert(card_id);
    }

    // Store revealed IDs for sub_ability condition/target injection
    state.last_revealed_ids = revealed_ids.clone();

    // Emit event with card names
    let card_names: Vec<String> = revealed_ids
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: target_player,
        card_ids: revealed_ids,
        card_names,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_reveal_top_ability(
        controller: PlayerId,
        target_player: PlayerId,
        count: u32,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealTop {
                player: TargetFilter::DefendingPlayer,
                count,
            },
            vec![TargetRef::Player(target_player)],
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn reveal_top_marks_top_card_as_revealed() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Library,
        );

        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.revealed_cards.contains(&card1));
    }

    #[test]
    fn reveal_top_emits_cards_revealed_event() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Library,
        );

        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let revealed = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed { card_names, .. } => Some(card_names.clone()),
            _ => None,
        });
        assert_eq!(revealed, Some(vec!["Mountain".to_string()]));
    }

    #[test]
    fn reveal_top_empty_library_is_noop() {
        let mut state = GameState::new_two_player(42);
        // Player 1 has no library

        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.revealed_cards.is_empty());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn reveal_top_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Library,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Island".to_string(),
            Zone::Library,
        );
        let _card3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );

        // library = [card1(top), card2, card3(bottom)]
        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Top 2 cards (library[0..2]) should be revealed
        assert!(state.revealed_cards.contains(&card1));
        assert!(state.revealed_cards.contains(&card2));
        assert_eq!(state.revealed_cards.len(), 2);
    }
}
