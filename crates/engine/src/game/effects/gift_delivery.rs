use crate::game::players;
use crate::types::ability::{
    Effect, EffectError, EffectKind, PtValue, QuantityExpr, ResolvedAbility, TargetFilter,
    TokenAttackDestination,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::keywords::GiftKind;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;

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
                ability,
                opponent,
                GiftToken::artifact("Treasure"),
                events,
            )?;
        }
        // CR 702.174d: "Gift a Food" means the chosen player creates a Food token.
        GiftKind::Food => {
            create_gift_token(
                state,
                ability,
                opponent,
                GiftToken::artifact("Food"),
                events,
            )?;
        }
        // CR 702.174f: "Gift a tapped Fish" means the chosen player creates a
        // tapped 1/1 blue Fish creature token.
        GiftKind::TappedFish => {
            create_gift_token(state, ability, opponent, GiftToken::tapped_fish(), events)?;
        }
        // CR 702.174g: "Gift an extra turn" means "The chosen player takes an
        // extra turn after this one." CR 500.7 owns the queue, so this routes
        // through the same authority `Effect::ExtraTurn` uses rather than
        // touching `extra_turns` directly.
        //
        // "After this one" is the ANCHOR: the extra turn follows the turn during
        // which the gift resolved, which is `state.active_player`'s — not the
        // recipient's next turn. `enqueue_extra_turn` takes that anchor exactly
        // as the effect resolver passes it.
        GiftKind::ExtraTurn => {
            crate::game::turns::enqueue_extra_turn(state, opponent, state.active_player, events);
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

/// The token a gift hands over, in the shape the Oracle parser gives the same
/// token ("create a Treasure token", "create a 1/1 blue Fish creature token").
struct GiftToken {
    name: &'static str,
    power_toughness: i32,
    core_type: &'static str,
    colors: Vec<ManaColor>,
    tapped: bool,
}

impl GiftToken {
    /// CR 111.10a + CR 111.10b: a predefined artifact token (Treasure, Food).
    fn artifact(name: &'static str) -> Self {
        Self {
            name,
            power_toughness: 0,
            core_type: "Artifact",
            colors: Vec::new(),
            tapped: false,
        }
    }

    /// CR 702.174f: a tapped 1/1 blue Fish creature token.
    fn tapped_fish() -> Self {
        Self {
            name: "Fish",
            power_toughness: 1,
            core_type: "Creature",
            colors: vec![ManaColor::Blue],
            tapped: true,
        }
    }
}

/// CR 702.174d/f/h + CR 111.2: the chosen player creates the gift token, so it
/// resolves as that player's own token creation through the standard token
/// resolver, as `investigate` does for its Clue. That makes it a token for every
/// token rule (CR 111.1, the CR 704.5d cleanup, "token" filters), gives it its
/// predefined abilities (CR 111.10a, CR 111.10b) and exposes the creation to
/// token replacement effects (CR 614.1a).
fn create_gift_token(
    state: &mut GameState,
    ability: &ResolvedAbility,
    recipient: PlayerId,
    token: GiftToken,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let creation = ResolvedAbility::new(
        Effect::Token {
            name: token.name.to_string(),
            power: PtValue::Fixed(token.power_toughness),
            toughness: PtValue::Fixed(token.power_toughness),
            types: vec![token.core_type.to_string(), token.name.to_string()],
            colors: token.colors,
            keywords: Vec::new(),
            tapped: token.tapped,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            attack_destination: TokenAttackDestination::ControllerChooses,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        },
        Vec::new(),
        ability.source_id,
        recipient,
    );
    super::token::resolve(state, &creation, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::zones::Zone;

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
        assert_eq!(
            events,
            vec![
                GameEvent::ExtraTurnCreated {
                    player_id: PlayerId(1),
                    anchor: state.active_player,
                },
                GameEvent::EffectResolved {
                    kind: EffectKind::GiftDelivery,
                    source_id: ObjectId(100),
                    subject: None,
                },
            ]
        );
        let turn = &state.extra_turns[0];
        let GameEvent::ExtraTurnCreated { player_id, anchor } = &events[0] else {
            unreachable!();
        };
        assert_eq!((*player_id, *anchor), (turn.player, turn.anchor));
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
        assert!(events.is_empty());
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ExtraTurnCreated { .. })));
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

    /// The gift tokens the recipient (`PlayerId(1)`) owns.
    fn recipient_tokens(state: &GameState) -> Vec<&crate::game::game_object::GameObject> {
        state
            .objects
            .values()
            .filter(|o| o.is_token && o.owner == PlayerId(1) && o.zone == Zone::Battlefield)
            .collect()
    }

    #[test]
    fn gift_treasure_creates_token_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Treasure, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let tokens = recipient_tokens(&state);
        let [token] = tokens.as_slice() else {
            panic!("one Treasure token for the opponent, got {tokens:?}");
        };
        assert_eq!(token.controller, PlayerId(1));
        assert!(token.card_types.subtypes.contains(&"Treasure".to_string()));
        assert!(token.card_types.core_types.contains(&CoreType::Artifact));
        // CR 111.10a: the predefined Treasure mana ability comes with the token.
        assert!(
            token
                .abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::Mana { .. })),
            "Treasure needs its mana ability"
        );
    }

    #[test]
    fn gift_tapped_fish_creates_tapped_token() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::TappedFish, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let tokens = recipient_tokens(&state);
        let [token] = tokens.as_slice() else {
            panic!("one Fish token for the opponent, got {tokens:?}");
        };
        assert_eq!(token.controller, PlayerId(1));
        assert_eq!(token.power, Some(1));
        assert_eq!(token.toughness, Some(1));
        assert!(token.tapped, "Fish should enter tapped");
        assert!(token.color.contains(&ManaColor::Blue));
        assert!(token.card_types.subtypes.contains(&"Fish".to_string()));
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
    }

    #[test]
    fn gift_food_creates_food_token_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Food, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let tokens = recipient_tokens(&state);
        let [token] = tokens.as_slice() else {
            panic!("one Food token for the opponent, got {tokens:?}");
        };
        assert!(token.card_types.subtypes.contains(&"Food".to_string()));
        // CR 111.10b: "{2}, {T}, Sacrifice this token: You gain 3 life."
        assert!(
            token.abilities.iter().any(|a| matches!(
                *a.effect,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    ..
                }
            )),
            "Food needs its gain-3-life ability"
        );
    }

    /// CR 704.5d: a gift Fish that leaves the battlefield ceases to exist; it
    /// must not stay in its owner's graveyard like a card.
    #[test]
    fn dead_gift_fish_ceases_to_exist() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_gift_ability(GiftKind::TappedFish, true),
            &mut events,
        )
        .unwrap();
        let fish = recipient_tokens(&state)
            .first()
            .map(|o| o.id)
            .expect("gift Fish on the battlefield");

        zones::move_to_zone(&mut state, fish, Zone::Graveyard, &mut events);
        assert!(
            state.players[1].graveyard.contains(&fish),
            "the Fish goes to the graveyard first (CR 111.7 note: dies triggers see it)"
        );
        crate::game::sba::check_state_based_actions(&mut state, &mut events);

        assert!(
            !state.players[1].graveyard.contains(&fish),
            "CR 704.5d: the Fish ceases to exist"
        );
        assert!(!state.objects.contains_key(&fish));
    }
}
