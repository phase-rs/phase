use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::{players, zones};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::{CardType, CoreType};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::keywords::GiftKind;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 702.174: Deliver a gift to the opponent of the ability's controller.
/// Gift delivery is a no-op when the gift wasn't promised (`additional_cost_paid == false`).
/// When promised, the opponent receives the gift before the spell's other effects resolve.
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

    // In 2-player, the opponent is the next player after the controller.
    let opponent = players::next_player(state, ability.controller);

    // CR 702.174b: On a permanent, the gift ability triggers when the permanent enters.
    // CR 702.174j: For instants/sorceries, the gift effect always happens first.
    match kind {
        // CR 702.174e: "Gift a card" means the chosen player draws a card.
        GiftKind::Card => {
            deliver_card_draw(state, events, opponent)?;
        }
        // CR 702.174h: "Gift a Treasure" means the chosen player creates a Treasure token.
        GiftKind::Treasure => {
            create_gift_token(state, events, opponent, "Treasure", |ct| {
                ct.core_types.push(CoreType::Artifact);
                ct.subtypes.push("Treasure".to_string());
            });
        }
        GiftKind::Food => {
            create_gift_token(state, events, opponent, "Food", |ct| {
                ct.core_types.push(CoreType::Artifact);
                ct.subtypes.push("Food".to_string());
            });
        }
        GiftKind::TappedFish => {
            let obj_id = create_gift_token(state, events, opponent, "Fish", |ct| {
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
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GiftDelivery,
        source_id: ability.source_id,
    });

    Ok(())
}

/// Deliver "gift a card" — opponent draws one card.
/// Routes through the replacement system so draw-replacement effects apply (CR 121.1).
fn deliver_card_draw(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    opponent: PlayerId,
) -> Result<(), EffectError> {
    let proposed = ProposedEvent::Draw {
        player_id: opponent,
        count: 1,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::Draw {
                player_id, count, ..
            } = event
            {
                let player = state
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .ok_or(EffectError::PlayerNotFound)?;

                let cards_to_draw: Vec<_> = player
                    .library
                    .iter()
                    .take(count as usize)
                    .copied()
                    .collect();

                for obj_id in cards_to_draw {
                    zones::move_to_zone(state, obj_id, Zone::Hand, events);
                    // CR 121.1 + CR 504.1: Increment counters first; embed the
                    // resulting per-step ordinal into the event so trigger
                    // conditions can identify the first draw of the draw step.
                    let nth_in_step =
                        if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                            p.cards_drawn_this_turn = p.cards_drawn_this_turn.saturating_add(1);
                            p.cards_drawn_this_step = p.cards_drawn_this_step.saturating_add(1);
                            p.cards_drawn_this_step
                        } else {
                            1
                        };
                    events.push(GameEvent::CardDrawn {
                        player_id,
                        object_id: obj_id,
                        nth_in_step,
                    });
                    // CR 702.94a + CR 603.11: Shared first-draw / miracle-offer hook.
                    super::draw::record_first_draw_and_enqueue_miracle(state, player_id, obj_id);
                }
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
        }
    }

    Ok(())
}

/// Create a token for a specific player with customizable card type setup.
/// Returns the ObjectId so callers can further customize the token (e.g., colors, P/T).
fn create_gift_token(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    owner: PlayerId,
    name: &str,
    setup: impl FnOnce(&mut CardType),
) -> crate::types::identifiers::ObjectId {
    let obj_id = zones::create_object(state, CardId(0), owner, name.to_string(), Zone::Battlefield);

    if let Some(obj) = state.objects.get_mut(&obj_id) {
        let mut card_type = CardType::default();
        setup(&mut card_type);
        obj.card_types = card_type.clone();
        obj.base_card_types = card_type;
    }

    // CR 400.7 + CR 302.6 + CR 603.6a: Single authority for ETB state.
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.reset_for_battlefield_entry(state.turn_number);
    }

    state.layers_dirty = true;
    crate::game::restrictions::record_battlefield_entry(state, obj_id);
    crate::game::restrictions::record_token_created(state, obj_id);

    // CR 111.1 + CR 603.6a: Token creation is a zone change from outside the
    // game — emit `ZoneChanged { from: None }` so ETB triggers (Soul Warden,
    // Panharmonicon, etc.) fire for gift tokens through the normal code path.
    let zone_change_record = state
        .objects
        .get(&obj_id)
        .expect("token just created")
        .snapshot_for_zone_change(obj_id, None, Zone::Battlefield);
    events.push(GameEvent::ZoneChanged {
        object_id: obj_id,
        from: None,
        to: Zone::Battlefield,
        record: Box::new(zone_change_record),
    });

    events.push(GameEvent::TokenCreated {
        object_id: obj_id,
        name: name.to_string(),
    });

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
