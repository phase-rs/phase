use rand::seq::SliceRandom;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, LibraryPosition, ResolvedAbility, RevealUntilDisposition,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::{EtbTapState, Zone};

/// CR 701.20a: Reveal cards from the top of the controller's library one at a
/// time until a card matching the filter is found. The matching card goes to
/// `kept_destination`, the remaining revealed cards go to `rest_destination`.
///
/// All revealed cards are marked as publicly revealed and a `CardsRevealed`
/// event is emitted. If the library is exhausted without finding a match, all
/// revealed cards go to `rest_destination`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (
        player_filter,
        filter,
        count_expr,
        matched_disposition,
        kept_destination,
        rest_destination,
        enter_tapped,
        enters_attacking,
        kept_optional_to,
        enters_under,
    ) = match &ability.effect {
        Effect::RevealUntil {
            player,
            filter,
            count,
            matched_disposition,
            kept_destination,
            rest_destination,
            enter_tapped,
            enters_attacking,
            kept_optional_to,
            enters_under,
        } => (
            player,
            filter,
            count,
            *matched_disposition,
            *kept_destination,
            *rest_destination,
            *enter_tapped,
            *enters_attacking,
            *kept_optional_to,
            enters_under.as_ref(),
        ),
        _ => return Err(EffectError::MissingParam("RevealUntil".to_string())),
    };

    // CR 701.20a + CR 608.2c: How many matching cards to reveal before the
    // until-loop terminates. The dominant `Fixed(1)` yields the historical
    // single-hit behavior; a dynamic count (e.g.
    // `DistinctColorsAmongPermanents` for Aurora Awakener / Sanar) resolves
    // against the live board with the ability in scope. CR 107.1b: a negative
    // computed count clamps to 0 (reveal nothing).
    let target_match_count =
        crate::game::quantity::resolve_quantity_with_targets(state, count_expr, ability).max(0)
            as usize;

    // CR 109.5 + CR 701.20a: Resolve which player's library is revealed.
    // `Controller` → activator (Jalira-style "you reveal..."); `ParentTargetController`
    // → controller of the parent ability's targeted object (Polymorph, Proteus Staff,
    // Transmogrify); other player-resolving filters → player extracted from
    // `ability.targets` (e.g., Telemin Performance "target opponent reveals...").
    let revealing_player = resolve_revealing_player(state, ability, player_filter);

    let player = state
        .players
        .iter()
        .find(|p| p.id == revealing_player)
        .ok_or(EffectError::PlayerNotFound)?;

    // Snapshot library (top = index 0) to iterate without borrow conflicts.
    let library: Vec<ObjectId> = player.library.iter().copied().collect();
    let mut revealed_misses: Vec<ObjectId> = Vec::new();
    let mut hit_card: Option<ObjectId> = None;

    // CR 107.3a + CR 601.2b: Evaluate the filter with the ability in scope so
    // dynamic thresholds (e.g. `Variable("X")`) resolve correctly.
    let ctx = FilterContext::from_ability(ability);

    // CR 701.20a + CR 608.2c: "reveal until you reveal X [filter] cards. Put any
    // number of those [filter] cards onto [kept_destination], then put the rest
    // of the revealed cards [in rest_destination]" (Aurora Awakener). Reveal
    // until `target_match_count` matches are found (or the library is exhausted),
    // then offer a `WaitingFor::DigChoice` over the matched set. Handled before
    // the single-hit loop below so the `KeepEach` path is untouched.
    if matches!(matched_disposition, RevealUntilDisposition::ChooseAnyNumber) {
        return resolve_choose_any_number(
            state,
            ability,
            revealing_player,
            &library,
            filter,
            &ctx,
            target_match_count,
            kept_destination,
            rest_destination,
            enter_tapped,
            events,
        );
    }

    // CR 701.20a: Reveal cards one at a time.
    for &card_id in &library {
        // Mark as revealed (CR 701.20b: card stays in library zone during reveal).
        state.revealed_cards.insert(card_id);

        if matches_target_filter(state, card_id, filter, &ctx) {
            hit_card = Some(card_id);
            break;
        } else {
            revealed_misses.push(card_id);
        }
    }

    // Build the full list of revealed card IDs for the event.
    let mut all_revealed: Vec<ObjectId> = revealed_misses.clone();
    if let Some(hit) = hit_card {
        all_revealed.push(hit);
    }

    // Emit CardsRevealed for all revealed cards.
    let card_names: Vec<String> = all_revealed
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: revealing_player,
        card_ids: all_revealed.clone(),
        card_names,
    });

    // Store revealed IDs for downstream reference.
    state.last_revealed_ids = all_revealed;

    // CR 701.20a + CR 608.2c: "You may put that card onto the battlefield" — when
    // the kept destination is a controller choice and a hit was found, pause for
    // `WaitingFor::RevealUntilKeptChoice`. The choice handler routes the hit card,
    // moves the misses, and drains `pending_continuation`. `EffectResolved` is
    // emitted here (before the pause) mirroring `discover::resolve`.
    if let (Some(accept_zone), Some(hit)) = (kept_optional_to, hit_card) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::RevealUntil,
            source_id: ability.source_id,
        });
        state.waiting_for = WaitingFor::RevealUntilKeptChoice {
            player: revealing_player,
            hit_card: hit,
            source_id: ability.source_id,
            accept_zone,
            decline_zone: kept_destination,
            enter_tapped,
            enters_attacking,
            revealed_misses,
            rest_destination,
        };
        return Ok(());
    }

    // Move the matching card to its destination.
    if let Some(hit) = hit_card {
        let controller_override = super::change_zone::resolve_enters_under_player(
            state,
            ability,
            "RevealUntil",
            enters_under,
        )?;
        match kept_destination {
            Zone::Battlefield => {
                // CR 614.1c + CR 306.5b / CR 310.4b: route the battlefield entry
                // through the zone-change pipeline so the full delivery tail runs
                // — intrinsic enters-with counters (a revealed planeswalker /
                // battle must enter with its loyalty / defense or it dies to
                // CR 704.5i), enters-with-counters statics, and the CR 614.1
                // tap-state. The pipeline applies `enter_tapped` from the seeded
                // `EntryMods`, so the previous manual `obj.tapped = true` is
                // dropped (it would double the work the tail already does).
                let mut req = ZoneMoveRequest::effect(hit, Zone::Battlefield, ability.source_id);
                req.mods.enter_tapped = enter_tapped;
                if let Some(controller) = controller_override {
                    req = req.under_control_of(controller);
                }
                match zone_pipeline::move_object(state, req, events) {
                    ZoneMoveResult::Done => {}
                    // CR 303.4f / CR 616.1: the kept card's battlefield entry
                    // paused on an as-enters choice (aura host pick / replacement
                    // ordering). The pause is parked centrally by `move_object`;
                    // defer the rest-pile move + reveal-marker cleanup onto the
                    // batch tail so the drain runs it once the entry resolves —
                    // otherwise the misses strand in their zone (the early-`return`
                    // bug). `EffectResolved` is emitted by the completion's
                    // continuation drain, not here, so the prompt is not clobbered.
                    ZoneMoveResult::NeedsChoice(_) | ZoneMoveResult::NeedsAuraAttachmentChoice => {
                        let mut clear_markers = revealed_misses.clone();
                        clear_markers.push(hit);
                        zone_pipeline::defer_completion_on_pause(
                            state,
                            BatchCompletion::RevealRestPile {
                                player: revealing_player,
                                rest_cards: revealed_misses,
                                rest_destination,
                                clear_markers,
                                publish_tracked_set: None,
                                emit_reveal_until_resolved: Some(ability.source_id),
                            },
                        );
                        return Ok(());
                    }
                }
                // CR 508.4: "put that card onto the battlefield tapped and
                // attacking" — place it in combat alongside the trigger source
                // (Raph & Mikey, Fireflux Squad). `enter_attacking` derives the
                // defending player from the source attacker.
                if enters_attacking {
                    let controller = state
                        .objects
                        .get(&hit)
                        .map(|obj| obj.controller)
                        .unwrap_or(ability.controller);
                    crate::game::combat::enter_attacking(state, hit, ability.source_id, controller);
                }
            }
            Zone::Library => {
                // CR 614.6 + CR 701.24a: a kept card sent back to the library
                // keeps the effect's historical bottom placement; this is a
                // placement, not a shuffle. Route through the placement-aware
                // pipeline arm so a future Library-destination `Moved` replacement
                // can still fire.
                match zone_pipeline::move_object(
                    state,
                    ZoneMoveRequest::effect(hit, Zone::Library, ability.source_id)
                        .at_library_position(LibraryPosition::Bottom),
                    events,
                ) {
                    ZoneMoveResult::Done => {}
                    ZoneMoveResult::NeedsChoice(_) | ZoneMoveResult::NeedsAuraAttachmentChoice => {
                        let mut clear_markers = revealed_misses.clone();
                        clear_markers.push(hit);
                        zone_pipeline::defer_completion_on_pause(
                            state,
                            BatchCompletion::RevealRestPile {
                                player: revealing_player,
                                rest_cards: revealed_misses,
                                rest_destination,
                                clear_markers,
                                publish_tracked_set: None,
                                emit_reveal_until_resolved: Some(ability.source_id),
                            },
                        );
                        return Ok(());
                    }
                }
            }
            other => {
                // CR 614.6: a kept card sent to another zone routes through the
                // pipeline so a matching `Moved` redirect can fire. On a CR 616.1
                // ordering pause, defer the rest-pile move + marker clear +
                // `EffectResolved` onto a `RevealRestPile` completion (the same
                // deferral the battlefield branch uses) so the misses don't strand
                // and `EffectResolved` doesn't land over the parked prompt.
                match zone_pipeline::move_object(
                    state,
                    ZoneMoveRequest::effect(hit, other, ability.source_id),
                    events,
                ) {
                    ZoneMoveResult::Done => {}
                    ZoneMoveResult::NeedsChoice(_) | ZoneMoveResult::NeedsAuraAttachmentChoice => {
                        let mut clear_markers = revealed_misses.clone();
                        clear_markers.push(hit);
                        zone_pipeline::defer_completion_on_pause(
                            state,
                            BatchCompletion::RevealRestPile {
                                player: revealing_player,
                                rest_cards: revealed_misses,
                                rest_destination,
                                clear_markers,
                                publish_tracked_set: None,
                                emit_reveal_until_resolved: Some(ability.source_id),
                            },
                        );
                        return Ok(());
                    }
                }
            }
        }
    }

    // CR 701.20a + CR 614.6: move the rest pile to its destination through the
    // zone-change pipeline so a per-card `Moved` graveyard→exile redirect (Rest
    // in Peace / Leyline of the Void) fires on each rest card — the 12
    // `rest_destination: Graveyard` reveal-until cards (Mind Funeral class)
    // previously dropped that redirect.
    //
    // On synchronous completion (the realistic single-redirect path) this
    // resolver runs its own reveal-marker clear + `EffectResolved` inline below,
    // matching the historical tail exactly (the chain processor that dispatched
    // this effect still owns priority/continuation). On a mid-pile CR 616.1
    // ordering pause, the prompt is parked and the undelivered tail stashed;
    // defer the marker-clear + `EffectResolved` onto a cleanup-only completion
    // (`rest_cards` empty — the pile IS this batch) so the drain runs it once the
    // pile lands, and bail before the inline tail so `EffectResolved` never lands
    // over the parked prompt.
    let mut clear_markers = revealed_misses.clone();
    if let Some(hit) = hit_card {
        clear_markers.push(hit);
    }
    match move_rest_then(state, &revealed_misses, rest_destination, None, events) {
        zone_pipeline::BatchMoveResult::Done => {}
        zone_pipeline::BatchMoveResult::NeedsChoice => {
            zone_pipeline::defer_completion_on_pause(
                state,
                BatchCompletion::RevealRestPile {
                    player: revealing_player,
                    rest_cards: Vec::new(),
                    rest_destination,
                    clear_markers,
                    publish_tracked_set: None,
                    emit_reveal_until_resolved: Some(ability.source_id),
                },
            );
            return Ok(());
        }
    }

    // Clear reveal markers — cards have moved zones.
    for &card_id in &clear_markers {
        state.revealed_cards.remove(&card_id);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RevealUntil,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.20a + CR 608.2c: Resolve the Aurora Awakener-class disposition —
/// "reveal until you reveal X [filter] cards. Put any number of those [filter]
/// cards onto [kept_destination], then put the rest of the revealed cards [in
/// rest_destination] (in a random order when bottoming a library)."
///
/// Reveals cards one at a time (CR 701.20a/701.20b — the card stays in the
/// library while revealed) until `target_match_count` cards match `filter` or
/// the library is exhausted, emits `CardsRevealed` for every revealed card, then
/// surfaces a `WaitingFor::DigChoice` over the matched set: the controller may
/// select any subset (`up_to`) for `kept_destination`; every other revealed card
/// (non-selected matches AND interleaved misses) flows to `rest_destination`.
/// This reuses the `Effect::Dig` "put any number onto the battlefield, rest on
/// the bottom" interaction machinery (the DigChoice handler routes the kept
/// cards through the CR 614.1c delivery tail and the rest through the partition
/// mover).
///
/// When `target_match_count == 0` (e.g. Aurora with zero colors among
/// permanents), nothing is revealed and the effect resolves with no choice
/// (CR 701.20a — the until-condition is already satisfied).
#[allow(clippy::too_many_arguments)]
fn resolve_choose_any_number(
    state: &mut GameState,
    ability: &ResolvedAbility,
    revealing_player: PlayerId,
    library: &[ObjectId],
    filter: &TargetFilter,
    ctx: &FilterContext,
    target_match_count: usize,
    kept_destination: Zone,
    rest_destination: Zone,
    enter_tapped: EtbTapState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut revealed: Vec<ObjectId> = Vec::new();
    let mut matched: Vec<ObjectId> = Vec::new();

    // CR 701.20a: reveal one at a time until `target_match_count` matches are
    // found (or the library runs out). `target_match_count == 0` reveals nothing.
    if target_match_count > 0 {
        for &card_id in library {
            // CR 701.20b: the card stays in its library zone while revealed.
            state.revealed_cards.insert(card_id);
            revealed.push(card_id);
            if matches_target_filter(state, card_id, filter, ctx) {
                matched.push(card_id);
                if matched.len() >= target_match_count {
                    break;
                }
            }
        }
    }

    // CR 701.20a: emit a single CardsRevealed for the whole revealed pile.
    let card_names: Vec<String> = revealed
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: revealing_player,
        card_ids: revealed.clone(),
        card_names,
    });
    state.last_revealed_ids = revealed.clone();

    // CR 608.2c: nothing was revealed (count 0 or an empty library) — the
    // disposition has no cards to act on; resolve cleanly with no interaction.
    if revealed.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::RevealUntil,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 701.20a + CR 608.2c: offer the controller a DigChoice over the matched
    // set — any number go to `kept_destination`, the rest to `rest_destination`.
    // `up_to` lets the controller keep zero. The reveal markers on every card are
    // cleared automatically when each card changes zone during the choice's
    // resolution (CR 400.7, `zones::move_*` clears `revealed_cards`).
    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        library_owner: revealing_player,
        cards: revealed,
        keep_count: matched.len(),
        up_to: true,
        selectable_cards: matched,
        kept_destination: Some(kept_destination),
        rest_destination: Some(rest_destination),
        source_id: Some(ability.source_id),
        enter_tapped: enter_tapped.is_tapped(),
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RevealUntil,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 109.5: Resolve the `player` filter on a [`RevealUntil`] effect into a
/// concrete [`PlayerId`]. Mirrors [`crate::game::effects::token::resolve_token_owner`]:
/// `Controller` → activator; `ParentTargetController` → controller of the parent
/// ability's targeted object (Polymorph, Proteus Staff, Transmogrify); any other
/// player-resolving filter → `TargetRef::Player` extracted from `ability.targets`
/// (Telemin Performance / Mind Funeral "target opponent reveals..."). Falls
/// back to the activator when the filter cannot be resolved (defensive default
/// matching the historical behavior of this effect).
fn resolve_revealing_player(
    state: &GameState,
    ability: &ResolvedAbility,
    player_filter: &TargetFilter,
) -> PlayerId {
    match player_filter {
        TargetFilter::Controller => ability.controller,
        TargetFilter::ParentTargetController => {
            crate::game::ability_utils::parent_target_controller(ability, state)
                .unwrap_or(ability.controller)
        }
        _ => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(id) => state.objects.get(id).map(|obj| obj.controller),
            })
            .unwrap_or(ability.controller),
    }
}

/// CR 701.20a + CR 614.6 + CR 603.10a: Move the rest pile to `rest_destination`,
/// running `completion` (the reveal-marker clear / tracked-set publish /
/// `RevealUntil`-resolved cleanup) exactly once after the pile lands — whether
/// the pile moves synchronously or a per-card `Moved` redirect pauses on a
/// CR 616.1 ordering choice.
///
/// Single authority for rest-pile placement. A `Zone::Graveyard` (or any other
/// non-library) rest pile routes through the zone-change pipeline so a `Moved`
/// graveyard→exile redirect (Rest in Peace / Leyline of the Void) fires on each
/// rest card — the 12 `rest_destination: Graveyard` reveal-until cards (Mind
/// Funeral class) previously dropped that redirect via the raw `move_to_zone`.
/// The pipeline batch co-stamps the departures (CR 603.10a) and, on a mid-pile
/// pause, parks the prompt and re-runs `completion` from the drain path; the
/// completion is carried with an empty `rest_cards` so it does NOT re-move the
/// pile (the pile IS this batch — the completion is cleanup-only here).
///
/// A `Zone::Library` rest pile randomizes the request order first, then delivers
/// every card through the placement-aware pipeline arm with
/// `LibraryPosition::Bottom`. This preserves the effect instruction's random
/// bottom placement while keeping `Moved(destination = Library)` replacement
/// consultation centralized in `zone_pipeline::move_object`.
pub(crate) fn move_rest_then(
    state: &mut GameState,
    cards: &[ObjectId],
    rest_destination: Zone,
    completion: Option<BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> zone_pipeline::BatchMoveResult {
    match rest_destination {
        Zone::Library => {
            // Random-order bottom placement is the effect instruction; CR 701.20a
            // keeps the cards revealed until this rest-pile work completes.
            let reqs = library_bottom_requests_in_random_order(state, cards);
            zone_pipeline::move_objects_simultaneously_then(state, reqs, completion, events)
        }
        dest => {
            // CR 400.7: the rest cards move themselves to `dest`; each anchors
            // its own attribution (the pre-pipeline raw move recorded no source).
            let reqs: Vec<ZoneMoveRequest> = cards
                .iter()
                .map(|&card_id| ZoneMoveRequest::effect(card_id, dest, card_id))
                .collect();
            zone_pipeline::move_objects_simultaneously_then(state, reqs, completion, events)
        }
    }
}

/// Build bottom-placement requests in random order.
fn library_bottom_requests_in_random_order(
    state: &mut GameState,
    cards: &[ObjectId],
) -> Vec<ZoneMoveRequest> {
    let mut shuffled = cards.to_vec();
    shuffled.shuffle(&mut state.rng);

    shuffled
        .into_iter()
        .map(|card_id| {
            ZoneMoveRequest::effect(card_id, Zone::Library, card_id)
                .at_library_position(LibraryPosition::Bottom)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ReplacementDefinition, TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;

    /// Synthetic board-wide replacement: "If an object would be put into
    /// `destination`, exile it instead." No pool card currently defines the
    /// Library variant, but it discriminates raw delivery from the pipeline arm.
    fn install_destination_to_exile_redirect(
        state: &mut GameState,
        destination: Zone,
        name: &str,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(90001),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::Any,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            face_down_profile: None,
                        },
                    ))
                    .destination_zone(destination),
            );
        source
    }

    fn install_library_to_exile_redirect(state: &mut GameState) -> ObjectId {
        install_destination_to_exile_redirect(state, Zone::Library, "Library Exile Redirect")
    }

    fn install_hand_to_exile_redirect(state: &mut GameState) -> ObjectId {
        install_destination_to_exile_redirect(state, Zone::Hand, "Hand Exile Redirect")
    }

    fn make_reveal_until_ability(
        controller: PlayerId,
        filter: TargetFilter,
        kept_destination: Zone,
        rest_destination: Zone,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealUntil {
                player: TargetFilter::Controller,
                filter,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                matched_disposition: RevealUntilDisposition::KeepEach,
                kept_destination,
                rest_destination,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: None,
                enters_under: None,
            },
            vec![],
            ObjectId(100),
            controller,
        )
    }

    fn make_reveal_until_ability_with_player(
        controller: PlayerId,
        player: TargetFilter,
        targets: Vec<TargetRef>,
        filter: TargetFilter,
        kept_destination: Zone,
        rest_destination: Zone,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealUntil {
                player,
                filter,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                matched_disposition: RevealUntilDisposition::KeepEach,
                kept_destination,
                rest_destination,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: None,
                enters_under: None,
            },
            targets,
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn reveal_until_finds_creature_puts_to_hand() {
        let mut state = GameState::new_two_player(42);

        // Library: land, land, creature (top to bottom by creation order)
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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature should be in hand
        assert!(state.players[0].hand.contains(&creature));
        // Lands should be on bottom of library
        assert!(state.players[0].library.contains(&land1));
        assert!(state.players[0].library.contains(&land2));
        // CardsRevealed event should include all three
        let revealed = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        });
        assert_eq!(revealed.unwrap().len(), 3);
    }

    /// C5 discriminating test (CR 614.6 + CR 701.20a): a kept card placed back
    /// into a library must run the `Moved` replacement consult. The old raw
    /// `move_to_zone(..., Library)` skipped this synthetic Library→Exile
    /// redirect and left the card in the library.
    #[test]
    fn reveal_until_kept_library_redirected_to_exile() {
        let mut state = GameState::new_two_player(42);
        install_library_to_exile_redirect(&mut state);

        let creature = create_object(
            &mut state,
            CardId(1),
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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Library,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&creature].zone, Zone::Exile);
        assert!(!state.players[0].library.contains(&creature));
    }

    /// C5 discriminating test (CR 614.6 + CR 701.20a): a kept card put into a
    /// hand must run the `Moved` replacement consult. The old raw
    /// `move_to_zone(..., Hand)` skipped this synthetic Hand→Exile redirect.
    #[test]
    fn reveal_until_kept_hand_redirected_to_exile() {
        let mut state = GameState::new_two_player(42);
        install_hand_to_exile_redirect(&mut state);

        let creature = create_object(
            &mut state,
            CardId(1),
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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&creature].zone, Zone::Exile);
        assert!(!state.players[0].hand.contains(&creature));
    }

    /// C5 discriminating test (CR 614.6 + CR 701.20a): a reveal-until rest pile
    /// returned to the bottom of a library must still run through the placement
    /// pipeline. The old raw `move_to_library_position(..., bottom)` skipped
    /// this synthetic Library→Exile redirect.
    #[test]
    fn reveal_until_library_rest_redirected_to_exile() {
        let mut state = GameState::new_two_player(42);
        install_library_to_exile_redirect(&mut state);

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let creature = create_object(
            &mut state,
            CardId(2),
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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&creature));
        assert_eq!(state.objects[&land].zone, Zone::Exile);
        assert!(!state.players[0].library.contains(&land));
    }

    #[test]
    fn reveal_until_puts_to_battlefield() {
        let mut state = GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Battlefield,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature should be on the battlefield
        assert!(state.battlefield.contains(&creature));
    }

    /// C5 discriminating test (CR 614.1c + CR 306.5b): a planeswalker revealed
    /// to the battlefield must enter with its intrinsic loyalty counters. The
    /// old raw `move_to_zone` skipped the delivery tail, so the planeswalker
    /// entered with 0 loyalty and was put into the graveyard by CR 704.5i.
    /// Routing through `move_object` seeds the intrinsic counters via the
    /// CR 614.1c pipeline.
    #[test]
    fn reveal_until_planeswalker_enters_with_intrinsic_loyalty() {
        use crate::types::card_type::CoreType;
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);

        let walker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Planeswalker".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&walker).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(4);
            obj.base_loyalty = Some(4);
        }

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                crate::types::ability::TypeFilter::Planeswalker,
            )),
            Zone::Battlefield,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 614.1c: entered with 4 loyalty counters (not 0).
        assert!(
            state.battlefield.contains(&walker),
            "planeswalker must be on the battlefield, not graveyard"
        );
        assert_eq!(
            state.objects[&walker]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(4),
            "planeswalker must enter with its intrinsic loyalty counters via the CR 614.1c delivery tail"
        );
    }

    #[test]
    fn reveal_until_rest_to_graveyard() {
        let mut state = GameState::new_two_player(42);

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let creature = create_object(
            &mut state,
            CardId(2),
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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Graveyard,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature in hand, land in graveyard
        assert!(state.players[0].hand.contains(&creature));
        assert!(state.players[0].graveyard.contains(&land));
    }

    /// Discriminating test (CR 614.6 + CR 701.20a): a `rest_destination:
    /// Graveyard` reveal-until (Mind Funeral class, 12 cards) whose rest pile is
    /// caught by a Rest in Peace–style `Moved` graveyard→exile redirect must
    /// have its rest cards EXILED, not graveyard'd. The old raw `move_to_zone`
    /// rest-pile delivery never proposed the inner ZoneChange, so the redirect
    /// silently dropped and the land landed in the graveyard. Routing the rest
    /// pile through `move_objects_simultaneously` consults the redirect.
    #[test]
    fn reveal_until_graveyard_rest_redirected_to_exile_by_rest_in_peace() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, ReplacementDefinition,
        };
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);

        // Rest in Peace: "If a card would be put into a graveyard from anywhere,
        // exile it instead." (graveyard→exile Moved redirect on the battlefield)
        let rip = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        let redirect = ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    origin: None,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
            ));
        state.objects.get_mut(&rip).unwrap().replacement_definitions = vec![redirect].into();

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let creature = create_object(
            &mut state,
            CardId(2),
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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Graveyard,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The matching creature still goes to hand; the rest pile (the land) is
        // redirected from graveyard → exile by Rest in Peace, NOT graveyard'd.
        assert!(state.players[0].hand.contains(&creature));
        assert!(
            !state.players[0].graveyard.contains(&land),
            "rest card must NOT reach the graveyard — RIP redirects it"
        );
        assert_eq!(
            state.objects.get(&land).map(|o| o.zone),
            Some(Zone::Exile),
            "rest card must be exiled by the graveyard→exile redirect"
        );
    }

    #[test]
    fn reveal_until_no_match_all_to_rest() {
        let mut state = GameState::new_two_player(42);

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

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No creature found — all cards go to bottom of library
        assert!(state.players[0].hand.is_empty());
        assert_eq!(state.players[0].library.len(), 2);
    }

    #[test]
    fn reveal_until_empty_library() {
        let mut state = GameState::new_two_player(42);

        let ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Hand,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No crash, effect resolves cleanly
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    /// CR 701.20a + CR 608.2c: "You may put that card onto the battlefield" —
    /// `kept_optional_to: Some(_)` pauses on `WaitingFor::RevealUntilKeptChoice`
    /// after a hit is found. The choice handler routes the hit card: accept →
    /// `accept_zone`; decline → `decline_zone` (the repurposed `kept_destination`).
    #[test]
    fn reveal_until_optional_kept_pauses_and_routes_choice() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        fn setup() -> (GameState, ObjectId, ObjectId) {
            let mut state = GameState::new_two_player(42);
            let land = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Library,
            );
            state
                .objects
                .get_mut(&land)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
            let creature = create_object(
                &mut state,
                CardId(2),
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
            (state, land, creature)
        }

        fn optional_ability() -> ResolvedAbility {
            ResolvedAbility::new(
                Effect::RevealUntil {
                    player: TargetFilter::Controller,
                    filter: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    matched_disposition: RevealUntilDisposition::KeepEach,
                    kept_destination: Zone::Hand,
                    rest_destination: Zone::Library,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    kept_optional_to: Some(Zone::Battlefield),
                    enters_under: None,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
        }

        // Accept → hit card onto the battlefield.
        {
            let (mut state, land, creature) = setup();
            let ability = optional_ability();
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();

            match state.waiting_for.clone() {
                WaitingFor::RevealUntilKeptChoice { hit_card, .. } => {
                    assert_eq!(hit_card, creature, "hit card should be the creature");
                }
                other => panic!("Expected RevealUntilKeptChoice, got {other:?}"),
            }

            let wf = state.waiting_for.clone();
            handle_resolution_choice(
                &mut state,
                wf,
                GameAction::DecideOptionalEffect { accept: true },
                &mut events,
            )
            .unwrap();
            assert!(
                state.battlefield.contains(&creature),
                "accepted hit card should be on the battlefield"
            );
            assert!(
                state.players[0].library.contains(&land),
                "miss should be on the bottom of the library"
            );
        }

        // Decline → hit card to the decline zone (kept_destination = Hand).
        {
            let (mut state, land, creature) = setup();
            let ability = optional_ability();
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();

            let wf = state.waiting_for.clone();
            handle_resolution_choice(
                &mut state,
                wf,
                GameAction::DecideOptionalEffect { accept: false },
                &mut events,
            )
            .unwrap();
            assert!(
                state.players[0].hand.contains(&creature),
                "declined hit card should be in hand (decline zone)"
            );
            assert!(
                state.players[0].library.contains(&land),
                "miss should be on the bottom of the library"
            );
            assert!(
                !state.battlefield.contains(&creature),
                "declined hit card must not be on the battlefield"
            );
        }
    }

    /// C5 review fix (CR 701.24a): an optional kept card accepted to
    /// `Zone::Library` must be explicit bottom placement, not a placement-less
    /// library move. Without `.at_library_position(Bottom)` the delivery tail
    /// auto-shuffles and emits `ShuffledLibrary`.
    #[test]
    fn reveal_until_kept_choice_library_accept_does_not_shuffle() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let creature = create_object(
            &mut state,
            CardId(2),
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

        let ability = ResolvedAbility::new(
            Effect::RevealUntil {
                player: TargetFilter::Controller,
                filter: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                matched_disposition: RevealUntilDisposition::KeepEach,
                kept_destination: Zone::Hand,
                rest_destination: Zone::Library,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: Some(Zone::Library),
                enters_under: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let wf = state.waiting_for.clone();
        handle_resolution_choice(
            &mut state,
            wf,
            GameAction::DecideOptionalEffect { accept: true },
            &mut events,
        )
        .unwrap();

        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                    ..
                }
            )),
            "accepted library placement must not degrade into an auto-shuffled library move"
        );
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            vec![creature, land],
            "accepted hit is placed on bottom, then the one-card rest pile is placed below it"
        );
    }

    /// C6 discriminating test (CR 614.1c + CR 306.5b): accepting a planeswalker
    /// through the `RevealUntilKeptChoice` battlefield path must enter it with
    /// its intrinsic loyalty counters. The old handler used a raw `move_to_zone`
    /// (loyalty 0 → dead by CR 704.5i); the migrated handler routes through
    /// `zone_pipeline::move_object` so the CR 614.1c delivery tail seeds them.
    #[test]
    fn reveal_until_kept_choice_planeswalker_enters_with_loyalty() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let walker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Planeswalker".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&walker).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(5);
            obj.base_loyalty = Some(5);
        }

        let ability = ResolvedAbility::new(
            Effect::RevealUntil {
                player: TargetFilter::Controller,
                filter: TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                    crate::types::ability::TypeFilter::Planeswalker,
                )),
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                matched_disposition: RevealUntilDisposition::KeepEach,
                kept_destination: Zone::Hand,
                rest_destination: Zone::Library,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: Some(Zone::Battlefield),
                enters_under: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let wf = state.waiting_for.clone();
        assert!(matches!(wf, WaitingFor::RevealUntilKeptChoice { .. }));
        handle_resolution_choice(
            &mut state,
            wf,
            GameAction::DecideOptionalEffect { accept: true },
            &mut events,
        )
        .unwrap();

        assert!(
            state.battlefield.contains(&walker),
            "planeswalker must be on the battlefield, not graveyard"
        );
        assert_eq!(
            state.objects[&walker]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(5),
            "planeswalker must enter with intrinsic loyalty via the CR 614.1c delivery tail"
        );
    }

    /// CR 109.5 + CR 701.20a: When `player = ParentTargetController`, the library
    /// of the parent ability's target's controller is revealed — the activator's
    /// own library is left untouched. This is the Polymorph / Proteus Staff /
    /// Transmogrify pattern.
    #[test]
    fn reveal_until_parent_target_controller_reveals_target_owner_library() {
        let mut state = GameState::new_two_player(42);

        // Activator is PlayerId(0); the targeted creature (and its library) belongs
        // to PlayerId(1). The activator's library must NOT be touched.
        let opponent_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Opponent's library: a land then a creature (top→bottom).
        let opp_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&opp_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let opp_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Bear2".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&opp_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Activator's library: a creature on top — must NOT be touched.
        let activator_creature = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "ActivatorBear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&activator_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = make_reveal_until_ability_with_player(
            PlayerId(0),
            TargetFilter::ParentTargetController,
            vec![TargetRef::Object(opponent_creature)],
            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            Zone::Battlefield,
            Zone::Library,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent's creature card moved to the battlefield (under its owner's control).
        assert!(state.battlefield.contains(&opp_creature));
        assert_eq!(
            state.objects.get(&opp_creature).unwrap().controller,
            PlayerId(1)
        );
        // Activator's library is undisturbed — their bear is still on top.
        assert_eq!(
            state.players[0].library.front().copied(),
            Some(activator_creature)
        );
        // The CardsRevealed event names the revealing player (the opponent), not the activator.
        let revealing_player = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed { player, .. } => Some(*player),
            _ => None,
        });
        assert_eq!(revealing_player, Some(PlayerId(1)));
    }
}
