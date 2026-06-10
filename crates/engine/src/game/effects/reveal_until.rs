use rand::seq::SliceRandom;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

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
        kept_destination,
        rest_destination,
        enter_tapped,
        enters_attacking,
        kept_optional_to,
    ) = match &ability.effect {
        Effect::RevealUntil {
            player,
            filter,
            kept_destination,
            rest_destination,
            enter_tapped,
            enters_attacking,
            kept_optional_to,
        } => (
            player,
            filter,
            *kept_destination,
            *rest_destination,
            *enter_tapped,
            *enters_attacking,
            *kept_optional_to,
        ),
        _ => return Err(EffectError::MissingParam("RevealUntil".to_string())),
    };

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
        match kept_destination {
            Zone::Hand => {
                zones::move_to_zone(state, hit, Zone::Hand, events);
            }
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
                // CR 701.20a: a kept card sent to the library (2 cards) is a
                // placement, not a redirect-eligible move — keep the raw mover
                // (no `Moved` class targets the library; routing through the
                // pipeline's placement arm would gain nothing).
                zones::move_to_zone(state, hit, Zone::Library, events);
            }
            other => {
                // CR 614.6: a kept card sent to the graveyard (4 cards) or exile
                // routes through the pipeline so a `Moved` graveyard→exile
                // redirect (Rest in Peace / Leyline of the Void) fires on it. On a
                // CR 616.1 ordering pause, defer the rest-pile move + marker clear
                // + `EffectResolved` onto a `RevealRestPile` completion (the same
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

/// CR 701.20a: Move the non-matching ("rest") pile to `rest_destination`. Single
/// authority for rest-pile placement — used by both the synchronous resolver path
/// and the `RevealUntilKeptChoice` handler (which, on decline, may add the hit
/// card to `cards` so it joins the random-order shuffle).
pub(crate) fn move_rest(
    state: &mut GameState,
    cards: &[ObjectId],
    rest_destination: Zone,
    events: &mut Vec<GameEvent>,
) {
    move_rest_then(state, cards, rest_destination, None, events);
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
/// A `Zone::Library` rest pile keeps the random-order shuffle-to-bottom
/// (`shuffle_to_bottom`, CR 701.20a "in a random order") and runs the completion
/// inline: a library reposition has no `Moved`-redirect class to consult (zero
/// `destination_zone(Library)` defs in the pool) and cannot pause, so routing it
/// through the pipeline's placement arm would gain nothing and lose the shuffle.
pub(crate) fn move_rest_then(
    state: &mut GameState,
    cards: &[ObjectId],
    rest_destination: Zone,
    completion: Option<BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> zone_pipeline::BatchMoveResult {
    match rest_destination {
        Zone::Library => {
            // "on the bottom of your library in a random order"
            shuffle_to_bottom(state, cards, events);
            if let Some(completion) = completion {
                crate::game::engine_resolution_choices::run_batch_completion(
                    state, completion, events,
                );
            }
            zone_pipeline::BatchMoveResult::Done
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

/// Put cards on the bottom of the player's library in random order.
//
// Zone-pipeline bucketing: `move_to_library_position` is a library-placement
// SIBLING raw mover (PLAN §0 / §5 "library-placement sibling treatment").
// Migrating it onto `move_object`'s placement arm is gated on completing that
// arm's consult (it is a Phase-A stub that delegates straight to
// `move_to_library_at_index`, skipping the replacement consult). That completion
// is DEFERRED: no `Moved` replacement in the card pool targets
// `destination_zone(Library)` (verified: 25 Battlefield / 17 Graveyard / 2 Exile
// destinations, zero Library; reproduce with
//   rg -o 'destination_zone\(Zone::\w+\)' crates/engine/src | sort | uniq -c
// — re-run before lifting this deferral), so the consult is a guaranteed no-op today, and
// completing it correctly requires gating the CR 701.24a delivery-tail
// auto-shuffle on placement-absence across the shared delivery signatures — a
// cross-cutting change with a silent-randomization landmine for zero current
// correctness gain. A library→bottom reposition is not "put into a
// graveyard/exile/hand", so nothing is skipped by staying on the raw sibling.
fn shuffle_to_bottom(state: &mut GameState, cards: &[ObjectId], events: &mut Vec<GameEvent>) {
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
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

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
                kept_destination,
                rest_destination,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: None,
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
                kept_destination,
                rest_destination,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: None,
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
                    kept_destination: Zone::Hand,
                    rest_destination: Zone::Library,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    kept_optional_to: Some(Zone::Battlefield),
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
                kept_destination: Zone::Hand,
                rest_destination: Zone::Library,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                kept_optional_to: Some(Zone::Battlefield),
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
