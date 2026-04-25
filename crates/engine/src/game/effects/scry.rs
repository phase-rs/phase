use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, WaitingFor};

/// CR 701.22a: Scry N — look at top N, put any number on bottom in any order, rest on top in any order.
///
/// CR 601.2c + CR 115.1: When the parsed `Effect::Scry { target }` is a
/// player-target filter (e.g. `TargetFilter::Player` from "Target player scrys
/// 2"), the scrying player is whichever `TargetRef::Player` was chosen during
/// spell announcement. `ResolvedAbility::target_player()` extracts that choice
/// and falls back to `ability.controller` when the target is a context-ref
/// (Controller, SelfRef, etc.) — preserving the historical "controller scries"
/// behavior for plain "scry N" / "you scry" patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (scry_num, scry_player): (usize, _) = match &ability.effect {
        Effect::Scry { count, target } => (
            resolve_quantity_with_targets(state, count, ability) as usize,
            // CR 121.1 + CR 615.5 + CR 609.7: see draw.rs for rationale —
            // context-ref filters resolve via state slots, not controller.
            super::resolve_player_for_context_ref(state, ability, target),
        ),
        _ => (1, ability.controller),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == scry_player)
        .ok_or(EffectError::PlayerNotFound)?;

    let count = scry_num.min(player.library.len());
    // CR 701.22b: If a player is instructed to scry 0, no scry event occurs.
    if count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    events.push(GameEvent::PlayerPerformedAction {
        player_id: scry_player,
        action: PlayerActionKind::Scry,
    });

    // Collect the top N card IDs for the player to choose from
    let cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    state.waiting_for = WaitingFor::ScryChoice {
        player: scry_player,
        cards,
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

    fn make_scry_ability(scry_num: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Scry {
                count: crate::types::ability::QuantityExpr::Fixed { value: scry_num },
                target: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_scry_2_sets_waiting_for_scry_choice() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_2: Vec<_> = state.players[0]
            .library
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>();

        let ability = make_scry_ability(2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::Scry,
            } if *player_id == PlayerId(0)
        )));

        match &state.waiting_for {
            WaitingFor::ScryChoice { player, cards } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 2);
                assert_eq!(*cards, top_2);
            }
            other => panic!("Expected ScryChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_scry_1_single_card_still_requires_choice() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card 0".to_string(),
            Zone::Library,
        );

        let ability = make_scry_ability(1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ScryChoice { player, cards } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 1);
            }
            other => panic!("Expected ScryChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_scry_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_scry_ability(2);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        // Should NOT set ScryChoice when library is empty
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
