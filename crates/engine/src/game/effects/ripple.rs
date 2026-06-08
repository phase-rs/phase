use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{CastOfferKind, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 702.60a: Ripple N — when you cast this spell, you may reveal the top N
/// cards of your library, cast any of them with the same name as this spell
/// without paying their mana cost, then put the rest on the bottom of your
/// library in a random order.
///
/// Reveal is modeled by moving the top N cards to exile (face up, like Cascade);
/// the matching card is cast during resolution via the shared
/// `initiate_cast_during_resolution` authority, and the non-cast revealed cards
/// are moved to the bottom by the resolution-choice handler after all same-named
/// cards the player chooses to cast from this reveal have been offered.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::Ripple { count } = ability.effect else {
        return Err(EffectError::InvalidParam("Expected Ripple".to_string()));
    };

    // CR 603.3a: Re-read the controller from the source spell at resolution time
    // (a control-change between trigger creation and resolution is honored); fall
    // back to the trigger snapshot if the spell has left the stack.
    let controller = state
        .objects
        .get(&ability.source_id)
        .map(|obj| obj.controller)
        .unwrap_or(ability.controller);

    if !state.players.iter().any(|p| p.id == controller) {
        return Err(EffectError::PlayerNotFound);
    }

    // CR 702.60a: same name as *this* spell. Read the source spell's name.
    let source_name = state
        .objects
        .get(&ability.source_id)
        .map(|obj| obj.name.clone())
        .unwrap_or_default();

    // CR 702.60a: reveal the top N cards (modeled as a face-up exile). Each
    // iteration re-reads the live library top so a replacement that mutated the
    // library mid-reveal is observed (mirrors Cascade).
    let mut revealed: Vec<ObjectId> = Vec::new();
    for _ in 0..count {
        let Some(card_id) = state
            .players
            .iter()
            .find(|p| p.id == controller)
            .and_then(|p| p.library.front().copied())
        else {
            break;
        };

        zones::move_to_zone(state, card_id, Zone::Exile, events);

        // CR 614.1: a replacement may have redirected the card elsewhere; only
        // count it as revealed if it actually landed in exile.
        if state.objects.get(&card_id).map(|o| o.zone) != Some(Zone::Exile) {
            if state
                .players
                .iter()
                .find(|p| p.id == controller)
                .is_some_and(|p| p.library.front().copied() == Some(card_id))
            {
                // Defensive: card somehow still on top — break to avoid looping.
                break;
            }
            continue;
        }
        revealed.push(card_id);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    let (mut hits, revealed_misses): (Vec<_>, Vec<_>) = revealed.into_iter().partition(|id| {
        !source_name.is_empty() && state.objects.get(id).is_some_and(|o| o.name == source_name)
    });

    match hits.is_empty() {
        false => {
            let hit_card = hits.remove(0);
            // CR 702.60a: offer the free cast. The accept/decline + bottoming of
            // the rest is handled in `engine_resolution_choices`.
            state.waiting_for = WaitingFor::CastOffer {
                player: controller,
                kind: CastOfferKind::Ripple {
                    hit_card,
                    remaining_hits: hits,
                    revealed_misses,
                },
            };
        }
        true => {
            // CR 702.60a: no same-named card revealed — put them all on the
            // bottom of the library.
            super::cascade::shuffle_to_bottom(state, &revealed_misses, events);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn setup(name: &str) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            name.to_string(),
            Zone::Stack,
        );
        (state, source_id)
    }

    fn add_library_card(state: &mut GameState, name: &str) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        create_object(state, card_id, PlayerId(0), name.to_string(), Zone::Library)
    }

    /// CR 702.60a: a same-named card in the top N is offered for a free cast.
    #[test]
    fn offers_same_named_revealed_card() {
        let (mut state, source_id) = setup("Surging Flame");
        let other = add_library_card(&mut state, "Mountain");
        let match_card = add_library_card(&mut state, "Surging Flame");
        state.players[0].library = im::vector![other, match_card];

        let ability =
            ResolvedAbility::new(Effect::Ripple { count: 2 }, vec![], source_id, PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Ripple {
                        hit_card,
                        remaining_hits,
                        revealed_misses,
                    },
                ..
            } => {
                assert_eq!(*hit_card, match_card);
                assert!(remaining_hits.is_empty());
                assert_eq!(revealed_misses, &vec![other]);
            }
            other => panic!("expected Ripple CastOffer, got {other:?}"),
        }
    }

    /// CR 702.60a: all same-named cards revealed by one ripple remain eligible.
    #[test]
    fn offers_all_same_named_revealed_cards_before_misses() {
        let (mut state, source_id) = setup("Surging Flame");
        let first_match = add_library_card(&mut state, "Surging Flame");
        let miss = add_library_card(&mut state, "Mountain");
        let second_match = add_library_card(&mut state, "Surging Flame");
        state.players[0].library = im::vector![first_match, miss, second_match];

        let ability =
            ResolvedAbility::new(Effect::Ripple { count: 3 }, vec![], source_id, PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Ripple {
                        hit_card,
                        remaining_hits,
                        revealed_misses,
                    },
                ..
            } => {
                assert_eq!(*hit_card, first_match);
                assert_eq!(remaining_hits, &vec![second_match]);
                assert_eq!(revealed_misses, &vec![miss]);
            }
            other => panic!("expected Ripple CastOffer, got {other:?}"),
        }
    }

    /// CR 702.60a: no same-named card revealed — all go to the bottom, no offer.
    #[test]
    fn no_match_bottoms_revealed_cards() {
        let (mut state, source_id) = setup("Surging Might");
        let a = add_library_card(&mut state, "Forest");
        let b = add_library_card(&mut state, "Bear");
        state.players[0].library = im::vector![a, b];

        let ability =
            ResolvedAbility::new(Effect::Ripple { count: 2 }, vec![], source_id, PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::CastOffer {
                    kind: CastOfferKind::Ripple { .. },
                    ..
                }
            ),
            "no same-named card should produce no offer"
        );
        // Both revealed cards returned to the library (bottom).
        assert_eq!(state.players[0].library.len(), 2);
        for id in [a, b] {
            assert_eq!(state.objects.get(&id).map(|o| o.zone), Some(Zone::Library));
        }
    }

    /// CR 702.60a: empty library reveals nothing and offers nothing.
    #[test]
    fn empty_library_no_offer() {
        let (mut state, source_id) = setup("Surging Aether");
        state.players[0].library.clear();

        let ability =
            ResolvedAbility::new(Effect::Ripple { count: 1 }, vec![], source_id, PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(!matches!(
            state.waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Ripple { .. },
                ..
            }
        ));
    }
}
