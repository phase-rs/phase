use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 701.17a: Mill N — put the top N cards of a player's library into their graveyard.
/// When `destination` is set to a zone other than Graveyard (e.g., Exile),
/// cards are moved there instead -- building block for "exile the top N cards" patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_cards, destination, target_player) = match &ability.effect {
        Effect::Mill {
            count,
            destination,
            target,
        } => (
            // CR 107.1b: Resolve with full ability context so `QuantityRef::Variable { "X" }`
            // reads the caster-chosen X from the resolving ability.
            resolve_quantity_with_targets(state, count, ability) as usize,
            *destination,
            // CR 701.17a + CR 115.1: Mirror Draw/Scry/Surveil — context-ref
            // target filters (Controller, PostReplacementSourceController,
            // ParentTargetController, etc.) must consult state slots, not
            // `ability.targets`, so a Mill sub-ability chained off a Player-
            // targeted parent does not inherit the parent's chosen player.
            super::resolve_player_for_context_ref(state, ability, target),
        ),
        _ => (1, Zone::Graveyard, ability.controller),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .ok_or(EffectError::PlayerNotFound)?;

    // CR 701.17b: A player can't mill more cards than are in their library;
    // if instructed to, they mill as many as possible.
    let count = num_cards.min(player.library.len());
    let cards_to_mill: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    // Move each card from library to destination zone
    for obj_id in cards_to_mill {
        zones::move_to_zone(state, obj_id, destination, events);
    }

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
    use crate::types::ability::{QuantityExpr, TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_mill_ability(num_cards: u32, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed {
                    value: num_cards as i32,
                },
                target: TargetFilter::Any,
                destination: Zone::Graveyard,
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn mill_3_moves_top_3_from_library_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_3: Vec<_> = state.players[1]
            .library
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>();

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].library.len(), 2);
        assert_eq!(state.players[1].graveyard.len(), 3);
        for id in &top_3 {
            assert!(state.players[1].graveyard.contains(id));
        }
    }

    #[test]
    fn mill_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[1].library.is_empty());

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.players[1].graveyard.is_empty());
    }

    #[test]
    fn mill_with_fewer_cards_than_requested_mills_available() {
        let mut state = GameState::new_two_player(42);
        for i in 0..2 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].library.is_empty());
        assert_eq!(state.players[1].graveyard.len(), 2);
    }
}
