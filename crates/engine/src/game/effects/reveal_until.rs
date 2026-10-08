use rand::seq::SliceRandom;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::zone_pipeline::{self, ZoneMoveRequest};
use crate::types::ability::{
    DigRestOrder, Effect, EffectError, EffectKind, LibraryPosition, ParentTargetMissingReason,
    ResolvedAbility, RevealUntilDisposition, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    ResolvedInformationAudience, ResolvedInformationEdit, ResolvedInformationLifetime,
};
use crate::types::zones::{EtbTapState, Zone};

/// CR 701.20a: Reveal cards from the top of the controller's library one at a
/// time until a card matching the filter is found. The matching cards go to
/// `kept_destination` (per-card `kept_destination_if`) as one simultaneous move
/// (CR 608.2f), the remaining revealed cards go to `rest_destination`.
///
/// All revealed cards are marked as publicly revealed and a `CardsRevealed`
/// event is emitted. If the library is exhausted without finding a match, all
/// revealed cards go to `rest_destination`.
///
/// CR 603.12 + CR 608.2c: after this instance's own moves have run (or paused),
/// its outcome is published on the one-hop parent->child hand-off slot, so the
/// immediate child — including a `WhenYouDo` "when you reveal a <filter> card
/// this way" reflexive — reads THIS reveal's verdict and never an unrelated
/// event that a nested replacement effect pushed into the same event slice.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut matched = false;
    let result = resolve_reveal(state, ability, events, &mut matched);
    if result.is_ok() {
        publish_reveal_until_verdict(state, matched);
    }
    result
}

/// CR 701.20a + CR 603.12: Publish whether this reveal-until revealed any card
/// matching its until-filter. A whiff records
/// `ParentTargetMissingReason::RevealUntil`; a hit clears the slot, because this
/// instance is the immediate parent of the next hand-off and no stale reason
/// from an earlier or nested producer may reach its child.
fn publish_reveal_until_verdict(state: &mut GameState, matched: bool) {
    state.last_parent_target_missing_reason =
        (!matched).then_some(ParentTargetMissingReason::RevealUntil);
}

fn resolve_reveal(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    matched: &mut bool,
) -> Result<(), EffectError> {
    let (
        player_filter,
        filter,
        count_expr,
        matched_disposition,
        kept_destination,
        rest_destination,
        rest_order,
        enter_tapped,
        enters_attacking,
        kept_optional_to,
        enters_under,
        kept_destination_if,
    ) = match &ability.effect {
        Effect::RevealUntil {
            player,
            filter,
            count,
            matched_disposition,
            kept_destination,
            rest_destination,
            rest_order,
            enter_tapped,
            enters_attacking,
            kept_optional_to,
            enters_under,
            kept_destination_if,
        } => (
            player,
            filter,
            count,
            *matched_disposition,
            *kept_destination,
            *rest_destination,
            *rest_order,
            *enter_tapped,
            *enters_attacking,
            *kept_optional_to,
            enters_under.as_ref(),
            kept_destination_if.as_ref(),
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
    let Some(revealing_player) = resolve_revealing_player(state, ability, player_filter) else {
        super::publish_reveal_result(state, Vec::new());
        return Ok(());
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == revealing_player)
        .ok_or(EffectError::PlayerNotFound)?;

    // Snapshot library (top = index 0) to iterate without borrow conflicts.
    let library: Vec<ObjectId> = state.library_of(player.id).iter().copied().collect();
    let mut revealed_misses: Vec<ObjectId> = Vec::new();
    let mut hit_cards: Vec<ObjectId> = Vec::new();

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
            rest_order,
            enter_tapped,
            events,
            matched,
        );
    }

    // CR 701.20a: Reveal cards one at a time until `target_match_count` matches
    // are found (or the library is exhausted). `target_match_count == 0` reveals
    // nothing (CR 701.20a — the until-condition is already satisfied).
    if target_match_count > 0 {
        for &card_id in &library {
            if matches_target_filter(state, card_id, filter, &ctx) {
                hit_cards.push(card_id);
                if hit_cards.len() >= target_match_count {
                    break;
                }
            } else {
                revealed_misses.push(card_id);
            }
        }
    }
    *matched = !hit_cards.is_empty();

    // CR 608.2c: when exactly one card was hit by the until condition, snapshot
    // it before moving to its destination so chained instructions (e.g. Erratic
    // Mutation's pump reading "that card's mana value") bind to it.
    let hit_snapshot = if hit_cards.len() == 1 {
        state.capture_event_object_snapshot(hit_cards[0])
    } else {
        None
    };

    // Build the full list of revealed card IDs for the event.
    let mut all_revealed: Vec<ObjectId> = revealed_misses.clone();
    all_revealed.extend(&hit_cards);

    state
        .resolve_and_apply_information(
            &all_revealed,
            ResolvedInformationAudience::Controller(ability.controller),
            ResolvedInformationLifetime::UntilActionBoundary,
            ResolvedInformationEdit::Reveal,
        )
        .expect("resolved reveal-until occurrences must be live and distinct");

    // CR 701.20a + CR 400.7: Only reveal-only and paused optional dispositions
    // retain these exact library occurrences after the resolver returns. Publish
    // those occurrences here, before any later zone change can create a new one.
    if matches!(matched_disposition, RevealUntilDisposition::RevealOnly)
        || matches!((kept_optional_to, hit_cards.as_slice()), (Some(_), [_]))
    {
        state
            .resolve_and_apply_information(
                &all_revealed,
                ResolvedInformationAudience::Public,
                ResolvedInformationLifetime::UntilZoneChange,
                ResolvedInformationEdit::Reveal,
            )
            .expect("published reveal-until occurrences must be live and distinct");
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
    state.last_revealed_ids = all_revealed.clone();
    // CR 608.2c + CR 701.20a: "[quantity] revealed this way" names the cards this
    // reveal revealed — every card looked at until the until-condition was met,
    // matching or not. Publish them as the chain's tracked set so a downstream
    // "the number of nonland cards revealed this way" reads the revealed
    // population (Goblin Charbelcher) even after the cards have been moved.
    if all_revealed.is_empty() {
        super::publish_fresh_tracked_set(state, Vec::new());
    } else {
        super::publish_tracked_set(state, all_revealed.clone());
    }

    // CR 701.20b: reveal-only until-loop — cards stay in their zones (Sanar's
    // Vivid draws nothing to hand before per-color exile from the library).
    if matches!(matched_disposition, RevealUntilDisposition::RevealOnly) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::RevealUntil,
            source_id: ability.source_id,
            subject: hit_snapshot.map(Box::new),
        });
        return Ok(());
    }

    // CR 701.20a + CR 608.2c: "You may put that card onto the battlefield" — when
    // the kept destination is a controller choice and a hit was found, pause for
    // `WaitingFor::RevealUntilKeptChoice`. The choice handler routes the hit card,
    // moves the misses, and drains `pending_continuation`. `EffectResolved` is
    // emitted here (before the pause) mirroring `discover::resolve`.
    if let (Some(accept_zone), [hit]) = (kept_optional_to, hit_cards.as_slice()) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::RevealUntil,
            source_id: ability.source_id,
            subject: hit_snapshot.map(Box::new),
        });
        state.waiting_for = WaitingFor::RevealUntilKeptChoice {
            player: revealing_player,
            hit_card: *hit,
            source_id: ability.source_id,
            accept_zone,
            decline_zone: kept_destination,
            enter_tapped,
            enters_attacking,
            revealed_misses,
            rest_destination,
            rest_order,
        };
        return Ok(());
    }

    // CR 701.20a + CR 608.2f: When every revealed card goes to the same zone —
    // both piles on the bottom of the library, "then puts those cards into
    // their graveyard" (Balustrade Spy, Mind Grind), "put all cards revealed
    // this way into your hand" (Treasure Hunt) — the instruction is one action
    // on the whole revealed pile, processed simultaneously as a single zone
    // change, so a "one or more" observer triggers once for it (CR 603.2c). A
    // per-card `kept_destination_if` split is not a whole-pile action, and a
    // battlefield pile keeps the per-set path below, whose requests carry the
    // entry modifiers (tap state, controller, attacking) this mover does not.
    let whole_pile_destination = match (kept_destination, rest_destination, kept_destination_if) {
        (Zone::Battlefield, _, _) | (_, _, Some(_)) => None,
        (kept, rest, None) if kept == rest => Some(kept),
        _ => None,
    };
    if let Some(pile_destination) = whole_pile_destination {
        let clear_markers = all_revealed.clone();
        // CR 608.2d + CR 401.4: a library pile "in any order" (PlayerChoice) of
        // 2+ cards pauses for the library's owner to announce their bottom order.
        if pile_destination == Zone::Library
            && rest_order == DigRestOrder::PlayerChoice
            && all_revealed.len() >= 2
        {
            state.waiting_for = WaitingFor::RevealUntilBottomOrder {
                player: revealing_player,
                source_id: ability.source_id,
                cards: all_revealed,
                clear_markers,
                emit_reveal_until_resolved: Some(ability.source_id),
                reveal_until_hit_snapshot: hit_snapshot.map(Box::new),
            };
            return Ok(());
        }
        match move_rest_then(
            state,
            &all_revealed,
            pile_destination,
            rest_order,
            None,
            events,
        ) {
            zone_pipeline::BatchMoveResult::Done => {}
            zone_pipeline::BatchMoveResult::NeedsChoice => {
                zone_pipeline::defer_completion_on_pause(
                    state,
                    BatchCompletion::RevealRestPile {
                        delivery_stage: crate::types::game_state::DigDeliveryStage::Rest,
                        player: revealing_player,
                        source_id: Some(ability.source_id),
                        rest_cards: Vec::new(),
                        rest_destination: pile_destination,
                        rest_order,
                        rest_split_top_count: None,
                        clear_markers,
                        publish_tracked_set: None,
                        publish_tracked_set_cause: None,
                        emit_reveal_until_resolved: Some(ability.source_id),
                        reveal_until_hit_snapshot: hit_snapshot.map(Box::new),
                        manifested_for_continuation: None,
                        kept_delivery: Default::default(),
                        continuation_targets: Vec::new(),
                        rest_delivery: Default::default(),
                    },
                );
                return Ok(());
            }
        }
        state
            .resolve_and_apply_information(
                &clear_markers,
                ResolvedInformationAudience::Controller(ability.controller),
                ResolvedInformationLifetime::UntilActionBoundary,
                ResolvedInformationEdit::Hide,
            )
            .expect("reveal-until cleanup must reference live card occurrences");

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::RevealUntil,
            source_id: ability.source_id,
            subject: hit_snapshot.map(Box::new),
        });
        return Ok(());
    }

    // Move the matching cards to their destinations.
    if !hit_cards.is_empty() {
        let controller_override = super::change_zone::resolve_enters_under_player(
            state,
            ability,
            "RevealUntil",
            enters_under,
        )?;
        // CR 608.2c: "if its mana value is <comparator> <quantity>, put it
        // onto the battlefield. Otherwise, put it into your hand" (Part in
        // Friendship) — a per-hit-card branch on the card's own
        // characteristics, evaluated exactly like the primary `filter`
        // field. `kept_destination` is the "otherwise" branch when the
        // card does not match. Every route is fixed before any card moves.
        let kept_routes: Vec<(ObjectId, Zone)> = hit_cards
            .iter()
            .map(|&hit| {
                let hit_destination = kept_destination_if
                    .filter(|(cond_filter, _)| matches_target_filter(state, hit, cond_filter, &ctx))
                    .map(|(_, zone)| *zone)
                    .unwrap_or(kept_destination);
                (hit, hit_destination)
            })
            .collect();
        let reqs: Vec<ZoneMoveRequest> = kept_routes
            .iter()
            .map(|&(hit, hit_destination)| match hit_destination {
                Zone::Battlefield => {
                    // CR 614.1c + CR 306.5b / CR 310.4b: route the battlefield entry
                    // through the zone-change pipeline so the full delivery tail runs
                    // — intrinsic enters-with counters (a revealed planeswalker /
                    // battle must enter with its loyalty / defense or it dies to
                    // CR 704.5i), enters-with-counters statics, and the CR 614.1
                    // tap-state. The pipeline applies `enter_tapped` from the seeded
                    // `EntryMods`.
                    let mut req =
                        ZoneMoveRequest::effect(hit, Zone::Battlefield, ability.source_id);
                    req.mods.enter_tapped = enter_tapped;
                    // CR 508.4: "put that card onto the battlefield tapped and
                    // attacking" — the creature is attacking as it enters, and
                    // its controller chooses what it attacks. The flag rides the
                    // entry request, so the delivery tail places it in combat
                    // before triggers are collected, and it survives a pause.
                    req.mods.enters_attacking = enters_attacking;
                    match controller_override {
                        Some(controller) => req.under_control_of(controller),
                        None => req,
                    }
                }
                // CR 614.6 + CR 701.24a: a kept card sent back to the library
                // keeps the effect's historical bottom placement; this is a
                // placement, not a shuffle. Route through the placement-aware
                // pipeline arm so a future Library-destination `Moved` replacement
                // can still fire.
                Zone::Library => ZoneMoveRequest::effect(hit, Zone::Library, ability.source_id)
                    .at_library_position(LibraryPosition::Bottom),
                // CR 614.6: a kept card sent to another zone routes through the
                // pipeline so a matching `Moved` redirect can fire.
                Zone::Hand | Zone::Graveyard | Zone::Stack | Zone::Exile | Zone::Command => {
                    ZoneMoveRequest::effect(hit, hit_destination, ability.source_id)
                }
            })
            .collect();
        // CR 608.2f + CR 701.20a: "Put those cards onto/into <zone>" is one
        // action on multiple objects, processed simultaneously — every matched
        // card moves in ONE zone-change batch, so the move is a single logical
        // zone change: a batched "one or more" observer triggers once for the
        // whole set (CR 603.2c), including one that is itself among the
        // newcomers (CR 603.6a). Non-batched per-entry observers among the
        // newcomers are still presence-gated by segment collection and see only
        // entries at or after their own — a pipeline-wide gap outside this seam.
        match zone_pipeline::move_objects_simultaneously(state, reqs, events) {
            zone_pipeline::BatchMoveResult::Done => {}
            // CR 303.4f / CR 614.1c / CR 616.1: the batch parked an as-enters
            // Aura-host or replacement-ordering prompt and stashed every
            // undelivered matched card in the active `BatchDelivery` frame.
            // Deferring the rest-pile tail onto that same frame means the drain
            // delivers the remaining matched cards, then the misses, the
            // reveal-marker cleanup, and `EffectResolved`, exactly once — so no
            // card strands in the library and `EffectResolved` never lands over
            // the parked prompt.
            zone_pipeline::BatchMoveResult::NeedsChoice => {
                let mut clear_markers = revealed_misses.clone();
                clear_markers.extend(&hit_cards);
                zone_pipeline::defer_completion_on_pause(
                    state,
                    BatchCompletion::RevealRestPile {
                        delivery_stage: crate::types::game_state::DigDeliveryStage::Rest,
                        player: revealing_player,
                        source_id: Some(ability.source_id),
                        rest_cards: revealed_misses,
                        rest_destination,
                        rest_order,
                        rest_split_top_count: None,
                        clear_markers,
                        publish_tracked_set: None,
                        publish_tracked_set_cause: None,
                        emit_reveal_until_resolved: Some(ability.source_id),
                        reveal_until_hit_snapshot: hit_snapshot.map(Box::new),
                        manifested_for_continuation: None,
                        kept_delivery: Default::default(),
                        continuation_targets: Vec::new(),
                        rest_delivery: Default::default(),
                    },
                );
                return Ok(());
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
    clear_markers.extend(&hit_cards);
    if rest_destination == Zone::Library
        && rest_order == DigRestOrder::PlayerChoice
        && revealed_misses.len() >= 2
    {
        state.waiting_for = WaitingFor::RevealUntilBottomOrder {
            player: revealing_player,
            source_id: ability.source_id,
            cards: revealed_misses,
            clear_markers,
            emit_reveal_until_resolved: Some(ability.source_id),
            reveal_until_hit_snapshot: hit_snapshot.map(Box::new),
        };
        return Ok(());
    }
    match move_rest_then(
        state,
        &revealed_misses,
        rest_destination,
        rest_order,
        None,
        events,
    ) {
        zone_pipeline::BatchMoveResult::Done => {}
        zone_pipeline::BatchMoveResult::NeedsChoice => {
            zone_pipeline::defer_completion_on_pause(
                state,
                BatchCompletion::RevealRestPile {
                    delivery_stage: crate::types::game_state::DigDeliveryStage::Rest,
                    player: revealing_player,
                    source_id: Some(ability.source_id),
                    rest_cards: Vec::new(),
                    rest_destination,
                    rest_order,
                    rest_split_top_count: None,
                    clear_markers,
                    publish_tracked_set: None,
                    publish_tracked_set_cause: None,
                    emit_reveal_until_resolved: Some(ability.source_id),
                    reveal_until_hit_snapshot: hit_snapshot.map(Box::new),
                    manifested_for_continuation: None,
                    kept_delivery: Default::default(),
                    continuation_targets: Vec::new(),
                    rest_delivery: Default::default(),
                },
            );
            return Ok(());
        }
    }

    // Zone delivery already clears the old occurrences through the shared
    // information authority. This no-op-safe call covers a same-zone placement
    // implementation that leaves a reveal lease behind.
    state
        .resolve_and_apply_information(
            &clear_markers,
            ResolvedInformationAudience::Controller(ability.controller),
            ResolvedInformationLifetime::UntilActionBoundary,
            ResolvedInformationEdit::Hide,
        )
        .expect("reveal-until cleanup must reference live card occurrences");

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RevealUntil,
        source_id: ability.source_id,
        subject: hit_snapshot.map(Box::new),
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
    rest_order: DigRestOrder,
    enter_tapped: EtbTapState,
    events: &mut Vec<GameEvent>,
    any_matched: &mut bool,
) -> Result<(), EffectError> {
    let mut revealed: Vec<ObjectId> = Vec::new();
    let mut matched: Vec<ObjectId> = Vec::new();

    // CR 701.20a: reveal one at a time until `target_match_count` matches are
    // found (or the library runs out). `target_match_count == 0` reveals nothing.
    if target_match_count > 0 {
        for &card_id in library {
            revealed.push(card_id);
            if matches_target_filter(state, card_id, filter, ctx) {
                matched.push(card_id);
                if matched.len() >= target_match_count {
                    break;
                }
            }
        }
    }
    *any_matched = !matched.is_empty();

    state
        .resolve_and_apply_information(
            &revealed,
            ResolvedInformationAudience::Controller(ability.controller),
            ResolvedInformationLifetime::UntilActionBoundary,
            ResolvedInformationEdit::Reveal,
        )
        .expect("resolved reveal-until occurrences must be live and distinct");
    state
        .resolve_and_apply_information(
            &revealed,
            ResolvedInformationAudience::Public,
            ResolvedInformationLifetime::UntilZoneChange,
            ResolvedInformationEdit::Reveal,
        )
        .expect("published reveal-until occurrences must be live and distinct");

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
        super::publish_fresh_tracked_set(state, Vec::new());
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::RevealUntil,
            source_id: ability.source_id,
            subject: None,
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
        rest_order,
        rest_split_top_count: None,
        source_id: Some(ability.source_id),
        enter_tapped: enter_tapped.is_tapped(),
        enters_attacking: false,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RevealUntil,
        source_id: ability.source_id,
        subject: None,
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
) -> Option<PlayerId> {
    match player_filter {
        TargetFilter::Controller => Some(ability.controller),
        TargetFilter::ParentTargetController => Some(
            crate::game::ability_utils::parent_target_controller(ability, state)
                .unwrap_or(ability.controller),
        ),
        TargetFilter::DeclaredPlayer { .. } => {
            super::resolve_player_for_context_ref(state, ability, player_filter)
        }
        _ if super::declared_player_slot_is_empty(state, ability, player_filter) => None,
        _ => Some(
            ability
                .targets
                .iter()
                .find_map(|target| match target {
                    TargetRef::Player(pid) => Some(*pid),
                    TargetRef::Object(id) => state.objects.get(id).map(|obj| obj.controller),
                })
                .unwrap_or(ability.controller),
        ),
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
/// A `Zone::Library` rest pile randomizes the request order first (when
/// `rest_order == DigRestOrder::Random`) or preserves encounter order (when
/// `rest_order == DigRestOrder::Preserve`), then delivers every card through the
/// placement-aware pipeline arm with `LibraryPosition::Bottom`. This preserves
/// the effect instruction's ordering while keeping `Moved(destination = Library)`
/// replacement consultation centralized in `zone_pipeline::move_object`.
pub(crate) fn move_rest_then(
    state: &mut GameState,
    cards: &[ObjectId],
    rest_destination: Zone,
    rest_order: DigRestOrder,
    completion: Option<BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> zone_pipeline::BatchMoveResult {
    match rest_destination {
        Zone::Library => {
            // CR 701.20a keeps the cards revealed until this rest-pile work completes.
            let reqs = match rest_order {
                DigRestOrder::Random => library_bottom_requests_in_random_order(state, cards),
                DigRestOrder::Preserve | DigRestOrder::PlayerChoice => {
                    library_bottom_requests_in_preserve_order(cards)
                }
            };
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

/// Build bottom-placement requests preserving the encounter order.
fn library_bottom_requests_in_preserve_order(cards: &[ObjectId]) -> Vec<ZoneMoveRequest> {
    cards
        .iter()
        .map(|&card_id| {
            ZoneMoveRequest::effect(card_id, Zone::Library, card_id)
                .at_library_position(LibraryPosition::Bottom)
        })
        .collect()
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
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    ))
                    .destination_zone(destination),
            );
        source
    }

    fn install_library_to_exile_redirect(state: &mut GameState) -> ObjectId {
        install_destination_to_exile_redirect(state, Zone::Library, "Library Exile Redirect")
    }

    fn library_card(state: &mut GameState, card: CardId, name: &str, core: CoreType) -> ObjectId {
        let id = create_object(state, card, PlayerId(0), name.to_string(), Zone::Library);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(core);
        id
    }

    /// CR 603.12 + CR 701.20a: the reveal publishes ITS OWN verdict after its
    /// moves. A hit clears a stale reason (e.g. one a nested producer left
    /// behind), and a whiff records `RevealUntil`, whatever was there before.
    #[test]
    fn verdict_is_this_reveals_own_outcome_regardless_of_the_prior_slot() {
        let creature_filter = TargetFilter::Typed(crate::types::ability::TypedFilter::creature());

        let mut hit_state = GameState::new_two_player(42);
        library_card(&mut hit_state, CardId(1), "Forest", CoreType::Land);
        library_card(&mut hit_state, CardId(2), "Bear", CoreType::Creature);
        hit_state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::RevealUntil);
        let ability = make_reveal_until_ability(
            PlayerId(0),
            creature_filter.clone(),
            Zone::Hand,
            Zone::Library,
        );
        resolve(&mut hit_state, &ability, &mut Vec::new()).unwrap();
        assert!(
            !hit_state.players[0].hand.is_empty(),
            "reach guard: the reveal hit"
        );
        assert_eq!(hit_state.last_parent_target_missing_reason, None);

        let mut whiff_state = GameState::new_two_player(42);
        library_card(&mut whiff_state, CardId(1), "Forest", CoreType::Land);
        library_card(&mut whiff_state, CardId(2), "Island", CoreType::Land);
        whiff_state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::Dig);
        resolve(&mut whiff_state, &ability, &mut Vec::new()).unwrap();
        assert_eq!(whiff_state.last_revealed_ids.len(), 2, "reach guard: whiff");
        assert_eq!(
            whiff_state.last_parent_target_missing_reason,
            Some(ParentTargetMissingReason::RevealUntil)
        );
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
                rest_order: DigRestOrder::Preserve,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: None,
                enters_under: None,
                kept_destination_if: None,
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
                rest_order: DigRestOrder::Preserve,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: None,
                enters_under: None,
                kept_destination_if: None,
            },
            targets,
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn reveal_until_keep_each_collects_multiple_matches() {
        let mut state = GameState::new_two_player(42);

        let instant = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let forest = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let mountain = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&mountain)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let ability = ResolvedAbility::new(
            Effect::RevealUntil {
                player: TargetFilter::Controller,
                filter: TargetFilter::Typed(crate::types::ability::TypedFilter::land()),
                count: crate::types::ability::QuantityExpr::Fixed { value: 2 },
                matched_disposition: RevealUntilDisposition::KeepEach,
                kept_destination: Zone::Battlefield,
                rest_destination: Zone::Library,
                rest_order: DigRestOrder::Preserve,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                kept_optional_to: None,
                enters_under: None,
                kept_destination_if: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&forest));
        assert!(state.battlefield.contains(&mountain));
        assert!(state.objects[&forest].tapped);
        assert!(state.objects[&mountain].tapped);
        assert!(state.players[0].library.contains(&instant));
        assert!(!state.players[0].library.contains(&forest));
        assert!(!state.players[0].library.contains(&mountain));
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
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
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
                    rest_order: DigRestOrder::Preserve,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    kept_optional_to: Some(Zone::Battlefield),
                    enters_under: None,
                    kept_destination_if: None,
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
                rest_order: DigRestOrder::Preserve,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: Some(Zone::Library),
                enters_under: None,
                kept_destination_if: None,
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
                rest_order: DigRestOrder::Preserve,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: Some(Zone::Battlefield),
                enters_under: None,
                kept_destination_if: None,
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

    /// CR 608.2c + CR 608.2f: with several matched cards, each card's
    /// `kept_destination_if` route is decided on its own characteristics, and
    /// the differently-routed cards each reach their own destination.
    /// The count is reached before the last library card is revealed.
    #[test]
    fn reveal_until_multi_hit_routes_each_hit_by_kept_destination_if() {
        let mut state = GameState::new_two_player(42);
        let creature_filter = TargetFilter::Typed(crate::types::ability::TypedFilter::creature());
        let land_filter = TargetFilter::Typed(crate::types::ability::TypedFilter::land());

        // Library top→bottom: creature (hit), Shock (miss), land (hit), Island.
        let creature = library_card(&mut state, CardId(1), "Bear", CoreType::Creature);
        let shock = library_card(&mut state, CardId(2), "Shock", CoreType::Instant);
        let land = library_card(&mut state, CardId(3), "Forest", CoreType::Land);
        let island = library_card(&mut state, CardId(4), "Island", CoreType::Land);

        let mut ability = make_reveal_until_ability(
            PlayerId(0),
            TargetFilter::Or {
                filters: vec![creature_filter, land_filter.clone()],
            },
            Zone::Hand,
            Zone::Graveyard,
        );
        if let Effect::RevealUntil {
            count,
            kept_destination_if,
            ..
        } = &mut ability.effect
        {
            *count = crate::types::ability::QuantityExpr::Fixed { value: 2 };
            *kept_destination_if = Some((Box::new(land_filter), Zone::Battlefield));
        }
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&creature));
        assert!(state.battlefield.contains(&land));
        assert_eq!(state.objects[&shock].zone, Zone::Graveyard);
        assert_eq!(state.objects[&island].zone, Zone::Library);
        assert_eq!(state.players[0].library.front().copied(), Some(island));
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 608.2c + CR 608.2f: a library/library reveal whose hit carries a
    /// per-card `kept_destination_if` route is not a whole-pile library move —
    /// the matching hit takes its conditional destination while the misses
    /// still go to the bottom of the library.
    #[test]
    fn reveal_until_library_piles_honor_kept_destination_if_route() {
        let mut state = GameState::new_two_player(42);
        let land_filter = TargetFilter::Typed(crate::types::ability::TypedFilter::land());

        // Library top→bottom: Shock (miss), Forest (hit), Bear (unrevealed).
        let shock = library_card(&mut state, CardId(1), "Shock", CoreType::Instant);
        let forest = library_card(&mut state, CardId(2), "Forest", CoreType::Land);
        let bear = library_card(&mut state, CardId(3), "Bear", CoreType::Creature);

        let mut ability = make_reveal_until_ability(
            PlayerId(0),
            land_filter.clone(),
            Zone::Library,
            Zone::Library,
        );
        if let Effect::RevealUntil {
            kept_destination_if,
            ..
        } = &mut ability.effect
        {
            *kept_destination_if = Some((Box::new(land_filter), Zone::Battlefield));
        }
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.battlefield.contains(&forest),
            "the land hit takes its kept_destination_if route onto the battlefield"
        );
        assert_eq!(state.objects[&forest].zone, Zone::Battlefield);
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            vec![bear, shock],
            "the miss goes to the bottom, under the unrevealed card"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 110.2a: an `enters_under` controller override (Tasha, Unholy
    /// Archmage's "under your control") rides every request in the kept-set
    /// batch — each matched card enters under the resolving controller while
    /// its owner is unchanged.
    #[test]
    fn reveal_until_multi_hit_enters_under_override_keeps_owner() {
        let mut state = GameState::new_two_player(42);
        let creature_filter = TargetFilter::Typed(crate::types::ability::TypedFilter::creature());

        // P1's library top→bottom: creature (hit), Forest (miss), creature (hit).
        let mut opp_library_card = |card: CardId, name: &str, core: CoreType| {
            let id = create_object(
                &mut state,
                card,
                PlayerId(1),
                name.to_string(),
                Zone::Library,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(core);
            id
        };
        let first = opp_library_card(CardId(1), "Bear", CoreType::Creature);
        let forest = opp_library_card(CardId(2), "Forest", CoreType::Land);
        let second = opp_library_card(CardId(3), "Wolf", CoreType::Creature);

        let mut ability = make_reveal_until_ability_with_player(
            PlayerId(0),
            TargetFilter::Player,
            vec![TargetRef::Player(PlayerId(1))],
            creature_filter,
            Zone::Battlefield,
            Zone::Graveyard,
        );
        if let Effect::RevealUntil {
            count,
            enters_under,
            ..
        } = &mut ability.effect
        {
            *count = crate::types::ability::QuantityExpr::Fixed { value: 2 };
            *enters_under = Some(crate::types::ability::ControllerRef::You);
        }
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for id in [first, second] {
            assert!(state.battlefield.contains(&id));
            assert_eq!(state.objects[&id].controller, PlayerId(0));
            assert_eq!(state.objects[&id].owner, PlayerId(1));
        }
        assert_eq!(state.objects[&forest].zone, Zone::Graveyard);
        assert!(state.players[1].graveyard.contains(&forest));
    }
}
