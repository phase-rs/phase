use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
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
    let (count, player_filter) = match &ability.effect {
        Effect::RevealTop { count, player } => (*count as usize, player.clone()),
        _ => return Err(EffectError::MissingParam("RevealTop count".to_string())),
    };

    // CR 115.1: Mirror Draw/Mill/Discard — context-ref filters (Controller,
    // DefendingPlayer, etc.) must consult state slots, not `ability.targets`,
    // so a chained "reveal top of your library" sub-ability does not inherit
    // the parent's Player target and reveal from the wrong library.
    let target_player = super::resolve_player_for_context_ref(state, ability, &player_filter);

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
    use crate::types::ability::{TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_reveal_top_ability(
        controller: PlayerId,
        target_player: PlayerId,
        count: u32,
    ) -> ResolvedAbility {
        // Non-context-ref filter so the explicit `TargetRef::Player` in
        // `ability.targets` legitimately wins (mirrors "target player reveals
        // the top card of their library" cards).
        ResolvedAbility::new(
            Effect::RevealTop {
                player: TargetFilter::Any,
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

    #[test]
    fn reveal_top_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained RevealTop with `player: Controller`
        // must reveal the spell controller's library, not the parent's
        // inherited Player target.
        let mut state = GameState::new_two_player(42);
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Top".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealTop {
                player: TargetFilter::Controller,
                count: 1,
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.revealed_cards.contains(&p0_top),
            "P0's library top should be revealed (Controller filter resolves to caster)"
        );
        assert!(
            !state.revealed_cards.contains(&p1_top),
            "P1's library top must NOT be revealed — inherited parent target must not override Controller filter"
        );
    }
}
