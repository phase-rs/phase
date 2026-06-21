use crate::game::zone_pipeline::{self, ZoneMoveRequest};
use crate::game::{quantity, zones};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{CastOfferKind, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.57a: Discover N — exile cards from the top of your library until
/// you exile a nonland card with mana value N or less. Cast it without paying
/// its mana cost or put it into your hand. Put the rest on the bottom of your
/// library in a random order.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (limit, discovering_player) = match &ability.effect {
        Effect::Discover {
            mana_value_limit,
            player,
        } => {
            let limit = quantity::resolve_quantity_with_targets(state, mana_value_limit, ability)
                .max(0) as u32;
            // CR 701.57a: the discovering player exiles from *their* library.
            // Default `TargetFilter::Controller` keeps "you discover"; Zoyowa's
            // Justice redirects to the parent target's owner ("that player
            // discovers X").
            let discovering_player =
                crate::game::effects::resolve_player_for_context_ref(state, ability, player);
            (limit, discovering_player)
        }
        _ => return Err(EffectError::InvalidParam("Expected Discover".to_string())),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == discovering_player)
        .ok_or(EffectError::PlayerNotFound)?;

    // Collect library IDs (top to bottom)
    let library: Vec<ObjectId> = player.library.iter().copied().collect();
    let mut exiled_misses: Vec<ObjectId> = Vec::new();
    let mut hit_card: Option<ObjectId> = None;

    // CR 701.57a: Exile one at a time until hit or library exhausted
    for &card_id in &library {
        // CR 701.57a + CR 614.6: exile the card via the zone-change pipeline so a
        // board-wide `Moved` exile redirect is consulted (none target Exile today
        // — behavior-preserving, future-proof). CR 616.1: a future Exile-targeting
        // redirect could surface an ordering choice mid-loop; park the prompt
        // (mirrors `exile_from_top_until`'s NeedsChoice arm) and return rather than
        // continuing to exile/classify the remaining cards past a parked prompt.
        let result = zone_pipeline::move_object(
            state,
            ZoneMoveRequest::effect(card_id, Zone::Exile, ability.source_id),
            events,
        );
        if let zone_pipeline::ZoneMoveResult::NeedsChoice(player) = result {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }

        // Check if this is a nonland card with MV ≤ limit
        let is_hit = state.objects.get(&card_id).is_some_and(|obj| {
            let is_land = obj.card_types.core_types.contains(&CoreType::Land);
            let mv = obj.mana_cost.mana_value();
            !is_land && mv <= limit
        });

        if is_hit {
            hit_card = Some(card_id);
            break;
        } else {
            exiled_misses.push(card_id);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    match hit_card {
        Some(hit) => {
            // CR 701.57a: the discovering player chooses to cast without paying
            // or put the hit card into their hand.
            state.waiting_for = WaitingFor::CastOffer {
                player: discovering_player,
                kind: CastOfferKind::Discover {
                    hit_card: hit,
                    exiled_misses,
                    discover_value: limit,
                },
            };
        }
        None => {
            // CR 701.57a: No hit — put all exiled misses on bottom in random order
            shuffle_to_bottom(state, &exiled_misses, discovering_player, events);
        }
    }

    Ok(())
}

/// Put cards on the bottom of the player's library in random order.
fn shuffle_to_bottom(
    state: &mut GameState,
    cards: &[ObjectId],
    _player_id: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) {
    use rand::seq::SliceRandom;

    let mut shuffled = cards.to_vec();
    shuffled.shuffle(&mut state.rng);

    for &card_id in &shuffled {
        zones::move_to_library_position(state, card_id, false, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ObjectScope, QuantityExpr, QuantityRef, TargetFilter};
    use crate::types::events::GameEvent;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;

    #[test]
    fn test_discover_finds_nonland_card() {
        let mut state = GameState::new_two_player(42);
        // Create library: land, land, nonland (MV 2)
        let land1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let land2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&creature).unwrap().mana_cost = ManaCost::generic(2);

        let ability = ResolvedAbility::new(
            Effect::Discover {
                mana_value_limit: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should find the creature and set DiscoverChoice
        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Discover {
                        hit_card,
                        exiled_misses,
                        ..
                    },
                ..
            } => {
                assert_eq!(*hit_card, creature);
                assert_eq!(exiled_misses.len(), 2, "Should have 2 land misses");
            }
            other => panic!("Expected DiscoverChoice, got {:?}", other),
        }
    }

    #[test]
    fn discover_limit_can_use_triggering_spell_mana_value() {
        let mut state = GameState::new_two_player(42);

        let hit = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Four Drop".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&hit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(4);
        }

        let triggering_spell = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Triggering Spell".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&triggering_spell).unwrap().mana_cost = ManaCost::generic(4);
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(4),
            controller: PlayerId(0),
            object_id: triggering_spell,
        });

        let ability = ResolvedAbility::new(
            Effect::Discover {
                mana_value_limit: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::EventSource,
                    },
                },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if hit_card == hit
        ));
    }

    /// CR 701.57a (player-redirect): "Then that player discovers X" (Zoyowa's
    /// Justice) makes a *redirected* player — not the controller — exile from
    /// their own library. The discover scans the opponent's library and the
    /// cast/keep offer is addressed to the opponent. Reverting the `player`
    /// plumbing (back to `ability.controller`) flips this: the controller's
    /// empty library would yield no hit and no `CastOffer` at all.
    #[test]
    fn discover_redirects_to_other_player_library() {
        use crate::types::ability::{ControllerRef, TargetRef, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);

        // The hit lives in the OPPONENT's library; the controller's library is
        // empty, so a non-redirected discover would find nothing.
        let hit = create_object(
            &mut state,
            CardId(7),
            opponent,
            "Opponent Two Drop".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&hit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(2);
        }

        // Mirror the parser output for a redirected discover: a player filter in
        // `player` with the chosen player declared in `targets`. (Zoyowa uses
        // `ParentTargetOwner`; an explicit `Opponent` Player target exercises the
        // same resolver redirect through a directly assertable seam.)
        let ability = ResolvedAbility::new(
            Effect::Discover {
                mana_value_limit: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Player(opponent)],
            ObjectId(100),
            controller,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } => {
                assert_eq!(
                    *player, opponent,
                    "the redirected player (not the controller) is offered the discover"
                );
                assert_eq!(
                    *hit_card, hit,
                    "discover scans the redirected player's library, not the controller's"
                );
            }
            other => panic!("expected CastOffer addressed to the opponent, got {other:?}"),
        }
    }

    /// CR 701.57a + CR 608.2c: Zoyowa's Justice-style "that player discovers X"
    /// binds the discovering player through the parent object target's owner. A
    /// direct `target opponent` filter takes the declared player-target branch;
    /// this regression exercises the context-ref branch used by the actual
    /// "that player" parser output.
    #[test]
    fn discover_parent_target_owner_uses_parent_object_owner_library() {
        use crate::types::ability::TargetRef;

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);

        let parent_target = create_object(
            &mut state,
            CardId(8),
            opponent,
            "Opponent-Owned Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&parent_target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let hit = create_object(
            &mut state,
            CardId(9),
            opponent,
            "Opponent Two Drop".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&hit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(2);
        }

        let ability = ResolvedAbility::new(
            Effect::Discover {
                mana_value_limit: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::ParentTargetOwner,
            },
            vec![TargetRef::Object(parent_target)],
            ObjectId(100),
            controller,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } => {
                assert_eq!(
                    *player, opponent,
                    "the parent target's owner, not the controller, is offered the discover"
                );
                assert_eq!(
                    *hit_card, hit,
                    "ParentTargetOwner discover scans the parent target owner's library"
                );
            }
            other => {
                panic!("expected CastOffer addressed to the parent target owner, got {other:?}")
            }
        }
    }
}
