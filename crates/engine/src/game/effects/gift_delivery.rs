use crate::game::{players, zones};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::{CardType, CoreType};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::GiftKind;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 702.174: Deliver a gift to the opponent chosen when the gift cost was paid.
/// Gift delivery is a no-op when the gift wasn't promised (`additional_cost_paid == false`).
/// When promised, the chosen opponent receives the gift before the spell's other effects resolve.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let kind = match &ability.effect {
        Effect::GiftDelivery { kind } => kind.clone(),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected GiftDelivery effect".to_string(),
            ))
        }
    };

    // Gift delivery only fires when the gift was promised (additional cost paid).
    // When not promised, this is a no-op — the sub_ability chain continues to the
    // spell's normal effects.
    if !ability.context.additional_cost_paid {
        return Ok(());
    }

    // CR 702.174a/e: Deliver to the opponent chosen when the gift cost was paid.
    // Prefer the cast-time SpellContext latch, then the finalize stamp on the
    // source object. Never fall back to turn-order `next_player`.
    let Some(opponent) = resolve_gift_recipient(state, ability) else {
        // CR 800.4b / CR 609.3: Latched recipient left the game, or the cast
        // path failed to stamp a recipient — do as much as possible (nothing).
        return Ok(());
    };

    // CR 702.174b: On a permanent, the gift ability triggers when the permanent enters.
    // CR 702.174j: For instants/sorceries, the gift effect always happens first.
    match kind {
        // CR 702.174e: "Gift a card" means the chosen player draws a card.
        GiftKind::Card => {
            deliver_card_draw(state, events, opponent)?;
        }
        // CR 702.174h: "Gift a Treasure" means the chosen player creates a Treasure token.
        GiftKind::Treasure => {
            create_gift_token(
                state,
                events,
                opponent,
                "Treasure",
                ability.source_id,
                |ct| {
                    ct.core_types.push(CoreType::Artifact);
                    ct.subtypes.push("Treasure".to_string());
                },
            );
        }
        GiftKind::Food => {
            create_gift_token(state, events, opponent, "Food", ability.source_id, |ct| {
                ct.core_types.push(CoreType::Artifact);
                ct.subtypes.push("Food".to_string());
            });
        }
        GiftKind::TappedFish => {
            let obj_id =
                create_gift_token(state, events, opponent, "Fish", ability.source_id, |ct| {
                    ct.core_types.push(CoreType::Creature);
                    ct.subtypes.push("Fish".to_string());
                });
            if let Some(obj) = state.objects.get_mut(&obj_id) {
                obj.color = vec![ManaColor::Blue];
                obj.base_color = vec![ManaColor::Blue];
                obj.power = Some(1);
                obj.toughness = Some(1);
                obj.base_power = Some(1);
                obj.base_toughness = Some(1);
                obj.tapped = true;
            }
        }
        // CR 702.174g: "Gift an extra turn" means "The chosen player takes an
        // extra turn after this one." CR 500.7 owns the queue, so this routes
        // through the same authority `Effect::ExtraTurn` uses rather than
        // touching `extra_turns` directly.
        //
        // "After this one" is the ANCHOR: the extra turn follows the turn during
        // which the gift resolved, which is `state.active_player`'s — not the
        // recipient's next turn. `enqueue_extra_turn` takes that anchor as its
        // third argument, exactly as the effect resolver passes it.
        GiftKind::ExtraTurn => {
            // CR 805.8: with shared team turns the extra turn is taken by the
            // recipient's team; the same normalization the effect resolver does.
            let recipient = crate::game::topology::normalize_shared_turn_recipient(state, opponent);
            crate::game::turns::enqueue_extra_turn(state, recipient, state.active_player);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GiftDelivery,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 702.174a: Resolve the latched gift recipient.
fn resolve_gift_recipient(state: &GameState, ability: &ResolvedAbility) -> Option<PlayerId> {
    let candidate = ability.context.gift_recipient.or_else(|| {
        state
            .objects
            .get(&ability.source_id)
            .and_then(|obj| obj.gift_recipient)
    })?;
    // CR 800.4: Only deliver if the chosen player is still in the game.
    players::is_alive(state, candidate).then_some(candidate)
}

/// Deliver "gift a card" — opponent draws one card.
/// Routes through the single-authority `start_draw_sequence` path so
/// draw-replacement effects apply and CR 121.1's `allowed_draw_count` gate
/// honors `CantDraw` and `PerTurnDrawLimit` statics. The old direct
/// `select_cards_to_draw` call bypassed that gate for Gift draws.
fn deliver_card_draw(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    opponent: PlayerId,
) -> Result<(), EffectError> {
    // CR 614.1a + CR 614.6 + CR 704.3: The sequence driver retains replacement
    // pauses and drains post-replacement continuations in this resolution step.
    let _ = super::draw::start_draw_sequence(state, opponent, 1, events);

    Ok(())
}

/// Create a token for a specific player with customizable card type setup.
/// Returns the ObjectId so callers can further customize the token (e.g., colors, P/T).
fn create_gift_token(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    owner: PlayerId,
    name: &str,
    source_id: ObjectId,
    setup: impl FnOnce(&mut CardType),
) -> crate::types::identifiers::ObjectId {
    let obj_id = zones::create_object(state, CardId(0), owner, name.to_string(), Zone::Battlefield);

    if let Some(obj) = state.objects.get_mut(&obj_id) {
        let mut card_type = CardType::default();
        setup(&mut card_type);
        obj.card_types = card_type.clone();
        obj.base_card_types = card_type;
    }

    // CR 613.7d: the gift token enters the battlefield, so it receives a
    // timestamp. Drawn before the `get_mut` (`next_timestamp` takes `&mut self`).
    let entry_timestamp = state.next_timestamp();

    // CR 400.7 + CR 302.6 + CR 603.6a: Single authority for ETB state.
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.reset_for_battlefield_entry(state.turn_number, entry_timestamp);
    }

    crate::game::layers::mark_layers_full(state);
    crate::game::restrictions::record_token_created(state, obj_id);

    // CR 111.1 + CR 603.6a: Token creation is a zone change from outside the
    // game — emit `ZoneChanged { from: None }` so ETB triggers (Soul Warden,
    // Panharmonicon, etc.) fire for gift tokens through the normal code path.
    //
    // CR 400.7 + CR 608.2i + CR 603.2c: route the record and the entry pair through the single
    // `from: None → Battlefield` authority so the emitted `ZoneChanged` carries this turn's real
    // zone-change index instead of the `0` placeholder. The authority performs the CR 608.2i
    // battlefield-entry bookkeeping itself, so the co-located `record_battlefield_entry` call is
    // deleted — keeping it would double-count `battlefield_entries_this_turn`.
    super::token::push_committed_token_entry_events(
        state,
        obj_id,
        name.to_string(),
        source_id,
        events,
    )
    .expect("token just created");

    obj_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ResolvedAbility;
    use crate::types::identifiers::ObjectId;

    fn make_gift_ability(kind: GiftKind, promised: bool) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::GiftDelivery { kind },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.context.additional_cost_paid = promised;
        if promised {
            // CR 702.174a: Latched recipient (2p sole opponent = P1).
            ability.context.gift_recipient = Some(PlayerId(1));
        }
        ability
    }

    #[test]
    fn gift_card_opponent_draws_when_promised() {
        let mut state = GameState::new_two_player(42);
        let card_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Card, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].hand.contains(&card_id));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == PlayerId(1))
        ));
    }

    /// CR 702.174g + CR 500.7: the promised extra turn is queued for the chosen
    /// player, anchored after the turn during which the gift resolved.
    #[test]
    fn gift_extra_turn_queues_a_turn_for_the_recipient() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::ExtraTurn, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state
                .extra_turns
                .iter()
                .map(|turn| (turn.player, turn.anchor))
                .collect::<Vec<_>>(),
            vec![(PlayerId(1), state.active_player)],
            "CR 702.174g: the CHOSEN player takes the extra turn, after this one"
        );
    }

    /// The negative that keeps the row above honest: an unpromised gift queues
    /// nothing, so the assertion is about the promise and not about the queue
    /// being writable.
    #[test]
    fn gift_extra_turn_queues_nothing_when_not_promised() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::ExtraTurn, false);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.extra_turns.is_empty());
    }

    #[test]
    fn gift_card_uses_source_object_recipient_when_context_is_absent() {
        let mut state = GameState::new_two_player(42);
        let source_id = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Gift Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source_id).unwrap().gift_recipient = Some(PlayerId(1));
        let card_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let mut ability = make_gift_ability(GiftKind::Card, true);
        ability.source_id = source_id;
        ability.context.gift_recipient = None;
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].hand.contains(&card_id));
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == PlayerId(1))
        ));
    }

    #[test]
    fn gift_card_noops_for_eliminated_recipient() {
        let mut state = GameState::new_two_player(42);
        let card_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Library,
        );
        state.players[1].is_eliminated = true;
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Card, true);
        assert_eq!(resolve_gift_recipient(&state, &ability), None);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].library.contains(&card_id));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { .. })));
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::GiftDelivery,
                ..
            }
        )));
    }

    #[test]
    fn gift_card_noop_when_not_promised() {
        let mut state = GameState::new_two_player(42);
        let card_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Card, false);
        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent should NOT have drawn
        assert!(state.players[1].library.contains(&card_id));
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CardDrawn { .. })));
    }

    #[test]
    fn gift_treasure_creates_token_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Treasure, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .objects
            .values()
            .find(|o| o.card_id == CardId(0) && o.owner == PlayerId(1));
        assert!(token.is_some(), "Treasure token should exist for opponent");
        let token = token.unwrap();
        assert!(token.card_types.subtypes.contains(&"Treasure".to_string()));
        assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    }

    #[test]
    fn gift_tapped_fish_creates_tapped_token() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::TappedFish, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .objects
            .values()
            .find(|o| o.card_id == CardId(0) && o.owner == PlayerId(1));
        assert!(token.is_some(), "Fish token should exist for opponent");
        let token = token.unwrap();
        assert_eq!(token.power, Some(1));
        assert_eq!(token.toughness, Some(1));
        assert!(token.tapped, "Fish should enter tapped");
        assert!(token.color.contains(&ManaColor::Blue));
    }

    #[test]
    fn gift_food_creates_food_token_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Food, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .objects
            .values()
            .find(|o| o.card_id == CardId(0) && o.owner == PlayerId(1));
        assert!(token.is_some(), "Food token should exist for opponent");
        let token = token.unwrap();
        assert!(token.card_types.subtypes.contains(&"Food".to_string()));
    }
}
