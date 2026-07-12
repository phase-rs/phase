use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::proposed_event::ProposedEvent;

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
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
            // CR 121.1 + CR 615.5 + CR 609.7: see draw.rs for rationale —
            // context-ref filters resolve via state slots, not controller.
            super::resolve_player_for_context_ref(state, ability, target),
        ),
        _ => (1, ability.controller),
    };

    let proposed = ProposedEvent::Scry {
        player_id: scry_player,
        count: scry_num as u32,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_scry_after_replacement(state, event, events);
            // CR 614.6 + CR 614.11 + CR 704.3: A scry replacement may substitute
            // scry→draw (handled inside `apply_scry_after_replacement` → calls
            // `apply_draw_after_replacement`); if the substituted draw stashes a
            // mandatory-post-effect continuation, drain it in the same step.
            // Mirrors the pipeline ceremony in
            // `effects::draw::draw_through_replacement` — scry's own propose
            // event variant means we can't use that helper directly, but the
            // CR-mandated drain is identical.
            if state.has_post_replacement_drain() {
                let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
                    state, None, None, None, events,
                );
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

pub(crate) fn apply_scry_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) {
    let (player_id, count) = match event {
        ProposedEvent::Scry {
            player_id, count, ..
        } => (player_id, count),
        ProposedEvent::Draw { .. } => {
            crate::game::effects::draw::apply_draw_after_replacement(state, event, events);
            return;
        }
        _ => return,
    };

    let Some(player) = state.players.iter().find(|p| p.id == player_id) else {
        return;
    };

    let count = (count as usize).min(player.library.len());
    if count == 0 {
        return;
    }

    events.push(GameEvent::PlayerPerformedAction {
        player_id,
        action: PlayerActionKind::Scry,
    });

    let cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    state.waiting_for = WaitingFor::ScryChoice {
        player: player_id,
        cards,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
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

    #[test]
    fn test_scry_keeps_reordered_subset_on_top_and_bottoms_rest() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

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
        // Library top-to-bottom: [obj0, obj1, obj2, obj3, obj4].
        let library: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
        let (obj0, obj1, obj2, obj3, obj4) =
            (library[0], library[1], library[2], library[3], library[4]);

        // Scry 3 over the top three cards.
        let ability = make_scry_ability(3);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let waiting = state.waiting_for.clone();
        assert!(matches!(waiting, WaitingFor::ScryChoice { .. }));

        // CR 701.22a: keep obj2 then obj0 on top (reordered partial subset of the
        // looked-at {obj0, obj1, obj2}); obj1 goes to the bottom in any order.
        let mut events = Vec::new();
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![obj2, obj0],
            },
            &mut events,
        )
        .unwrap();

        let library_after: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
        // Kept-on-top in submitted order, untouched cards beneath, bottomed card last.
        assert_eq!(library_after, vec![obj2, obj0, obj3, obj4, obj1]);
    }

    #[test]
    fn test_scry_rejects_foreign_card_and_duplicate() {
        use crate::game::engine::EngineError;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

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
        let library: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
        let obj0 = library[0];
        let foreign = ObjectId(9999);

        let ability = make_scry_ability(2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let waiting = state.waiting_for.clone();

        // CR 701.22a: a selection containing a card that was not looked at is rejected.
        let mut events = Vec::new();
        let result = handle_resolution_choice(
            &mut state,
            waiting.clone(),
            GameAction::SelectCards {
                cards: vec![obj0, foreign],
            },
            &mut events,
        );
        assert!(matches!(result, Err(EngineError::InvalidAction(_))));

        // CR 701.22a: a selection containing a duplicate is rejected.
        let mut events = Vec::new();
        let result = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![obj0, obj0],
            },
            &mut events,
        );
        assert!(matches!(result, Err(EngineError::InvalidAction(_))));

        // The library must be untouched by the rejected actions.
        let library_after: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
        assert_eq!(library_after, library);
    }

    #[test]
    fn scry_replacement_to_draw_delivers_through_resolver() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Eligeth".to_string(),
            Zone::Battlefield,
        );
        let replacement = crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Scry)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        state
            .objects
            .get_mut(&source_id)
            .expect("replacement source exists")
            .replacement_definitions
            .push(replacement);

        let ability = make_scry_ability(2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].hand.len(), 2);
        assert_eq!(state.players[0].library.len(), 1);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
