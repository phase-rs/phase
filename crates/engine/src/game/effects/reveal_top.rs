use crate::types::ability::{
    Effect, EffectError, EffectKind, ParentTargetMissingReason, ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.20a/b: reveal the top cards without changing their zone.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let player_filter = match &ability.effect {
        Effect::RevealTop { player, .. } => player,
        _ => return Err(EffectError::MissingParam("RevealTop count".to_string())),
    };
    state.last_parent_target_missing_reason = None;
    // CR 115.1: only an actual player-target filter reads inherited targets.
    let targeted_players: Vec<_> = ability
        .targets
        .iter()
        .filter_map(|target| match target {
            crate::types::ability::TargetRef::Player(player) => Some(*player),
            crate::types::ability::TargetRef::Object(_) => None,
        })
        .collect();
    if matches!(player_filter, crate::types::ability::TargetFilter::Player)
        && targeted_players.len() > 1
    {
        return resolve_for_players(state, ability, &targeted_players, events);
    }
    let Some(player) = super::resolve_player_for_context_ref(state, ability, player_filter) else {
        super::publish_reveal_result(state, Vec::new());
        state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::Dig);
        return Ok(());
    };
    let missing = state.library_of(player).is_empty();
    resolve_for_players(state, ability, &[player], events)?;
    // Seat-local demonstratives still fail when their single producer is missing.
    if missing {
        state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::Dig);
    }
    Ok(())
}

/// Complete one instruction over its ordered population before publishing its result.
/// CR 101.3 + CR 609.3: an empty library cannot suppress another player's reveal.
pub(super) fn resolve_for_players(
    state: &mut GameState,
    ability: &ResolvedAbility,
    players: &[crate::types::player::PlayerId],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count = match &ability.effect {
        Effect::RevealTop { count, .. } => *count as usize,
        _ => return Err(EffectError::MissingParam("RevealTop count".to_string())),
    };
    state.last_parent_target_missing_reason = None;
    let mut accumulated = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &player in players {
        if state.players.get(player.0 as usize).is_none() {
            continue;
        }
        let library = state.library_of(player);
        if library.is_empty() {
            continue;
        }
        let revealed_ids: Vec<_> = library
            .iter()
            .take(count.min(library.len()))
            .copied()
            .collect();
        // CR 701.20b: the cards stay in the same library and in the same order.
        for &id in &revealed_ids {
            state.revealed_cards.insert(id);
            if seen.insert(id) {
                accumulated.push(id);
            }
        }
        let card_names = revealed_ids
            .iter()
            .filter_map(|id| state.objects.get(id).map(|object| object.name.clone()))
            .collect();
        events.push(GameEvent::CardsRevealed {
            player,
            card_ids: revealed_ids,
            card_names,
        });
    }
    // CR 608.2c: even an empty completed instruction replaces the earlier result.
    // The Vec preserves first-seen order when a format shares one library.
    super::publish_reveal_result(state, accumulated);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
        source_id: ability.source_id,
        subject: None,
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
    fn ordinary_reveal_collection_publishes_once_and_replaces_with_empty() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".into(),
            Zone::Library,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "B".into(),
            Zone::Library,
        );
        let mut ability = make_reveal_top_ability(PlayerId(0), PlayerId(0), 1);
        let before = state.next_tracked_set_id;
        let mut events = Vec::new();
        resolve_for_players(
            &mut state,
            &ability,
            &[PlayerId(0), PlayerId(1), PlayerId(0)],
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.next_tracked_set_id,
            before + 1,
            "no intermediate publication"
        );
        assert_eq!(
            state.last_revealed_ids,
            vec![a, b],
            "stable unique membership"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CardsRevealed { .. }))
                .count(),
            3,
            "per-seat presentations survive deduplication"
        );
        ability.effect = Effect::RevealTop {
            count: 0,
            player: TargetFilter::Controller,
        };
        resolve_for_players(
            &mut state,
            &ability,
            &[PlayerId(0), PlayerId(1)],
            &mut events,
        )
        .unwrap();
        assert_eq!(state.next_tracked_set_id, before + 2);
        assert!(state.last_revealed_ids.is_empty());
        assert!(state.tracked_object_sets[&state.chain_tracked_set_id.unwrap()].is_empty());
        assert_eq!(state.last_parent_target_missing_reason, None);
        resolve_for_players(&mut state, &ability, &[], &mut events).unwrap();
        assert_eq!(
            state.next_tracked_set_id,
            before + 3,
            "empty population still completes"
        );
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
