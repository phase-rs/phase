use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, WaitingFor};

/// CR 701.25a: Surveil N — look at top N, put any number into graveyard, rest on top in any order.
///
/// CR 601.2c + CR 115.1: When the parsed `Effect::Surveil { target }` is a
/// player-target filter (e.g. `TargetFilter::Player` from "Target opponent
/// surveils 2"), the surveiling player is whichever `TargetRef::Player` was
/// chosen during spell announcement. `ResolvedAbility::target_player()`
/// extracts that choice and falls back to `ability.controller` when the
/// target is a context-ref (Controller, SelfRef, etc.) — preserving the
/// historical "controller surveils" behavior for plain "surveil N" /
/// "you surveil" patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (surveil_num, surveil_player): (usize, _) = match &ability.effect {
        Effect::Surveil { count, target } => (
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
        .find(|p| p.id == surveil_player)
        .ok_or(EffectError::PlayerNotFound)?;

    let count = surveil_num.min(player.library.len());
    // CR 701.25c: If a player is instructed to surveil 0, no surveil event occurs.
    if count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    events.push(GameEvent::PlayerPerformedAction {
        player_id: surveil_player,
        action: PlayerActionKind::Surveil,
    });

    let cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    state.waiting_for = WaitingFor::SurveilChoice {
        player: surveil_player,
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

    fn make_surveil_ability(surveil_num: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Surveil {
                count: crate::types::ability::QuantityExpr::Fixed { value: surveil_num },
                target: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_surveil_2_sets_waiting_for_surveil_choice() {
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

        let ability = make_surveil_ability(2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::Surveil,
            } if *player_id == PlayerId(0)
        )));

        match &state.waiting_for {
            WaitingFor::SurveilChoice { player, cards } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 2);
                assert_eq!(*cards, top_2);
            }
            other => panic!("Expected SurveilChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_surveil_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_surveil_ability(2);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
