use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::static_abilities::prohibition_scope_matches_player;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::statics::StaticMode;
#[cfg(test)]
use crate::types::zones::Zone;

pub(crate) fn allowed_draw_count(
    state: &GameState,
    player_id: crate::types::player::PlayerId,
    count: u32,
) -> u32 {
    let Some(player) = state.players.iter().find(|p| p.id == player_id) else {
        return 0;
    };

    let mut allowed = count;
    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the phased-out /
    // command-zone / condition gate.
    for (source_obj, def) in crate::game::functioning_abilities::battlefield_active_statics(state) {
        let source_id = source_obj.id;

        {
            match def.mode {
                StaticMode::CantDraw { ref who }
                    if prohibition_scope_matches_player(who, player_id, source_id, state) =>
                {
                    return 0;
                }
                StaticMode::PerTurnDrawLimit { ref who, max }
                    if prohibition_scope_matches_player(who, player_id, source_id, state) =>
                {
                    let remaining = max.saturating_sub(player.cards_drawn_this_turn);
                    allowed = allowed.min(remaining);
                }
                _ => {}
            }
        }
    }

    allowed
}

/// CR 121.1 + CR 613.11: True when an active `DrawFromBottom` static redirects
/// `player_id`'s draws to the bottom of their library. Mirrors the
/// `battlefield_active_statics` scan in [`allowed_draw_count`].
pub(crate) fn draws_from_bottom(
    state: &GameState,
    player_id: crate::types::player::PlayerId,
) -> bool {
    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the phased-out /
    // command-zone / condition gate.
    for (source_obj, def) in crate::game::functioning_abilities::battlefield_active_statics(state) {
        if let StaticMode::DrawFromBottom { ref who } = def.mode {
            if prohibition_scope_matches_player(who, player_id, source_obj.id, state) {
                return true;
            }
        }
    }
    false
}

/// CR 121.1 + CR 121.2 + CR 613.11: SINGLE AUTHORITY for which library cards a
/// draw pulls. Every draw-delivery path (spell/ability resolution, the
/// turn-based draw step, connive, gift) MUST call this for card selection so a
/// `DrawFromBottom` static is honored uniformly.
///
/// Returns up to `count` object ids, pulled from the BOTTOM (CR 121.2: cards are
/// drawn one at a time, each taking the then-current bottommost card →
/// `.rev().take(n)`) when an active `DrawFromBottom` matches the player,
/// otherwise from the TOP (CR 121.1, `library[0]`). Partial draws
/// (`count > library.len()`) return all available ids; an empty library returns
/// an empty vec — empty-library SBA handling (CR 704.5b) stays at the call site.
pub(crate) fn select_cards_to_draw(
    state: &GameState,
    player_id: crate::types::player::PlayerId,
    count: usize,
) -> Vec<crate::types::identifiers::ObjectId> {
    let Some(player) = state.players.iter().find(|p| p.id == player_id) else {
        return Vec::new();
    };
    if draws_from_bottom(state, player_id) {
        player.library.iter().rev().take(count).copied().collect()
    } else {
        player.library.iter().take(count).copied().collect()
    }
}

/// CR 121.1: Draw a card — put the top card of library into hand.
///
/// CR 601.2c + CR 115.1: When the parsed `Effect::Draw { target }` is a
/// player-target filter (e.g. `TargetFilter::Player` from "Target player draws
/// a card"), the drawing player is whichever `TargetRef::Player` was chosen
/// during spell announcement. `ResolvedAbility::target_player()` extracts
/// that choice and falls back to `ability.controller` when the target is a
/// context-ref (Controller, SelfRef, etc.) — preserving the historical
/// "controller draws" behavior for plain "draw a card" / "you draw" patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_cards, drawing_player) = match &ability.effect {
        // CR 107.1b: Resolve with full ability context so `QuantityRef::Variable { "X" }`
        // finds the caster-chosen X on the ability.
        // CR 601.2c: For `target: TargetFilter::Player`, the drawing player was
        // chosen during spell announcement and is in `ability.targets` —
        // `target_player()` reads it back, falling back to controller for
        // context-ref filters that don't surface a target slot.
        // CR 608.2d: "Draw up to N" is encoded as `count: UpTo { max }`.
        // Generic resolution sees `UpTo` transparently as `max`, so this
        // call already returns the upper-bound count. By the time we reach
        // here the engine has already resolved the chosen count via the
        // player choice mechanism in `engine_resolution_choices` and
        // baked it into the ProposedEvent::Draw count.
        Effect::Draw { count, target } => (
            // CR 107.1b: a calculation yielding a negative number uses zero
            // instead. Clamp before the `as u32` cast — an unclamped negative
            // (e.g. Mr. Foxglove when the defender's hand is smaller than the
            // controller's) would wrap to ~4 billion and draw the whole library.
            resolve_quantity_with_targets(state, count, ability).max(0) as u32,
            // CR 121.1 + CR 615.5 + CR 609.7: context-ref target filters
            // (PostReplacementSourceController, ParentTargetController, etc.)
            // resolve via state slots — falling straight to `ability.controller`
            // would draw cards for the wrong player on prevention follow-ups
            // like Swans of Bryn Argoll.
            super::resolve_player_for_context_ref(state, ability, target),
        ),
        _ => (1, ability.controller),
    };

    // CR 614.1a: Route draw through replacement pipeline (e.g. Dredge, Abundance).
    match draw_through_replacement(
        state,
        drawing_player,
        num_cards,
        events,
        apply_draw_after_replacement,
    ) {
        ReplacementResult::Execute(_) | ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(_) => return Ok(()),
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 614.6 + CR 614.11 + CR 704.3: Single authority for the
/// "propose Draw → replace → apply → drain post-replacement continuation"
/// sequence. Every site that proposes a `ProposedEvent::Draw` MUST call this
/// helper — otherwise a substituted mandatory-post-effect (Jace WinTheGame,
/// Abundance reveal-until) leaks past the resolution step and drains against
/// the wrong player on a later priority pass.
///
/// `apply_executed` is invoked on the `Execute` arm with the (possibly
/// pre-zeroed by `apply_single_replacement`) replaced event, so callers can
/// layer their own bookkeeping — miracle tracking (`effects/draw.rs`,
/// `effects/connive.rs`, `effects/gift_delivery.rs`), draw-step
/// `has_drawn_this_turn` flag (`turns.rs`), or the chain's discard step
/// (connive). The continuation drain runs immediately after `apply_executed`
/// returns, inside the same resolution step so SBAs (CR 704.5b
/// draw-from-empty-library loss) and priority never fall between the
/// (possibly pre-zeroed) draw and its substitute.
///
/// On `NeedsChoice`, sets `state.waiting_for` to the replacement-choice
/// prompt before returning so callers only need to bail. On `Prevented`,
/// `apply_executed` is not called.
pub(crate) fn draw_through_replacement(
    state: &mut GameState,
    player_id: crate::types::player::PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
    apply_executed: impl FnOnce(&mut GameState, ProposedEvent, &mut Vec<GameEvent>),
) -> replacement::ReplacementResult {
    let proposed = ProposedEvent::Draw {
        player_id,
        count,
        applied: HashSet::new(),
    };
    let result = replacement::replace_event(state, proposed, events);
    match &result {
        ReplacementResult::Execute(event) => {
            apply_executed(state, event.clone(), events);
            if state.post_replacement_continuation.is_some() {
                let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
                    state, None, None, None, events,
                );
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(*player, state);
        }
    }
    result
}

/// CR 121.1: Apply a post-replacement `ProposedEvent::Draw` to the game state.
///
/// Extracted from `resolve`'s Execute arm so the same logic can be invoked by
/// `handle_replacement_choice` when a player accepts a draw-replacement choice.
/// Caller is responsible for emitting `EffectResolved`.
pub fn apply_draw_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) {
    let ProposedEvent::Draw {
        player_id,
        count,
        applied,
    } = event
    else {
        debug_assert!(
            false,
            "apply_draw_after_replacement called with non-Draw ProposedEvent"
        );
        return;
    };

    let allowed_count = allowed_draw_count(state, player_id, count);
    // CR 121.1 + CR 613.11: card selection routes through the single
    // `select_cards_to_draw` authority so a `DrawFromBottom` static is honored.
    let cards_to_draw = select_cards_to_draw(state, player_id, allowed_count as usize);

    // CR 704.5b: If library has fewer cards than requested, mark the player.
    // CR 121.4: Partial draws are legal — draw what's available.
    if allowed_count > 0 && cards_to_draw.len() < allowed_count as usize {
        if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
            player.drew_from_empty_library = true;
        }
    }

    // CR 609.3: Record the actually-drawn count so chained sub-abilities like
    // "draw cards equal to N, then discard that many" can resolve their
    // dynamic count via `EventContextAmount`'s `last_effect_count` fallback.
    // Mirrors the convention used by `change_zone.rs`, `sacrifice.rs`, etc.
    let drawn_count = cards_to_draw.len() as i32;
    state.last_effect_count = Some(drawn_count);

    for obj_id in cards_to_draw {
        // CR 121.1 (PLAN Risk #5, tranche 4): drawing IS a Library → Hand zone
        // change, so the per-card delivery routes through the unified pipeline
        // (`move_object` via `ZoneMoveRequest::draw`) with the inner `Moved`
        // consult ENABLED — a future "cards you would draw go to exile instead"
        // redirect must see this move. The raw `zones::move_to_zone` bypass that
        // previously lived here was the exact debt the pipeline exists to
        // eliminate; "consults nothing in the current pool" did not justify a
        // permanent bypass that a future `Moved` def would silently miss.
        //
        // CR 614.5 dedup guard: a replacement effect gets only one opportunity to
        // affect an event "or any modified events that may replace that event".
        // The outer `ReplacementEvent::Draw` pass already ran (upstream of this
        // delivery), and its applied-`ReplacementId` set rides in `applied`.
        // Seeding it into the inner `Moved` consult (`ZoneMoveRequest::draw(obj_id,
        // applied.clone())`) stops a def that matches BOTH the Draw class and the
        // Moved class from firing twice — `find_applicable_replacements` skips any
        // rid already in the seeded set (`already_applied`). One `applied` snapshot
        // per draw event; cloned per card because each delivered card is a
        // distinct zone-change event.
        let _ = crate::game::zone_pipeline::move_object(
            state,
            crate::game::zone_pipeline::ZoneMoveRequest::draw(obj_id, applied.clone()),
            events,
        );
        // CR 616.1 + OQ#1 (audit-justified non-interactivity; delivery
        // post-condition): no production `Moved` def can match a Library → Hand
        // move today (every destination-unconstrained `Moved` def is `valid_card:
        // SelfRef`-bound to a battlefield host; the only `valid_card: None` class
        // — Rest in Peace / Leyline "put into a graveyard → exile" — is
        // destination-gated to Graveyard), so a draw cannot surface a CR 616.1
        // ordering choice and no `pending_batch_deliveries` resume is wired.
        //
        // The assert catches the MECHANICAL non-delivery bug: if the card is
        // still in the library, the move stranded — `move_object` returned a
        // swallowed `NeedsChoice` (the tranche-3 hazard: parked
        // `pending_replacement` overwritten by the next card, truncating SBAs)
        // or otherwise failed — yet `CardDrawn` + the draw counters fire below as
        // if it were delivered. A legitimate future redirect (card → graveyard /
        // exile) is intentionally NOT asserted against: forbidding it here would
        // break the very consult the migration enables. The OPEN question a
        // future migrator must settle when a real to-Hand/redirect `Moved` def is
        // added: whether a move-level-redirected card still counts as "drawn" (the
        // `CardDrawn` emission + counter increments below currently assume yes),
        // and — if the redirect can be interactive — route this loop through
        // `move_objects_simultaneously_then` + a `BatchCompletion` (OQ#1: extend
        // the shared batch machinery, never add a per-flow pause).
        debug_assert!(
            state
                .objects
                .get(&obj_id)
                .is_none_or(|o| o.zone != crate::types::zones::Zone::Library),
            "draw delivery stranded the card in the library — move_object returned \
             a swallowed NeedsChoice or otherwise failed to deliver, yet CardDrawn \
             and the draw counters fire below"
        );
        // CR 121.1 + CR 504.1: Increment per-step + per-turn counters BEFORE
        // emitting the event so the ordinal embedded in `CardDrawn` reflects
        // this draw (1-indexed). Triggers/replacements that gate on "first
        // draw of the draw step" read this ordinal.
        let (nth_in_turn, nth_in_step) =
            if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
                player.cards_drawn_this_turn = player.cards_drawn_this_turn.saturating_add(1);
                player.cards_drawn_this_step = player.cards_drawn_this_step.saturating_add(1);
                (player.cards_drawn_this_turn, player.cards_drawn_this_step)
            } else {
                (1, 1)
            };
        events.push(GameEvent::CardDrawn {
            player_id,
            object_id: obj_id,
            nth_in_turn,
            nth_in_step,
        });
        super::drawn_this_turn_choice::record_drawn_card(state, player_id, obj_id);
        record_first_draw_and_enqueue_miracle(state, player_id, obj_id);
    }
}

/// CR 702.94a + CR 603.11: Shared first-draw hook — record the drawn
/// `ObjectId` as `player`'s first-of-turn if absent, and if the drawn card has
/// `Keyword::Miracle(cost)`, enqueue a `MiracleOffer` for the priority-entry
/// flush to surface as `WaitingFor::MiracleReveal`. Subsequent draws do NOT
/// overwrite the first-draw entry and do NOT enqueue more offers (the static
/// ability only functions for the first-drawn card per CR 702.94a).
pub(crate) fn record_first_draw_and_enqueue_miracle(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    object_id: crate::types::identifiers::ObjectId,
) {
    // Only the FIRST draw of the turn per player establishes the miracle
    // eligibility condition. `or_insert_with` returns a `&mut V` indicating
    // whether the entry was freshly set; compare against `object_id` to know.
    let is_first = !state.first_card_drawn_this_turn.contains_key(&player);
    state
        .first_card_drawn_this_turn
        .entry(player)
        .or_insert(object_id);
    if !is_first {
        return;
    }
    // CR 702.94a: Static ability functions from hand — check the drawn object's
    // keywords. Reads the concrete `Keyword::Miracle(ManaCost)` payload; if
    // layers remove miracle before draw resolution the offer simply never
    // queues. `obj.keywords` is the printed+granted keyword set visible on the
    // object while it's in the owner's hand.
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };
    if obj.owner != player {
        return;
    }
    let miracle_cost = obj.keywords.iter().find_map(|k| match k {
        crate::types::keywords::Keyword::Miracle(cost) => Some(cost.clone()),
        _ => None,
    });
    let Some(cost) = miracle_cost else {
        return;
    };
    state
        .pending_miracle_offers
        .push(crate::types::game_state::MiracleOffer {
            player,
            object_id,
            cost,
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, ReplacementDefinition, StaticDefinition,
        SubAbilityLink, TargetFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::ProhibitionScope;

    fn make_ability(num_cards: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed {
                    value: num_cards as i32,
                },
                target: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn draw_moves_top_card_to_hand() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(1);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&card_id));
        assert!(!state.players[0].library.contains(&card_id));
    }

    #[test]
    fn draw_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(2);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&c1));
        assert!(state.players[0].hand.contains(&c2));
    }

    #[test]
    fn draw_emits_card_drawn_and_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::CardDrawn { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Draw,
                ..
            }
        )));
    }

    #[test]
    fn draw_from_empty_library_sets_flag() {
        let mut state = GameState::new_two_player(42);
        // Library is empty — drawing should set the flag
        let mut events = Vec::new();

        let ability = make_ability(1);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.players[0].drew_from_empty_library,
            "Drawing from empty library should set flag"
        );
    }

    #[test]
    fn partial_draw_sets_flag() {
        let mut state = GameState::new_two_player(42);
        // Library has 1 card, but we draw 3 — partial draw, flag should be set
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(3);
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have drawn the 1 card available
        assert_eq!(state.players[0].hand.len(), 1);
        // But flag should be set because library couldn't fulfill the full draw
        assert!(
            state.players[0].drew_from_empty_library,
            "Partial draw should set flag"
        );
    }

    #[test]
    fn normal_draw_does_not_set_flag() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(1);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.players[0].drew_from_empty_library,
            "Normal draw should not set flag"
        );
    }

    #[test]
    fn teferi_ageless_insight_preserves_sub_ability_discard() {
        // Regression test for issue #1964: Teferi's Ageless Insight replacement
        // ("draw two cards instead") should not remove the discard sub_ability from
        // Temmet, Naktamun's Will's attack trigger ("draw a card, then discard a card").
        let mut state = GameState::new_two_player(42);

        let teferi = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Ageless Insight".to_string(),
            Zone::Battlefield,
        );
        let teferi_obj = state.objects.get_mut(&teferi).unwrap();
        teferi_obj.replacement_definitions = vec![ReplacementDefinition::new(
            ReplacementEvent::Draw,
        )
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
        ))]
        .into();

        for card_id in 2..=4 {
            create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Card {card_id}"),
                Zone::Library,
            );
        }

        let mut resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        resolved.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                filter: None,
                selection: crate::types::ability::CardSelectionMode::Random,
                unless_filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));
        if let Some(ref mut sub) = resolved.sub_ability {
            sub.sub_link = SubAbilityLink::ContinuationStep;
        }

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            1,
            "Should draw 2 then discard 1"
        );
        assert_eq!(state.players[0].graveyard.len(), 1, "Should discard 1 card");
    }

    #[test]
    fn cant_draw_blocks_all_draws_for_affected_player() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Omen Machine".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantDraw {
                who: ProhibitionScope::AllPlayers,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert!(state.players[0].hand.is_empty());
        assert_eq!(state.players[0].library.len(), 1);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { .. })));
    }

    #[test]
    fn cant_draw_opponents_only_does_not_block_controller() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Narset".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantDraw {
                who: ProhibitionScope::Opponents,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn per_turn_draw_limit_allows_partial_multi_card_draw() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Spirit of the Labyrinth".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::AllPlayers,
                max: 1,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].cards_drawn_this_turn, 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CardDrawn { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn per_turn_draw_limit_ignores_unaffected_player() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Narset".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::Opponents,
                max: 1,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].cards_drawn_this_turn, 1);
    }

    /// CR 702.94a + CR 603.11: First card drawn per turn is recorded so the
    /// miracle reveal prompt can gate eligibility. Subsequent draws do NOT
    /// overwrite the recorded ObjectId.
    #[test]
    fn first_card_drawn_this_turn_records_only_the_first() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let _second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );

        // Pre-condition: no first-draw recorded yet.
        assert!(!state.first_card_drawn_this_turn.contains_key(&PlayerId(0)));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        // Post-condition: only the first drawn object is recorded.
        assert_eq!(
            state.first_card_drawn_this_turn.get(&PlayerId(0)),
            Some(&first),
            "first_card_drawn_this_turn should record the first drawn ObjectId and not overwrite",
        );
    }

    /// CR 702.94a: A second resolve() call in the same turn does NOT update
    /// the recorded first-drawn ObjectId — the entry is set on the very first
    /// draw of the turn and stable until the turn reset clears it.
    #[test]
    fn first_card_drawn_this_turn_stable_across_draw_calls() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let _second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert_eq!(
            state.first_card_drawn_this_turn.get(&PlayerId(0)),
            Some(&first),
            "second draw this turn must not overwrite the first-draw entry",
        );
    }

    /// CR 702.94a + CR 603.11: A card with Miracle drawn as the first card of
    /// the turn queues a `MiracleOffer` with the keyword's mana cost. A second
    /// draw of another miracle card in the same resolution does NOT queue a
    /// second offer (CR 702.94a only honors the first-drawn card).
    #[test]
    fn miracle_first_draw_queues_offer() {
        use crate::types::mana::{ManaCost, ManaCostShard};
        let mut state = GameState::new_two_player(42);
        // Put two miracle-tagged cards on the library top.
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "MiracleOne".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "MiracleTwo".to_string(),
            Zone::Library,
        );
        // Attach Keyword::Miracle({W}) to each.
        for obj_id in [first, second] {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords
                .push(crate::types::keywords::Keyword::Miracle(ManaCost::Cost {
                    shards: vec![ManaCostShard::White],
                    generic: 0,
                }));
            obj.base_keywords = obj.keywords.clone();
        }

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        // Only the first drawn card queues a miracle offer.
        assert_eq!(
            state.pending_miracle_offers.len(),
            1,
            "only the first drawn card should queue a miracle offer"
        );
        let offer = &state.pending_miracle_offers[0];
        assert_eq!(offer.player, PlayerId(0));
        assert_eq!(offer.object_id, first);
    }

    /// CR 702.94a: A card without Miracle as the first-drawn card does NOT
    /// queue an offer, even if later drawn cards have Miracle.
    #[test]
    fn miracle_non_first_draw_does_not_queue_offer() {
        use crate::types::mana::{ManaCost, ManaCostShard};
        let mut state = GameState::new_two_player(42);
        // First card: no miracle. Second card: miracle.
        let _first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mundane".to_string(),
            Zone::Library,
        );
        let miracle_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "MiracleCard".to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&miracle_card).unwrap();
        obj.keywords
            .push(crate::types::keywords::Keyword::Miracle(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            }));
        obj.base_keywords = obj.keywords.clone();

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        assert!(
            state.pending_miracle_offers.is_empty(),
            "non-first-drawn miracle card must not queue an offer"
        );
    }

    /// Seed a player's library in deterministic top→bottom order. `create_object`
    /// appends (push_back), and `library[0]` is the top, so the first name is the
    /// top card and the last name is the bottom card.
    fn seed_library(state: &mut GameState, player: PlayerId, names: &[&str]) -> Vec<ObjectId> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                create_object(
                    state,
                    CardId(1000 + i as u64),
                    player,
                    name.to_string(),
                    Zone::Library,
                )
            })
            .collect()
    }

    fn push_draw_from_bottom(state: &mut GameState, controller: PlayerId, who: ProhibitionScope) {
        let source = create_object(
            state,
            CardId(9000),
            controller,
            "River Song".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DrawFromBottom { who }));
    }

    /// CR 121.1: with no `DrawFromBottom` static, `select_cards_to_draw` pulls
    /// from the TOP (`library[0]`) in order; `count > len` returns all available.
    #[test]
    fn select_pulls_from_top_without_static() {
        let mut state = GameState::new_two_player(42);
        let lib = seed_library(&mut state, PlayerId(0), &["top", "mid", "bottom"]);

        assert!(!draws_from_bottom(&state, PlayerId(0)));
        assert_eq!(select_cards_to_draw(&state, PlayerId(0), 1), vec![lib[0]]);
        assert_eq!(
            select_cards_to_draw(&state, PlayerId(0), 2),
            vec![lib[0], lib[1]]
        );
        // count > len → all available, no panic.
        assert_eq!(select_cards_to_draw(&state, PlayerId(0), 99), lib);
    }

    /// CR 121.1 + CR 121.2: with a controller-scoped `DrawFromBottom` static,
    /// selection pulls from the BOTTOM one at a time (bottommost first, then
    /// next-from-bottom). Empty library returns an empty vec.
    #[test]
    fn select_pulls_from_bottom_with_controller_static() {
        let mut state = GameState::new_two_player(42);
        let lib = seed_library(&mut state, PlayerId(0), &["top", "mid", "bottom"]);
        push_draw_from_bottom(&mut state, PlayerId(0), ProhibitionScope::Controller);

        assert!(draws_from_bottom(&state, PlayerId(0)));
        // lib = [top, mid, bottom]; bottom is the last element.
        assert_eq!(select_cards_to_draw(&state, PlayerId(0), 1), vec![lib[2]]);
        assert_eq!(
            select_cards_to_draw(&state, PlayerId(0), 2),
            vec![lib[2], lib[1]]
        );

        state.players[0].library.clear();
        assert!(select_cards_to_draw(&state, PlayerId(0), 1).is_empty());
    }

    /// CR 613.11: `DrawFromBottom { Opponents }` redirects an opponent's draws
    /// but NOT the source-controller's — scope correctness across both players.
    #[test]
    fn select_scope_opponents_only() {
        let mut state = GameState::new_two_player(42);
        let p0_lib = seed_library(&mut state, PlayerId(0), &["p0top", "p0bottom"]);
        let p1_lib = seed_library(&mut state, PlayerId(1), &["p1top", "p1bottom"]);
        // Source controlled by P0, scoping its OPPONENTS (P1).
        push_draw_from_bottom(&mut state, PlayerId(0), ProhibitionScope::Opponents);

        // P1 (the opponent) draws from the bottom.
        assert!(draws_from_bottom(&state, PlayerId(1)));
        assert_eq!(
            select_cards_to_draw(&state, PlayerId(1), 1),
            vec![p1_lib[1]]
        );
        // P0 (the controller) is unaffected — top.
        assert!(!draws_from_bottom(&state, PlayerId(0)));
        assert_eq!(
            select_cards_to_draw(&state, PlayerId(0), 1),
            vec![p0_lib[0]]
        );
    }

    /// CR 121.1 + CR 121.2 + CR 613.11: the spell/ability draw path
    /// (`apply_draw_after_replacement`) honors `DrawFromBottom` — a draw-2 pulls
    /// the bottommost then next-from-bottom, leaving the top card in the library.
    #[test]
    fn spell_draw_pulls_bottom_under_static() {
        let mut state = GameState::new_two_player(42);
        let lib = seed_library(&mut state, PlayerId(0), &["top", "mid", "bottom"]);
        push_draw_from_bottom(&mut state, PlayerId(0), ProhibitionScope::Controller);

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        assert!(
            state.players[0].hand.contains(&lib[2]),
            "bottom card must be drawn first"
        );
        assert!(
            state.players[0].hand.contains(&lib[1]),
            "next-from-bottom must be drawn second"
        );
        assert!(
            state.players[0].library.contains(&lib[0]),
            "top card must remain in the library"
        );
    }
}

/// CR 121.1 + CR 614.5: tranche-4 draw-pipeline migration coverage. These drive
/// the REAL draw pipeline (`resolve` / `apply_draw_after_replacement` →
/// `zone_pipeline::move_object`), not hand-constructed expected state.
/// `draw_consult_runs_for_unseeded_moved_redirect` is the migration tripwire — it
/// installs an always-matching `Moved` redirect with an EMPTY seed and asserts the
/// drawn card is redirected to the graveyard, which fails under the old raw
/// `zones::move_to_zone` bypass (no consult) and under any regression that makes
/// `ZoneChangeCause::Draw` exempt. The dedup test then pins that a def already
/// applied at the Draw level is suppressed at the Moved level (CR 614.5), and the
/// graveyard-gated test pins the destination gate (CR 614.6).
#[cfg(test)]
mod tranche4_draw_pipeline_tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0};
    use crate::parser::oracle_replacement::parse_replacement_line;
    use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use crate::types::identifiers::ObjectId;
    use crate::types::proposed_event::ReplacementId;
    use crate::types::replacements::ReplacementEvent;

    fn draw_one_ability() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            P0,
        )
    }

    /// The audit's NEGATIVE case, end-to-end: a REAL parsed "If a card would be
    /// put into a graveyard from anywhere, exile it instead" (Leyline of the
    /// Void / Rest in Peace class) `Moved` def is `destination_zone:
    /// Graveyard`-gated, so it must NOT fire on a draw (a Library → Hand move).
    /// The drawn card lands in HAND untouched. This pins that routing the draw
    /// through `move_object`'s inner `Moved` consult does not let a
    /// graveyard-scoped redirect leak onto the draw delivery (CR 614.6
    /// destination-zone gate).
    #[test]
    fn draw_with_parsed_graveyard_exile_def_on_board_still_lands_in_hand() {
        let mut sc = GameScenario::new();
        let rip = sc.add_creature(P0, "Rest in Peace", 0, 0).id();
        let drawn = sc.add_card_to_library_top(P0, "Mountain");
        let mut state = sc.state;

        // Install the REAL parsed graveyard-exile redirect (destination_zone:
        // Graveyard) on a battlefield permanent.
        let def = parse_replacement_line(
            "If a card would be put into a graveyard from anywhere, exile it instead.",
            "Rest in Peace",
        )
        .expect("graveyard-exile replacement line must parse");
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(
            def.destination_zone,
            Some(Zone::Graveyard),
            "the graveyard-exile redirect must be destination-gated to Graveyard"
        );
        state
            .objects
            .get_mut(&rip)
            .unwrap()
            .replacement_definitions
            .push(def);

        let mut events = Vec::new();
        resolve(&mut state, &draw_one_ability(), &mut events).unwrap();

        // Discriminating assertion: the drawn card is in HAND, not Exile. If the
        // migration mis-routed the destination gate, the card would be exiled.
        assert_eq!(
            state.objects[&drawn].zone,
            Zone::Hand,
            "CR 614.6: a Graveyard-destination redirect must not fire on a \
             Library → Hand draw — the drawn card lands in hand"
        );
        assert!(state.players[0].hand.contains(&drawn));
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::CardDrawn { object_id, .. } if *object_id == drawn)
            ),
            "the migrated draw must still emit CardDrawn for the delivered card"
        );
    }

    /// CR 614.6 dedup guard (the heart of this tranche), discriminating: a
    /// destination-unconstrained `valid_card: None` `Moved` def that ALSO fired
    /// at the `ReplacementEvent::Draw` level must NOT fire again at the inner
    /// `Moved` delivery level. No parsed card produces a to-Hand `Moved` redirect
    /// today (audit), so this installs a synthetic always-matching `Moved` def
    /// and drives `apply_draw_after_replacement` with the def's `ReplacementId`
    /// pre-seeded into the Draw event's `applied` set. The seed is synthetic /
    /// forward-looking — a real Draw pass only deposits `ReplacementEvent::Draw`
    /// rids, and a `Moved` def's rid can never appear there (the registry
    /// dispatches by event), so this models the future case the guard is armor
    /// against rather than a production-reachable state. The guard threads that
    /// set into the
    /// per-card `ZoneMoveRequest::draw`, which seeds the inner consult's
    /// `applied`; the matcher's `already_applied(&rid)` skip then prevents the
    /// second application. The redirect would have sent the card to the graveyard
    /// — so the discriminating assertion is that the card lands in HAND (guard
    /// held), not Graveyard (guard absent → double-apply).
    #[test]
    fn dedup_guard_blocks_moved_def_already_applied_at_draw_level() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Dedup Source", 0, 0).id();
        let drawn = sc.add_card_to_library_top(P0, "Mountain");
        let mut state = sc.state;

        // A synthetic destination-unconstrained `Moved` redirect (Library/Hand →
        // Graveyard) on a battlefield permanent. `valid_card: None` matches every
        // card, including the drawn one; no `destination_zone` gate, so it WOULD
        // match the Library → Hand draw if not deduped.
        let redirect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Graveyard,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let def = crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(redirect)
            .description("synthetic always-match Moved".to_string());
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(def);
        // The def's ReplacementId is index 0 on `source`.
        let rid = ReplacementId { source, index: 0 };

        // Drive the apply path with the rid PRE-SEEDED into the Draw event's
        // applied set — modelling a def that already fired at the Draw level.
        let mut applied = std::collections::HashSet::new();
        applied.insert(rid);
        let mut events = Vec::new();
        apply_draw_after_replacement(
            &mut state,
            ProposedEvent::Draw {
                player_id: P0,
                count: 1,
                applied,
            },
            &mut events,
        );

        // Discriminating: guard held → card in HAND. Revert the seed-threading
        // and the redirect double-applies, sending it to the graveyard.
        assert_eq!(
            state.objects[&drawn].zone,
            Zone::Hand,
            "CR 614.6: a Moved def already applied at the Draw level must not \
             re-fire at the inner Moved delivery — the drawn card stays in hand"
        );
        assert!(state.players[0].graveyard.is_empty());
    }

    /// MIGRATION TRIPWIRE (positive discriminator): the SAME synthetic
    /// always-match `Moved` redirect, but with an EMPTY seed (nothing applied
    /// upstream), MUST be consulted by the migrated draw and redirect the drawn
    /// card to the graveyard. This is the assertion the dedup test cannot make:
    /// it fails under the old raw `zones::move_to_zone` bypass (no consult → card
    /// stays in hand) and under any regression that marks `ZoneChangeCause::Draw`
    /// exempt (consult skipped → card stays in hand). A single mandatory candidate,
    /// so `pipeline_loop` returns `Execute` with no CR 616.1 choice. CR 121.1:
    /// drawing is a replaceable Library → Hand zone change.
    #[test]
    fn draw_consult_runs_for_unseeded_moved_redirect() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Redirect Source", 0, 0).id();
        let drawn = sc.add_card_to_library_top(P0, "Mountain");
        let mut state = sc.state;

        let redirect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Graveyard,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let def = crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(redirect)
            .description("synthetic always-match Moved".to_string());
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(def);

        // EMPTY seed: nothing applied upstream, so the inner consult MUST fire on
        // the Library → Hand draw and redirect the card.
        let mut events = Vec::new();
        apply_draw_after_replacement(
            &mut state,
            ProposedEvent::Draw {
                player_id: P0,
                count: 1,
                applied: std::collections::HashSet::new(),
            },
            &mut events,
        );

        assert_eq!(
            state.objects[&drawn].zone,
            Zone::Graveyard,
            "CR 121.1: a draw routed through the pipeline must consult Moved defs — \
             an always-match redirect sends the drawn card to the graveyard. Hand \
             here means the consult did not run (raw-bypass or exempt-Draw regression)."
        );
        assert!(state.players[0].graveyard.contains(&drawn));
    }
}
