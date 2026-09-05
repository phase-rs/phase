use crate::game::zone_pipeline::BatchMoveResult;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, CastOfferKind, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    ResolvedInformationAudience, ResolvedInformationEdit, ResolvedInformationLifetime,
};
use crate::types::zones::Zone;

/// CR 702.60a: Ripple N — "When you cast this spell, you may reveal the top N
/// cards of your library, or, if there are fewer than N cards in your library,
/// you may reveal all the cards in your library. If you reveal cards from your
/// library this way, you may cast any of those cards with the same name as this
/// spell without paying their mana costs, then put all revealed cards not cast
/// this way on the bottom of your library in any order."
///
/// This is a two-decision effect (CR 608.2d):
/// 1. `WaitingFor::RippleRevealChoice` — the optional reveal ("you **may**
///    reveal"). Declining leaves the library untouched and publishes nothing.
/// 2. `WaitingFor::RippleBottomOrder` — the controller announces the bottom
///    placement order for the uncast revealed cards ("in any order"). Raised
///    only when 2+ cards remain.
///
/// CR 701.20b: revealing does NOT move the revealed cards — they stay on top of
/// the library while the free-cast offers run. The matching card is cast *from
/// the library* during resolution via the shared
/// `initiate_cast_during_resolution` authority (its
/// `ExileWithAltCost { resolution_cleanup: Some(_) }` grant is zone-agnostic —
/// see `castable_from_current_zone`).
///
/// Everything after decision 1's accept lives in
/// [`crate::game::engine_resolution_choices`] alongside the `RippleChoice` /
/// `SelectCards` handlers; this resolver only computes the reveal size and opens
/// decision 1.
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

    // CR 702.60a: how many cards the reveal would show — the top N, or the
    // whole library if it holds fewer than N.
    let available = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .map(|p| p.library.len().min(count as usize))
        .unwrap_or(0);

    // The Ripple effect's resolver has run; the reveal decision and its
    // follow-ups are driven from the resolution-choice handler.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    if available == 0 {
        // CR 702.60a: an empty library has nothing to reveal — resolve cleanly.
        return Ok(());
    }

    // CR 702.60a: "you **may** reveal the top N cards of your library." Offer
    // that decision before anything is revealed or published.
    state.waiting_for = WaitingFor::RippleRevealChoice {
        player: controller,
        source_id: ability.source_id,
        count: available as u32,
    };
    Ok(())
}

/// CR 702.60a: the controller accepted the optional reveal. Publish the top-N
/// pile (still in the library, CR 701.20b), then either offer the first
/// same-named card for a free cast or move to the bottom-order step.
pub(crate) fn perform_reveal_and_offer(
    state: &mut GameState,
    source_id: ObjectId,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    let Some(controller) = state.objects.get(&source_id).map(|obj| obj.controller) else {
        return;
    };
    let source_name = state
        .objects
        .get(&source_id)
        .map(|obj| obj.name.clone())
        .unwrap_or_default();

    // CR 702.60a + CR 701.20b: reveal the top N (or all) WITHOUT moving them.
    let revealed: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .map(|p| p.library.iter().take(count as usize).copied().collect())
        .unwrap_or_default();

    if revealed.is_empty() {
        return;
    }

    publish_ripple_reveal(state, controller, &revealed, events);

    // `partition` preserves top-first order within each bucket.
    let (mut hits, revealed_misses): (Vec<_>, Vec<_>) = revealed.into_iter().partition(|id| {
        !source_name.is_empty() && state.objects.get(id).is_some_and(|o| o.name == source_name)
    });

    if hits.is_empty() {
        // CR 702.60a: no same-named card — go straight to the bottom-order step.
        open_bottom_order_or_place(state, source_id, controller, revealed_misses, None, events);
    } else {
        let hit_card = hits.remove(0);
        state.waiting_for = WaitingFor::CastOffer {
            player: controller,
            kind: CastOfferKind::Ripple {
                hit_card,
                remaining_hits: hits,
                revealed_misses,
                source_id,
            },
        };
    }
}

/// CR 702.60a + CR 608.2d: place the uncast revealed cards on the bottom of the
/// library "in any order". With 2+ cards, prompt the controller for the order
/// (`WaitingFor::RippleBottomOrder`); with 0 or 1 there is no ordering choice,
/// so place immediately. `final_cast` is threaded to
/// `BatchCompletion::RippleTerminalComplete` so the parked-trigger / terminal
/// `SpellCast` settlement fires once the cards land.
pub(crate) fn open_bottom_order_or_place(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    cards: Vec<ObjectId>,
    final_cast: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    if cards.len() >= 2 {
        state.waiting_for = WaitingFor::RippleBottomOrder {
            player: controller,
            source_id,
            cards,
            final_cast,
        };
        return BatchMoveResult::Done;
    }
    place_on_library_bottom(state, source_id, &cards, final_cast, events)
}

/// CR 702.60a + CR 603.3b: place `ordered` on the library bottom in the given
/// order and fire `RippleTerminalComplete`. The completion is what un-pauses the
/// resolving Ripple trigger (it sets `waiting_for` back to `Priority`), so it
/// runs on *every* terminal path — even an empty `ordered` (a declined reveal or
/// an all-hits Ripple) still passes an empty batch to fire it.
pub(crate) fn place_on_library_bottom(
    state: &mut GameState,
    source_id: ObjectId,
    ordered: &[ObjectId],
    final_cast: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let completion = state
        .objects
        .get(&source_id)
        .map(|obj| obj.controller)
        .map(|player| BatchCompletion::RippleTerminalComplete {
            player,
            source_id,
            final_cast,
        });
    crate::game::engine_resolution_choices::route_rest_partition_then(
        state,
        ordered,
        Zone::Library,
        Some(source_id),
        completion,
        events,
    )
}

/// CR 701.20a/b: Publish a Ripple reveal. The cards stay in the library, so
/// visibility rides entirely on the resolved-information sets:
///
/// * `Controller` / `UntilActionBoundary` feeds `state.revealed_cards`, which
///   `is_visible_revealed_card` honors for every viewer in every zone (the
///   library included). `apply_action` keeps this set alive across the
///   `CastOffer { kind: Ripple }` boundary (see `engine.rs`).
/// * `Public` / `UntilZoneChange` is the durable CR 701.20a public fact,
///   auto-cleared per card when it changes zones (cast to the stack, or
///   bottomed within the library).
///
/// One `CardsRevealed` event carries the whole simultaneously-revealed pile
/// (CR 701.20a); it also lights up the game log and the client reveal
/// animation. `last_revealed_ids` is set for `LastRevealed` consumers.
fn publish_ripple_reveal(
    state: &mut GameState,
    controller: PlayerId,
    revealed: &[ObjectId],
    events: &mut Vec<GameEvent>,
) {
    if revealed.is_empty() {
        return;
    }
    state
        .resolve_and_apply_information(
            revealed,
            ResolvedInformationAudience::Controller(controller),
            ResolvedInformationLifetime::UntilActionBoundary,
            ResolvedInformationEdit::Reveal,
        )
        .expect("resolved ripple reveal occurrences must be live and distinct");
    state
        .resolve_and_apply_information(
            revealed,
            ResolvedInformationAudience::Public,
            ResolvedInformationLifetime::UntilZoneChange,
            ResolvedInformationEdit::Reveal,
        )
        .expect("published ripple reveal occurrences must be live and distinct");

    let card_names: Vec<String> = revealed
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: controller,
        card_ids: revealed.to_vec(),
        card_names,
    });
    state.last_revealed_ids = revealed.to_vec();
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

    /// Run `resolve` (which opens the optional-reveal prompt) then accept it,
    /// mirroring the `(RippleRevealChoice, RippleChoice::Cast)` engine handler.
    fn resolve_and_accept(
        state: &mut GameState,
        source_id: ObjectId,
        count: u32,
        events: &mut Vec<GameEvent>,
    ) {
        let ability =
            ResolvedAbility::new(Effect::Ripple { count }, vec![], source_id, PlayerId(0));
        resolve(state, &ability, events).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::RippleRevealChoice { count: c, .. } if c == count),
            "resolve must open the optional-reveal prompt, got {:?}",
            state.waiting_for
        );
        perform_reveal_and_offer(state, source_id, count, events);
    }

    /// CR 702.60a: `resolve` opens the "you may reveal" decision, carrying N.
    #[test]
    fn resolve_opens_optional_reveal_prompt() {
        let (mut state, source_id) = setup("Surging Flame");
        let a = add_library_card(&mut state, "Mountain");
        let b = add_library_card(&mut state, "Mountain");
        state.players[0].library = im::vector![a, b];

        let ability =
            ResolvedAbility::new(Effect::Ripple { count: 4 }, vec![], source_id, PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::RippleRevealChoice {
                player: PlayerId(0),
                count: 2, // clamped to the two-card library
                ..
            }
        ));
        // CR 701.20b: nothing revealed or published yet.
        assert!(state.revealed_cards.is_empty());
        for id in [a, b] {
            assert_eq!(state.objects.get(&id).map(|o| o.zone), Some(Zone::Library));
        }
    }

    /// CR 702.60a: a same-named card in the top N is offered for a free cast.
    /// CR 701.20b: the revealed cards stay in the library and are published to
    /// every viewer for the duration of the offer.
    #[test]
    fn offers_same_named_revealed_card() {
        let (mut state, source_id) = setup("Surging Flame");
        let other = add_library_card(&mut state, "Mountain");
        let match_card = add_library_card(&mut state, "Surging Flame");
        state.players[0].library = im::vector![other, match_card];

        let mut events = Vec::new();
        resolve_and_accept(&mut state, source_id, 2, &mut events);

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Ripple {
                        hit_card,
                        remaining_hits,
                        revealed_misses,
                        ..
                    },
                ..
            } => {
                assert_eq!(*hit_card, match_card);
                assert!(remaining_hits.is_empty());
                assert_eq!(revealed_misses, &vec![other]);
            }
            other => panic!("expected Ripple CastOffer, got {other:?}"),
        }

        // CR 701.20b: no card moved — both are still in the library.
        for id in [other, match_card] {
            assert_eq!(state.objects.get(&id).map(|o| o.zone), Some(Zone::Library));
        }
        // CR 701.20a: both are publicly revealed while the offer is open.
        assert!(state.revealed_cards.contains(&other));
        assert!(state.revealed_cards.contains(&match_card));
        // CR 701.20a: one event carries the whole revealed pile, top-first.
        let revealed_event = events
            .iter()
            .find_map(|e| match e {
                GameEvent::CardsRevealed {
                    card_ids, player, ..
                } => Some((player, card_ids)),
                _ => None,
            })
            .expect("Ripple emits a CardsRevealed event");
        assert_eq!(*revealed_event.0, PlayerId(0));
        assert_eq!(revealed_event.1, &vec![other, match_card]);
    }

    /// CR 702.60a: all same-named cards revealed by one ripple remain eligible.
    #[test]
    fn offers_all_same_named_revealed_cards_before_misses() {
        let (mut state, source_id) = setup("Surging Flame");
        let first_match = add_library_card(&mut state, "Surging Flame");
        let miss = add_library_card(&mut state, "Mountain");
        let second_match = add_library_card(&mut state, "Surging Flame");
        state.players[0].library = im::vector![first_match, miss, second_match];

        resolve_and_accept(&mut state, source_id, 3, &mut Vec::new());

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Ripple {
                        hit_card,
                        remaining_hits,
                        revealed_misses,
                        ..
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

    /// CR 702.60a + CR 608.2d: with no same-named card and 2+ revealed cards,
    /// the reveal is published and the controller is prompted for the bottom
    /// order (`WaitingFor::RippleBottomOrder`).
    #[test]
    fn no_match_opens_bottom_order_prompt() {
        let (mut state, source_id) = setup("Surging Might");
        let a = add_library_card(&mut state, "Forest");
        let b = add_library_card(&mut state, "Bear");
        state.players[0].library = im::vector![a, b];

        let mut events = Vec::new();
        resolve_and_accept(&mut state, source_id, 2, &mut events);

        match &state.waiting_for {
            WaitingFor::RippleBottomOrder {
                player,
                cards,
                final_cast,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards, &vec![a, b]);
                assert!(final_cast.is_none());
            }
            other => panic!("expected RippleBottomOrder, got {other:?}"),
        }
        // CR 701.20b: still in the library, and publicly revealed.
        for id in [a, b] {
            assert_eq!(state.objects.get(&id).map(|o| o.zone), Some(Zone::Library));
            assert!(state.revealed_cards.contains(&id));
        }
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids == &vec![a, b]
        )));
    }

    /// CR 702.60a: an empty library has nothing to reveal — `resolve` opens no
    /// prompt and completes.
    #[test]
    fn empty_library_no_prompt() {
        let (mut state, source_id) = setup("Surging Aether");
        state.players[0].library.clear();
        let before = state.waiting_for.clone();

        let ability =
            ResolvedAbility::new(Effect::Ripple { count: 1 }, vec![], source_id, PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert_eq!(state.waiting_for, before);
        assert!(state.revealed_cards.is_empty());
    }
}
