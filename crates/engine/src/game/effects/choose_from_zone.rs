use rand::seq::IndexedRandom; // rand 0.9: `choose_multiple` on `[T]` lives here.

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::players;
use crate::types::ability::{
    ChooseFromZoneConstraint, Chooser, Effect, EffectError, EffectKind, ForEachCategoryAction,
    ParentTargetMissingReason, ResolvedAbility, TargetFilter, TargetRef, ZoneOwner,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ResolvingTriggerContext, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 608.2d: Choose card(s) from a tracked set — player selects from exiled/revealed cards.
/// The available cards come from the most recent tracked set recorded by the parent effect
/// (e.g., ChangeZone to exile). The `chooser` field determines whether the controller or
/// an opponent makes the selection.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, zone, additional_zones, zone_owner, filter, chooser, up_to, constraint) =
        match &ability.effect {
            Effect::ChooseFromZone {
                count,
                zone,
                additional_zones,
                zone_owner,
                filter,
                chooser,
                up_to,
                constraint,
                ..
            } => (
                *count as usize,
                *zone,
                additional_zones.clone(),
                *zone_owner,
                filter.clone(),
                *chooser,
                *up_to,
                constraint.clone(),
            ),
            _ => return Err(EffectError::MissingParam("ChooseFromZone".to_string())),
        };

    // CR 101.4 + CR 608.2c: "For each player, choose ... in that player's zone"
    // iterates every player in APNAP order, parking one choice per player and
    // accumulating each pick into the chain's tracked set. Routed here before
    // the single-pool path so the per-player prompts never collapse into one
    // candidate scan. Building block for Breach the Multiverse.
    // CR 102.2: `EachOpponent` is the same iteration with the controller
    // excluded ("for each OTHER player" — Kaya, Spirits' Justice).
    if matches!(zone_owner, ZoneOwner::EachPlayer | ZoneOwner::EachOpponent) {
        let players = if matches!(zone_owner, ZoneOwner::EachOpponent) {
            crate::game::players::apnap_order(state)
                .into_iter()
                .filter(|&p| p != ability.controller)
                .collect()
        } else {
            crate::game::players::apnap_order(state)
        };
        // No pick has accumulated yet — the first one must start a fresh set.
        return prompt_next_each_player(state, ability, players, false, events);
    }

    let cards = resolve_candidate_cards(
        state,
        ability,
        zone,
        &additional_zones,
        zone_owner,
        filter.as_ref(),
    )?;

    // CR 608.2d: If there are no objects to choose from, skip the choice
    // (a player can't choose an option that's illegal or impossible).
    if cards.is_empty() || count == 0 {
        state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::ChooseFromZone);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseFromZone,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let clamped_count = count.min(cards.len());

    // CR 608.2d: Determine who makes the choice.
    let choosing_player = resolve_chooser(state, ability, chooser);

    // CR 608.2: An ability's resolution is a single ongoing process. This
    // interactive pause makes `stack::resolve_top` run to completion and
    // unconditionally clear the live, resolution-scoped trigger context; preserve
    // it here (this site runs inside `execute_effect`, before that clear) so an
    // `EventContextAmount` ("that many") sub_ability continuation resolves the
    // triggering event's amount after the pause (Amy Pond). Restored by the
    // `ChooseFromZoneChoice` handler around the continuation drain. Set
    // unconditionally on every single-pool raise: `capture` yields `None` for a
    // non-trigger ChooseFromZone (activated/spell), so a stale value from a prior
    // resolution can never carry over; consumed by `.take()` in the handler.
    state.pending_choose_zone_trigger_context = ResolvingTriggerContext::capture(state);

    state.waiting_for = WaitingFor::ChooseFromZoneChoice {
        player: choosing_player,
        cards,
        count: clamped_count,
        up_to,
        constraint,
        source_id: ability.source_id,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseFromZone,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 608.2c + CR 105.1 / CR 205.2a / CR 122.1: Resolve an
/// `Effect::ForEachCategory` iteration ("for each color/card type, …"). Iterates
/// the category's members in printed order; per-member body is either pool exile
/// (Sanar) or battlefield counter placement (Call the Spirit Dragons).
pub fn resolve_for_each_category(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (category, action) = match &ability.effect {
        Effect::ForEachCategory {
            category, action, ..
        } => (*category, action),
        _ => return Err(EffectError::MissingParam("ForEachCategory".to_string())),
    };
    let pool = match action {
        ForEachCategoryAction::ExileFromPool { .. } => resolve_category_pool(state, ability),
        ForEachCategoryAction::PutCounter { target, .. } => {
            resolve_put_counter_pool(state, ability, target)
        }
    };
    super::publish_fresh_tracked_set(state, Vec::new());
    let member_filters = match action {
        ForEachCategoryAction::PutCounter { target, .. } => category
            .member_filters()
            .into_iter()
            .map(|member| TargetFilter::And {
                filters: vec![target.clone(), member],
            })
            .collect(),
        ForEachCategoryAction::ExileFromPool { .. } => category.member_filters(),
    };
    prompt_next_category_member(state, ability, &pool, member_filters, events)
}

/// Deprecated alias kept for call-site clarity during migration — dispatches to
/// [`resolve_for_each_category`].
pub fn resolve_for_each_category_put_counter(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    resolve_for_each_category(state, ability, events)
}

/// CR 608.2c: Park the next category member's `ChooseFromZoneChoice` prompt for
/// an `Effect::ForEachCategoryExile`. Members whose pool holds no matching card
/// are skipped (CR 608.2c — nothing to exile of that color/type). When no member
/// remains, emits the resolution event so the parked continuation runs. Drives
/// both the initial call from `resolve_for_each_category` and each resumed call
/// from `drain_pending_per_category_zone_choice`.
fn prompt_next_category_member(
    state: &mut GameState,
    ability: &ResolvedAbility,
    pool: &[ObjectId],
    mut remaining_member_filters: Vec<TargetFilter>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (zone, chooser, up_to, put_counter) = match &ability.effect {
        Effect::ForEachCategory {
            chooser,
            action: ForEachCategoryAction::ExileFromPool { zone, up_to },
            ..
        } => (*zone, *chooser, *up_to, None),
        Effect::ForEachCategory {
            chooser,
            action:
                ForEachCategoryAction::PutCounter {
                    counter_type,
                    count,
                    ..
                },
            ..
        } => (
            Zone::Battlefield,
            *chooser,
            false,
            Some((counter_type.clone(), count.clone())),
        ),
        _ => {
            return Err(EffectError::MissingParam(
                "ForEachCategoryIteration".to_string(),
            ))
        }
    };

    while !remaining_member_filters.is_empty() {
        let member_filter = remaining_member_filters.remove(0);
        let cards = filter_category_pool(state, ability, pool, zone, &member_filter);
        if cards.is_empty() {
            continue;
        }

        if let Some((counter_type, count)) = put_counter.as_ref() {
            if cards.len() == 1 {
                let object_id = cards[0];
                let count_val =
                    crate::game::quantity::resolve_quantity_with_targets(state, count, ability)
                        .max(0) as u32;
                crate::game::effects::counters::apply_counter_addition(
                    state,
                    ability.controller,
                    object_id,
                    counter_type.clone(),
                    count_val,
                    events,
                );
                publish_tracked_set_unique(state, &[object_id]);
                continue;
            }
        }

        // CR 608.2d: "you may exile" → 0..=1 of that member; `up_to` is true.
        let count = 1usize;
        let choosing_player = resolve_chooser(state, ability, chooser);

        state.waiting_for = WaitingFor::ChooseFromZoneChoice {
            player: choosing_player,
            cards,
            count,
            up_to,
            constraint: None,
            source_id: ability.source_id,
        };
        state.pending_per_category_zone_choice =
            Some(crate::types::game_state::PendingPerCategoryZoneChoice {
                ability: Box::new(ability.clone()),
                pool: pool.to_vec(),
                remaining_member_filters,
            });
        return Ok(());
    }

    // CR 608.2c: No member had an eligible card — emit the resolution event so
    // the parked continuation still runs.
    let kind = match &ability.effect {
        Effect::ForEachCategory {
            action: ForEachCategoryAction::PutCounter { .. },
            ..
        } => EffectKind::PutCounter,
        Effect::ForEachCategory { .. } => EffectKind::ChooseFromZone,
        _ => EffectKind::ChooseFromZone,
    };
    events.push(GameEvent::EffectResolved {
        kind,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 608.2c: Resume an `Effect::ForEachCategoryExile` iteration after the
/// current member's pick resolves. Exiles the chosen card and extends the
/// chain's "cards exiled this way" tracked set (started empty by
/// `resolve_for_each_category`), then prompts the next member. Mirrors
/// `drain_pending_per_player_zone_choice`.
pub(crate) fn drain_pending_per_category_zone_choice(
    state: &mut GameState,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> crate::game::zone_pipeline::BatchMoveResult {
    let Some(pending) = state.pending_per_category_zone_choice.take() else {
        return crate::game::zone_pipeline::BatchMoveResult::Done;
    };
    let crate::types::game_state::PendingPerCategoryZoneChoice {
        ability,
        pool,
        remaining_member_filters,
    } = pending;

    if matches!(
        &ability.effect,
        Effect::ForEachCategory {
            action: ForEachCategoryAction::ExileFromPool { .. },
            ..
        }
    ) {
        // CR 701.13a + CR 614.1 + CR 616.1: Each chosen card's exile is an
        // effect-owned zone-change event. Keep the tracked-set extension and
        // next-member prompt on the batch tail so neither can run before a
        // replacement choice settles the exile.
        let requests = chosen
            .iter()
            .map(|&card_id| {
                crate::game::zone_pipeline::ZoneMoveRequest::effect(
                    card_id,
                    Zone::Exile,
                    ability.source_id,
                )
            })
            .collect();
        return crate::game::zone_pipeline::move_objects_simultaneously_then(
            state,
            requests,
            Some(
                crate::types::game_state::BatchCompletion::ForEachCategoryExileComplete {
                    ability,
                    pool,
                    remaining_member_filters,
                    chosen: chosen.to_vec(),
                },
            ),
            events,
        );
    }

    if let Effect::ForEachCategory {
        action:
            ForEachCategoryAction::PutCounter {
                counter_type,
                count,
                ..
            },
        ..
    } = &ability.effect
    {
        let count_val = crate::game::quantity::resolve_quantity_with_targets(state, count, &ability)
            .max(0) as u32;
        for &card_id in chosen {
            crate::game::effects::counters::apply_counter_addition(
                state,
                ability.controller,
                card_id,
                counter_type.clone(),
                count_val,
                events,
            );
        }
        if !chosen.is_empty() {
            publish_tracked_set_unique(state, chosen);
        }
    }

    let _ = prompt_next_category_member(state, &ability, &pool, remaining_member_filters, events);
    crate::game::zone_pipeline::BatchMoveResult::Done
}

/// CR 608.2c: Complete one settled `ForEachCategoryExile` member. The typed
/// batch tail owns both the tracked-set extension and the next-member prompt so
/// a CR 616.1 replacement choice resolves before the iteration advances.
pub(crate) fn complete_per_category_exile(
    state: &mut GameState,
    ability: Box<ResolvedAbility>,
    pool: Vec<ObjectId>,
    remaining_member_filters: Vec<TargetFilter>,
    chosen: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) {
    if !chosen.is_empty() {
        super::publish_tracked_set(state, chosen);
    }
    let _ = prompt_next_category_member(state, &ability, &pool, remaining_member_filters, events);
}

fn publish_tracked_set_unique(state: &mut GameState, ids: &[ObjectId]) {
    let unique: Vec<ObjectId> = ids
        .iter()
        .copied()
        .filter(|id| {
            state
                .chain_tracked_set_id
                .and_then(|set_id| state.tracked_object_sets.get(&set_id))
                .is_none_or(|set| !set.contains(id))
        })
        .collect();
    if !unique.is_empty() {
        super::publish_tracked_set(state, unique);
    }
}

fn resolve_put_counter_pool(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    let filter_ctx = FilterContext::from_ability(ability);
    crate::game::targeting::zone_object_ids(state, Zone::Battlefield)
        .into_iter()
        .filter(|id| matches_target_filter(state, *id, target, &filter_ctx))
        .collect()
}

/// CR 608.2c: Snapshot the revealed/exiled pool for a `ForEachCategoryExile`
/// iteration. Priority mirrors `resolve_candidate_cards`: the chain's tracked
/// set, then the latest published tracked set, then explicit object targets.
/// Captured ONCE at iteration start so per-member filtering reads a stable pool.
fn resolve_category_pool(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    chain_tracked_set_cards(state)
        .or_else(|| {
            crate::game::targeting::latest_tracked_set_id(state)
                .and_then(|id| state.tracked_object_sets.get(&id).cloned())
        })
        .unwrap_or_else(|| {
            ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect()
        })
}

/// CR 608.2c + CR 105.1 / CR 205.2a: Candidate cards for one category member —
/// the iteration's pool snapshot restricted to cards matching `member_filter`
/// (the bound color/type) AND still in `zone` (a card already exiled by an
/// earlier member has left `zone` and cannot be re-exiled).
fn filter_category_pool(
    state: &GameState,
    ability: &ResolvedAbility,
    pool: &[ObjectId],
    zone: Zone,
    member_filter: &TargetFilter,
) -> Vec<ObjectId> {
    let filter_ctx = FilterContext::from_ability(ability);
    pool.iter()
        .copied()
        .filter(|id| state.objects.get(id).is_some_and(|obj| obj.zone == zone))
        .filter(|id| matches_target_filter(state, *id, member_filter, &filter_ctx))
        .collect()
}

/// CR 608.2d (override) + CR 701.9b (analogous): Resolve a random
/// `Effect::ChooseFromZone` in place ("choose one of them at random" — River
/// Song's Diary). Picks `count` distinct cards uniformly via the seeded RNG and
/// sets them as the resolving ability's `targets`, so the chain propagates them
/// to the sub-ability (`CastFromZone { target: ParentTarget }`) via
/// `apply_parent_chain_context` exactly as the interactive answer handler sets
/// `cont.chain.targets`. No interactive `WaitingFor::ChooseFromZoneChoice` is
/// raised. Returns `true` when this was a random `ChooseFromZone` (and was
/// resolved, including the do-nothing empty-pool case per CR 609.3); `false`
/// otherwise. Emits `EffectResolved` itself when it resolves.
pub(crate) fn resolve_random_in_chain(
    state: &mut GameState,
    ability: &mut ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> bool {
    let (count, zone, additional_zones, zone_owner, filter) = match &ability.effect {
        Effect::ChooseFromZone {
            count,
            zone,
            additional_zones,
            zone_owner,
            filter,
            selection,
            ..
        } if selection.is_random() => (
            *count as usize,
            *zone,
            additional_zones.clone(),
            *zone_owner,
            filter.clone(),
        ),
        _ => return false,
    };

    let cards = resolve_candidate_cards(
        state,
        ability,
        zone,
        &additional_zones,
        zone_owner,
        filter.as_ref(),
    )
    .unwrap_or_default();

    // CR 609.3: An empty pool (or count 0) does nothing; the chain then skips
    // any continuation that depends on the missing pick.
    if cards.is_empty() || count == 0 {
        state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::ChooseFromZone);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseFromZone,
            source_id: ability.source_id,
            subject: None,
        });
        return true;
    }

    // CR 608.2d (override): the game selects `count` distinct cards at random.
    let clamped = count.min(cards.len());
    let picked: Vec<ObjectId> = cards
        .choose_multiple(&mut state.rng, clamped)
        .copied()
        .collect();
    ability.targets = picked.iter().map(|&id| TargetRef::Object(id)).collect();

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseFromZone,
        source_id: ability.source_id,
        subject: None,
    });
    true
}

/// CR 101.4 + CR 608.2c: Park the next eligible player's `ChooseFromZoneChoice`
/// for a `ChooseFromZone { zone_owner: EachPlayer }` iteration, stashing the
/// players still to be prompted in `pending_per_player_zone_choice`. Players
/// whose zone holds no matching candidate are skipped (CR 608.2c — there's
/// nothing to choose). When no eligible player remains, the iteration is
/// disposed (the parked `pending_continuation` then runs). Drives both the
/// initial call from `resolve` and each resumed call from
/// `drain_pending_per_player_zone_choice`.
fn prompt_next_each_player(
    state: &mut GameState,
    ability: &ResolvedAbility,
    mut remaining_players: Vec<PlayerId>,
    accumulated: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, zone, additional_zones, filter, chooser, up_to, constraint) = match &ability.effect
    {
        Effect::ChooseFromZone {
            count,
            zone,
            additional_zones,
            filter,
            chooser,
            up_to,
            constraint,
            ..
        } => (
            *count as usize,
            *zone,
            additional_zones.clone(),
            filter.clone(),
            *chooser,
            *up_to,
            constraint.clone(),
        ),
        _ => return Err(EffectError::MissingParam("ChooseFromZone".to_string())),
    };

    while let Some(owner) = remaining_players.first().copied() {
        remaining_players.remove(0);

        let cards = collect_player_zone_cards(
            state,
            ability,
            owner,
            zone,
            &additional_zones,
            filter.as_ref(),
        );
        if cards.is_empty() || count == 0 {
            continue;
        }

        let clamped_count = count.min(cards.len());
        // CR 101.4 + CR 608.2c: For "for each player, choose ...", the spell's controller is
        // the chooser regardless of whose zone is scanned (Breach the
        // Multiverse). `Chooser::Opponent` would route to an opponent; honor it.
        let choosing_player = resolve_chooser(state, ability, chooser);

        state.waiting_for = WaitingFor::ChooseFromZoneChoice {
            player: choosing_player,
            cards,
            count: clamped_count,
            up_to,
            constraint,
            source_id: ability.source_id,
        };
        state.pending_per_player_zone_choice =
            Some(crate::types::game_state::PendingPerPlayerZoneChoice {
                ability: Box::new(ability.clone()),
                remaining_players,
                accumulated,
            });
        return Ok(());
    }

    // CR 101.4 + CR 608.2c: No iterated player had an eligible card. When
    // `accumulated == false` this is the FIRST resolution of the iteration (no
    // player was ever prompted), so it MUST rebind a FRESH (empty) chain tracked
    // set before the parked continuation runs — mirroring the first-resolution
    // rebind in `drain_pending_per_player_zone_choice`. Otherwise an EARLIER
    // same-chain producer's tracked set (e.g. Breach the Multiverse's preceding
    // mill) stays bound and a downstream `ChangeZoneAll { TrackedSet }` over-acts
    // on that stale set instead of this iteration's (empty) picks: "those chosen
    // cards" must mean exactly the cards chosen by THIS iteration. When
    // `accumulated == true` an earlier player already rebound a fresh set for this
    // iteration — leave it bound, do not clobber it.
    if !accumulated {
        super::publish_fresh_tracked_set(state, Vec::new());
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseFromZone,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 101.4 + CR 608.2c: Resume a per-player `ChooseFromZone { EachPlayer }`
/// iteration after the current player's pick resolves. Accumulates the chosen
/// cards into the resolution chain's tracked set (a fresh set on the first
/// pick, extended on each subsequent pick) so a downstream "put those cards
/// onto the battlefield" reads exactly the cards chosen across all players,
/// then prompts the next eligible player. Mirrors
/// `vote::drain_pending_vote_ballot_iteration`.
pub(crate) fn drain_pending_per_player_zone_choice(
    state: &mut GameState,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) {
    let Some(pending) = state.pending_per_player_zone_choice.take() else {
        return;
    };

    let crate::types::game_state::PendingPerPlayerZoneChoice {
        ability,
        remaining_players,
        accumulated,
    } = pending;

    // CR 603.7 + CR 608.2c: The FIRST resolution of this per-player iteration
    // STARTS a fresh chosen-card set — even when that first player declines (an
    // empty "up to one" pick). It must NOT extend or inherit an earlier
    // producer's tracked set: Breach the Multiverse mills first (publishing a
    // "Milled" set), so extending here would reanimate the milled cards alongside
    // the chosen ones ("those cards" = the chosen cards only, CR 608.2c).
    // `publish_fresh_tracked_set` allocates a new (possibly empty) set and rebinds
    // `chain_tracked_set_id`, overwriting the milled binding; binding it on the
    // first resolution regardless of emptiness guarantees a downstream
    // `ChangeZoneAll { TrackedSet }` acts only on this iteration's picks (an
    // all-declined loop → empty fresh set → mass move acts on nothing) instead of
    // a stale prior producer's set. Every LATER pick extends that fresh set so all
    // players' chosen cards unify under one "those cards" reference. The Cyberman /
    // impulse "milled this way" path is unaffected — it never uses this per-player
    // drain.
    let accumulated = if accumulated {
        if !chosen.is_empty() {
            super::publish_tracked_set(state, chosen.to_vec());
        }
        true
    } else {
        super::publish_fresh_tracked_set(state, chosen.to_vec());
        true
    };

    let _ = prompt_next_each_player(state, &ability, remaining_players, accumulated, events);
}

/// CR 101.4: Candidate cards in a SINGLE player's zone(s) for a per-player
/// iteration, applying the effect's filter. Unlike `collect_direct_zone_cards`,
/// the owner is supplied explicitly (the iterating player), so the tracked-set
/// short-circuit in `resolve_candidate_cards` is bypassed — each player's
/// graveyard is scanned independently.
fn collect_player_zone_cards(
    state: &GameState,
    ability: &ResolvedAbility,
    owner: PlayerId,
    zone: Zone,
    additional_zones: &[Zone],
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let filter_ctx = FilterContext::from_ability(ability);
    let mut zones = Vec::with_capacity(1 + additional_zones.len());
    zones.push(zone);
    zones.extend_from_slice(additional_zones);
    zones
        .into_iter()
        .flat_map(|zone| object_ids_in_player_zone(state, owner, zone))
        .filter(|id| {
            filter.is_none_or(|filter| matches_target_filter(state, *id, filter, &filter_ctx))
        })
        .collect()
}

/// CR 608.2c + CR 608.2d + CR 603.7: Resolve the candidate card pool for a
/// tracked-set pick.
///
/// Priority order:
/// 1. The current resolution chain's tracked set, including an empty set.
/// 2. The latest non-empty tracked set from any prior publish in this game.
/// 3. Explicit `TargetRef::Object` targets on the ability.
/// 4. Direct zone scan (`zone` + `additional_zones`).
fn resolve_candidate_cards(
    state: &GameState,
    ability: &ResolvedAbility,
    zone: Zone,
    additional_zones: &[Zone],
    zone_owner: ZoneOwner,
    filter: Option<&TargetFilter>,
) -> Result<Vec<ObjectId>, EffectError> {
    if let Some(cards) = chain_tracked_set_cards(state) {
        return Ok(cards);
    }

    let cards = crate::game::targeting::latest_tracked_set_id(state)
        .and_then(|id| state.tracked_object_sets.get(&id).cloned())
        .unwrap_or_else(|| {
            ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect()
        });

    let cards = if cards.is_empty() {
        collect_direct_zone_cards(state, ability, zone, additional_zones, zone_owner, filter)?
    } else {
        cards
    };

    Ok(cards)
}

fn chain_tracked_set_cards(state: &GameState) -> Option<Vec<ObjectId>> {
    let chain_id = state.chain_tracked_set_id?;
    state.tracked_object_sets.get(&chain_id).cloned()
}

fn collect_direct_zone_cards(
    state: &GameState,
    ability: &ResolvedAbility,
    zone: Zone,
    additional_zones: &[Zone],
    zone_owner: ZoneOwner,
    filter: Option<&TargetFilter>,
) -> Result<Vec<ObjectId>, EffectError> {
    let filter_ctx = FilterContext::from_ability(ability);
    let mut zones = Vec::with_capacity(1 + additional_zones.len());
    zones.push(zone);
    zones.extend_from_slice(additional_zones);

    // CR 701.38d: For ScopedPlayer on Battlefield, scan ALL battlefield
    // permanents and rely on the filter (FilterProp::Owned { ScopedPlayer })
    // to restrict to objects owned by the voter. This is necessary because
    // "owned by" is distinct from "controlled by" — the voter may own
    // permanents that another player controls.
    if matches!(zone_owner, ZoneOwner::ScopedPlayer)
        && zones.iter().any(|z| matches!(z, Zone::Battlefield))
    {
        return Ok(state
            .battlefield
            .iter()
            .copied()
            .filter(|id| state.objects.get(id).is_some_and(|obj| obj.is_phased_in()))
            .filter(|id| {
                filter.is_none_or(|filter| matches_target_filter(state, *id, filter, &filter_ctx))
            })
            .collect());
    }

    // CR 400.1 + CR 607.2a: For AllOwners, scan the referenced zone(s) across
    // EVERY player and rely entirely on the filter to scope. Exile is a zone
    // shared by all players (CR 400.1), and "exiled with [source]" is a
    // linked-ability reference (CR 607.2a) whose membership ignores ownership,
    // so owner-gating would drop the opponent-owned cards Koh exiled before the
    // filter ever runs. Mirrors the ScopedPlayer-on-Battlefield branch above:
    // scan broadly, let the filter (here ExiledBySource) narrow. Kept general
    // for all zones — the union across players reduces to the whole shared
    // exile zone for Exile and to each player's own zone for per-player zones,
    // with every object counted once (each object has exactly one owner).
    if matches!(zone_owner, ZoneOwner::AllOwners) {
        let owners: Vec<PlayerId> = state.players.iter().map(|p| p.id).collect();
        return Ok(owners
            .into_iter()
            .flat_map(|owner| {
                zones
                    .iter()
                    .flat_map(|&zone| object_ids_in_player_zone(state, owner, zone))
                    .collect::<Vec<_>>()
            })
            .filter(|id| {
                filter.is_none_or(|filter| matches_target_filter(state, *id, filter, &filter_ctx))
            })
            .collect());
    }

    let owner = resolve_zone_owner(state, ability, zone_owner)?;

    Ok(zones
        .into_iter()
        .flat_map(|zone| object_ids_in_player_zone(state, owner, zone))
        .filter(|id| {
            filter.is_none_or(|filter| matches_target_filter(state, *id, filter, &filter_ctx))
        })
        .collect())
}

fn resolve_zone_owner(
    state: &GameState,
    ability: &ResolvedAbility,
    zone_owner: ZoneOwner,
) -> Result<PlayerId, EffectError> {
    match zone_owner {
        ZoneOwner::Controller => Ok(ability.controller),
        ZoneOwner::TargetedPlayer => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Player(player) => Some(*player),
                _ => None,
            })
            .ok_or_else(|| EffectError::MissingParam("ChooseFromZone targeted player".to_string())),
        ZoneOwner::Opponent => players::opponents(state, ability.controller)
            .into_iter()
            .next()
            .ok_or_else(|| EffectError::MissingParam("ChooseFromZone opponent".to_string())),
        // CR 701.38d: The scoped player (voter) supplies the zone.
        ZoneOwner::ScopedPlayer => Ok(ability.scoped_player.unwrap_or(ability.controller)),
        // CR 101.4: `EachPlayer` / `EachOpponent` resolve a *set* of zone
        // owners, not one — they are handled by `prompt_next_each_player`, which
        // scans each iterated player's zone directly and never routes here.
        ZoneOwner::EachPlayer | ZoneOwner::EachOpponent => Err(EffectError::MissingParam(
            "ChooseFromZone EachPlayer/EachOpponent resolves per-player, not via single owner"
                .to_string(),
        )),
        // CR 400.1: `AllOwners` scans a zone shared across EVERY owner, not a
        // single one — it is handled by the early return in
        // `collect_direct_zone_cards` and never routes through this
        // single-owner resolver.
        ZoneOwner::AllOwners => Err(EffectError::MissingParam(
            "ChooseFromZone AllOwners scans all owners, not via single owner".to_string(),
        )),
    }
}

fn object_ids_in_player_zone(state: &GameState, player: PlayerId, zone: Zone) -> Vec<ObjectId> {
    let Some(player_state) = state.players.iter().find(|p| p.id == player) else {
        return Vec::new();
    };

    match zone {
        Zone::Hand => player_state.hand.iter().copied().collect(),
        Zone::Library => player_state.library.iter().copied().collect(),
        Zone::Graveyard => player_state.graveyard.iter().copied().collect(),
        Zone::Exile => state
            .exile
            .iter()
            .copied()
            .filter(|id| state.objects.get(id).is_some_and(|obj| obj.owner == player))
            .collect(),
        Zone::Battlefield => state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.controller == player && obj.is_phased_in())
            })
            .collect(),
        Zone::Stack => state
            .stack
            .iter()
            .filter(|entry| entry.controller == player)
            .map(|entry| entry.id)
            .collect(),
        Zone::Command => Vec::new(),
    }
}

/// CR 608.2c-e: Resolve the `Chooser` enum to an actual `PlayerId`.
/// For `Opponent`, first checks ability targets for a pre-targeted opponent player
/// (handles "target opponent chooses"), then falls back to the first opponent in APNAP order.
fn resolve_chooser(state: &GameState, ability: &ResolvedAbility, chooser: Chooser) -> PlayerId {
    match chooser {
        Chooser::Controller => ability.controller,
        Chooser::Opponent => {
            // Check if an opponent was already targeted by the spell.
            if let Some(targeted_opponent) = ability.targets.iter().find_map(|t| match t {
                TargetRef::Player(id) if *id != ability.controller => Some(*id),
                _ => None,
            }) {
                return targeted_opponent;
            }
            // Fallback: first opponent in APNAP order (CR-correct for 2-player).
            players::opponents(state, ability.controller)
                .into_iter()
                .next()
                .unwrap_or(ability.controller)
        }
    }
}

pub fn selection_satisfies_constraint(
    state: &GameState,
    chosen: &[ObjectId],
    constraint: Option<&ChooseFromZoneConstraint>,
) -> bool {
    match constraint {
        None => true,
        Some(ChooseFromZoneConstraint::DistinctCardTypes { categories }) => {
            selected_cards_cover_distinct_card_types(state, chosen, categories)
        }
    }
}

fn selected_cards_cover_distinct_card_types(
    state: &GameState,
    chosen: &[ObjectId],
    categories: &[CoreType],
) -> bool {
    if chosen.is_empty() {
        return true;
    }
    if chosen.len() > categories.len() {
        return false;
    }

    let card_options: Option<Vec<Vec<usize>>> = chosen
        .iter()
        .map(|id| {
            state.objects.get(id).map(|obj| {
                categories
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, category)| {
                        obj.card_types.core_types.contains(category).then_some(idx)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    let mut card_options = match card_options {
        Some(options) => options,
        None => return false,
    };
    if card_options.iter().any(Vec::is_empty) {
        return false;
    }

    card_options.sort_by_key(Vec::len);
    let mut used = vec![false; categories.len()];
    assign_distinct_categories(&card_options, &mut used, 0)
}

fn assign_distinct_categories(card_options: &[Vec<usize>], used: &mut [bool], idx: usize) -> bool {
    if idx == card_options.len() {
        return true;
    }
    for &category_idx in &card_options[idx] {
        if used[category_idx] {
            continue;
        }
        used[category_idx] = true;
        if assign_distinct_categories(card_options, used, idx + 1) {
            return true;
        }
        used[category_idx] = false;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{TypeFilter, TypedFilter};
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, TrackedSetId};
    use crate::types::zones::Zone;

    /// Regression: `ChooseFromZoneConstraint` must serialize internally tagged
    /// (`{ "type": "DistinctCardTypes", ... }`) so the frontend `CardChoiceModal`
    /// confirm gate — which discriminates on `constraint.type` — can recognize the
    /// constraint. The default external representation left `type` undefined and
    /// permanently disabled the confirm button (e.g. Atraxa, Grand Unifier).
    #[test]
    fn distinct_card_types_constraint_is_internally_tagged() {
        let constraint = ChooseFromZoneConstraint::DistinctCardTypes {
            categories: vec![CoreType::Creature, CoreType::Land],
        };
        let value = serde_json::to_value(&constraint).unwrap();
        assert_eq!(value["type"], "DistinctCardTypes");
        assert_eq!(value["categories"][0], "Creature");
        assert_eq!(value["categories"][1], "Land");
        // Round-trips back to an equal value.
        let back: ChooseFromZoneConstraint = serde_json::from_value(value).unwrap();
        assert_eq!(back, constraint);
    }

    /// CR 608.2 + CR 702.62b + CR 122.1: Amy Pond's combat-damage trigger
    /// ("choose a suspended card you own and remove that many time counters from
    /// it") must resolve `that many` to the combat damage AFTER the interactive
    /// `ChooseFromZone` pause, removing the counters from the CHOSEN card. This is
    /// the deliverable proof: without the `ResolvingTriggerContext` save/restore
    /// the `EventContextAmount` continuation reads `None` → removes 0; without the
    /// §D anaphor rebind the counters strip off Amy instead of the chosen card.
    fn amy_pond_setup(
        time_counters: u32,
        combat_damage: u32,
    ) -> (crate::game::scenario::GameRunner, ObjectId, ObjectId) {
        use crate::game::scenario::GameScenario;
        use crate::game::triggers::process_triggers;
        use crate::types::events::GameEvent;
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaCost;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::phase::Phase::CombatDamage);
        let amy = scenario
            .add_creature(PlayerId(0), "Amy Pond", 2, 2)
            .from_oracle_text(
                "Whenever ~ deals combat damage to a player, choose a suspended card \
                 you own and remove that many time counters from it.",
            )
            .id();
        let mut runner = scenario.build();
        let suspended;
        {
            let state = runner.state_mut();
            state.active_player = PlayerId(0);

            // A suspended card you own in exile with `time_counters` time counters.
            suspended = create_object(
                state,
                CardId(900),
                PlayerId(0),
                "Suspended Spell".to_string(),
                Zone::Exile,
            );
            let obj = state.objects.get_mut(&suspended).unwrap();
            let suspend_kw = Keyword::Suspend {
                count: time_counters,
                cost: ManaCost::generic(2),
            };
            // CR 702.62b: off-battlefield keyword queries read `base_keywords`
            // (the printed face), so the suspended card must carry Suspend there.
            obj.keywords.push(suspend_kw.clone());
            obj.base_keywords.push(suspend_kw);
            obj.counters.insert(CounterType::Time, time_counters);
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.base_card_types.core_types.push(CoreType::Sorcery);

            // Amy deals `combat_damage` combat damage to a player → trigger fires.
            // A SelfRef `DamageDone` trigger listens on the per-source
            // `DamageDealt` event (CR 510.2), not the aggregate
            // `CombatDamageDealtToPlayer` (which is for non-SelfRef observers).
            let event = GameEvent::DamageDealt {
                source_id: amy,
                target: TargetRef::Player(PlayerId(1)),
                amount: combat_damage,
                is_combat: true,
                excess: 0,
            };
            process_triggers(state, std::slice::from_ref(&event));
            crate::game::stack::resolve_top(state, &mut Vec::new());
        }
        (runner, amy, suspended)
    }

    #[test]
    fn amy_pond_removes_combat_damage_time_counters_from_chosen_card_after_pause() {
        use crate::types::actions::GameAction;

        let (mut runner, amy, suspended) = amy_pond_setup(3, 2);

        // The trigger paused on the interactive ChooseFromZone, and the resolving
        // trigger context was captured for the EventContextAmount continuation.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseFromZoneChoice { .. }
            ),
            "expected ChooseFromZoneChoice pause, got {:?}",
            runner.state().waiting_for
        );
        assert!(
            runner.state().pending_choose_zone_trigger_context.is_some(),
            "the resolving trigger context must be captured across the pause"
        );

        crate::game::engine::apply(
            runner.state_mut(),
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![suspended],
            },
        )
        .expect("selecting the suspended card must succeed");

        // EventContextAmount = 2 combat damage → 3 - 2 = 1 left on the CHOSEN card.
        // `== 1` rejects a spurious batched `match_count` of 1 (which would leave 2)
        // and proves the §D rebinding put the counters on the chosen card, not Amy.
        assert_eq!(
            runner.state().objects[&suspended]
                .counters
                .get(&CounterType::Time)
                .copied()
                .unwrap_or(0),
            1,
            "removed exactly the combat-damage amount (2) from the chosen card"
        );
        assert_eq!(
            runner.state().objects[&amy]
                .counters
                .get(&CounterType::Time)
                .copied()
                .unwrap_or(0),
            0,
            "Amy Pond's own counters are untouched"
        );
        assert!(
            runner.state().pending_choose_zone_trigger_context.is_none(),
            "the stash is consumed exactly once"
        );
    }

    #[test]
    fn amy_pond_removes_all_time_counters_when_damage_equals_counters() {
        use crate::types::actions::GameAction;

        let (mut runner, _amy, suspended) = amy_pond_setup(2, 2);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ));

        crate::game::engine::apply(
            runner.state_mut(),
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![suspended],
            },
        )
        .expect("selecting the suspended card must succeed");

        // EventContextAmount = 2 → removes the last 2 of 2 time counters → 0 left
        // (CR 702.62a free-cast on last-counter removal is existing behavior).
        assert_eq!(
            runner.state().objects[&suspended]
                .counters
                .get(&CounterType::Time)
                .copied()
                .unwrap_or(0),
            0,
            "all time counters removed when damage equals the counter count"
        );
    }

    #[test]
    fn resolve_with_controller_chooser() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Exile,
        );

        // Simulate tracked set from parent ChangeZone
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1, card2]);
        state.next_tracked_set_id = 2;

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                up_to,
                constraint,
                ..
            } => {
                assert_eq!(*player, PlayerId(0), "Controller should be the chooser");
                assert_eq!(cards.len(), 2);
                assert_eq!(*count, 1);
                assert!(!up_to);
                assert!(constraint.is_none());
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    /// CR 400.1 + CR 607.2a: `ZoneOwner::AllOwners` scans a shared zone across
    /// EVERY owner and lets the `TargetFilter` perform all scoping — the
    /// per-owner gate in `object_ids_in_player_zone` is bypassed. Building-block
    /// proof for the category "membership is defined by the filter, not by
    /// ownership" (Koh, the Face Stealer: a creature card exiled with Koh, where
    /// Koh typically exiles *opponents'* creatures).
    ///
    /// Discriminating on two axes: (1) an opponent-owned linked card MUST be
    /// offered under `AllOwners` but is DROPPED under the old `Controller` scope
    /// (the bug); (2) an opponent-owned *unlinked* exiled card must be excluded,
    /// proving `AllOwners` does not over-collect — the `ExiledBySource` filter
    /// still narrows the whole-zone scan.
    #[test]
    fn all_owners_scope_enumerates_exile_across_owners() {
        let mut state = GameState::new_two_player(42);

        // The linked-exile source (a Koh-like permanent under the controller).
        let source = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Koh-like Source".to_string(),
            Zone::Battlefield,
        );

        // Two creature cards in the SHARED exile zone (CR 400.1), owned by
        // different players, both linked to `source` (CR 607.2a).
        let mine = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Exiled Face".to_string(),
            Zone::Exile,
        );
        let theirs = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Exiled Face".to_string(),
            Zone::Exile,
        );
        // An opponent-owned exiled card NOT linked to the source — the filter
        // must exclude it even though `AllOwners` scans the whole zone.
        let unlinked = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Unlinked Exiled Card".to_string(),
            Zone::Exile,
        );
        crate::game::exile_links::push_tracked_by_source(&mut state, mine, source);
        crate::game::exile_links::push_tracked_by_source(&mut state, theirs, source);

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::AllOwners,
                filter: Some(TargetFilter::ExiledBySource),
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let filter = TargetFilter::ExiledBySource;

        // AllOwners: BOTH owners' linked cards are offered; the unlinked card is
        // filtered out. The opponent-owned card present here is the fix.
        let all = collect_direct_zone_cards(
            &state,
            &ability,
            Zone::Exile,
            &[],
            ZoneOwner::AllOwners,
            Some(&filter),
        )
        .expect("AllOwners enumerates without routing through a single-owner resolver");
        assert!(
            all.contains(&mine),
            "AllOwners offers the controller's own exiled card"
        );
        assert!(
            all.contains(&theirs),
            "AllOwners offers the OPPONENT-owned exiled card {theirs:?} (the fix)"
        );
        assert!(
            !all.contains(&unlinked),
            "AllOwners excludes the unlinked exiled card {unlinked:?} — the filter still scopes"
        );
        assert_eq!(
            all.len(),
            2,
            "exactly the two linked cards regardless of owner, got {all:?}"
        );

        // Controller (the old scope): the owner gate drops the opponent-owned
        // linked card, leaving only the controller's own — exactly the bug that
        // `AllOwners` fixes. Asserted so a regression into owner-gating fails.
        let controller_only = collect_direct_zone_cards(
            &state,
            &ability,
            Zone::Exile,
            &[],
            ZoneOwner::Controller,
            Some(&filter),
        )
        .expect("Controller scope resolves to a single owner");
        assert_eq!(
            controller_only,
            vec![mine],
            "Controller scope offers only the controller's own exiled card — \
             the opponent-owned linked card {theirs:?} is wrongly dropped"
        );
    }

    #[test]
    fn resolve_with_opponent_chooser() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );

        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1]);
        state.next_tracked_set_id = 2;

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Opponent,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { player, count, .. } => {
                assert_eq!(*player, PlayerId(1), "Opponent should be the chooser");
                assert_eq!(*count, 1);
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn resolve_with_targeted_opponent() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );

        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1]);
        state.next_tracked_set_id = 2;

        // Simulate a targeted opponent (e.g., Gifts Ungiven targeting PlayerId(1))
        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Opponent,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { player, .. } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "Targeted opponent should be the chooser"
                );
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn empty_tracked_set_skips_choice() {
        let mut state = GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Opponent,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should not set ChooseFromZoneChoice — no cards to choose from
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
            "Should skip choice when tracked set is empty"
        );
    }

    #[test]
    fn count_clamped_to_available_cards() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );

        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1]);
        state.next_tracked_set_id = 2;

        // Request 3 but only 1 card available
        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 3,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { count, .. } => {
                assert_eq!(*count, 1, "Count should be clamped to available cards");
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn direct_zone_choice_filters_controller_hand() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Hand,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    ..Default::default()
                })),
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, count, .. } => {
                assert_eq!(*cards, vec![creature]);
                assert_eq!(*count, 1);
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn direct_zone_choice_uses_targeted_players_zones() {
        let mut state = GameState::new_two_player(42);
        let graveyard_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Graveyard Card".to_string(),
            Zone::Graveyard,
        );
        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Hand Card".to_string(),
            Zone::Hand,
        );
        let controller_hand_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Controller Hand Card".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Graveyard,
                additional_zones: vec![Zone::Hand],
                zone_owner: ZoneOwner::TargetedPlayer,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(*cards, vec![graveyard_card, hand_card]);
                assert!(!cards.contains(&controller_hand_card));
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn direct_zone_choice_requires_targeted_player() {
        let mut state = GameState::new_two_player(42);
        let _card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hand Card".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Hand,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::TargetedPlayer,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let err = resolve(&mut state, &ability, &mut events).unwrap_err();
        assert!(
            matches!(err, EffectError::MissingParam(message) if message == "ChooseFromZone targeted player")
        );
    }

    #[test]
    fn distinct_card_type_constraint_accepts_valid_assignment() {
        let mut state = GameState::new_two_player(42);
        let artifact_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Patchwork Automaton".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&artifact_creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact, CoreType::Creature];
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Elvish Mystic".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        assert!(selection_satisfies_constraint(
            &state,
            &[artifact_creature, creature],
            Some(&ChooseFromZoneConstraint::DistinctCardTypes {
                categories: vec![CoreType::Artifact, CoreType::Creature],
            }),
        ));
    }

    #[test]
    fn distinct_card_type_constraint_rejects_duplicate_assignment_only() {
        let mut state = GameState::new_two_player(42);
        let creature_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Elvish Mystic".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        let creature_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        assert!(!selection_satisfies_constraint(
            &state,
            &[creature_a, creature_b],
            Some(&ChooseFromZoneConstraint::DistinctCardTypes {
                categories: vec![CoreType::Artifact, CoreType::Creature],
            }),
        ));
    }

    /// End-to-end regression for Atraxa, Grand Unifier's ETB chain:
    /// RevealTop(10) → ChooseFromZone(DistinctCardTypes) must offer only the
    /// revealed library cards.
    #[test]
    fn atraxa_style_reveal_top_chain_offers_revealed_library_cards() {
        use super::super::resolve_ability_chain;
        use crate::types::ability::TargetFilter;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Atraxa, Grand Unifier".to_string(),
            Zone::Battlefield,
        );

        let mut library_top = Vec::new();
        for i in 0..10 {
            let core_type = if i % 2 == 0 {
                CoreType::Creature
            } else {
                CoreType::Instant
            };
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Library Card {i}"),
                Zone::Library,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![core_type];
            library_top.push(id);
        }
        let _library_padding = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Library Padding".to_string(),
            Zone::Library,
        );

        let stale_graveyard_card = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Stale Graveyard Card".to_string(),
            Zone::Graveyard,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(5), vec![stale_graveyard_card]);
        state.next_tracked_set_id = 6;

        let categories = vec![
            CoreType::Artifact,
            CoreType::Battle,
            CoreType::Creature,
            CoreType::Enchantment,
            CoreType::Instant,
            CoreType::Land,
            CoreType::Planeswalker,
            CoreType::Sorcery,
        ];
        let change_zone = Box::new(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
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
            vec![],
            source,
            PlayerId(0),
        ));
        let choose = ResolvedAbility {
            sub_ability: Some(change_zone),
            ..ResolvedAbility::new(
                Effect::ChooseFromZone {
                    count: categories.len() as u32,
                    zone: Zone::Library,
                    additional_zones: Vec::new(),
                    zone_owner: ZoneOwner::Controller,
                    filter: None,
                    chooser: Chooser::Controller,
                    up_to: true,
                    constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { categories }),
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                },
                vec![],
                source,
                PlayerId(0),
            )
        };
        let reveal = ResolvedAbility {
            sub_ability: Some(Box::new(choose)),
            ..ResolvedAbility::new(
                Effect::RevealTop {
                    player: TargetFilter::Controller,
                    count: 10,
                },
                vec![],
                source,
                PlayerId(0),
            )
        };

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &reveal, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, up_to, .. } => {
                assert!(up_to);
                assert_eq!(cards.len(), 10, "must offer exactly the ten revealed cards");
                for id in &library_top {
                    assert!(cards.contains(id), "missing revealed library card {id:?}");
                }
                assert!(
                    !cards.contains(&stale_graveyard_card),
                    "graveyard cards must never appear in the reveal-and-choose pool"
                );
                assert!(
                    !cards.contains(&_library_padding),
                    "cards below the reveal window must not be offered"
                );
            }
            other => panic!(
                "Expected ChooseFromZoneChoice after RevealTop, got {:?}",
                other
            ),
        }
    }

    /// CR 608.2c-d: an empty reveal is still the current resolution's
    /// authoritative set. Atraxa must not offer cards left over from an older
    /// reveal when its controller's library is empty.
    #[test]
    fn atraxa_style_empty_reveal_does_not_reuse_a_stale_tracked_set() {
        use super::super::resolve_ability_chain;
        use crate::types::ability::TargetFilter;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(910),
            PlayerId(0),
            "Atraxa, Grand Unifier".to_string(),
            Zone::Battlefield,
        );
        let stale = create_object(
            &mut state,
            CardId(911),
            PlayerId(1),
            "Stale Revealed Card".to_string(),
            Zone::Library,
        );
        state.objects.get_mut(&stale).unwrap().card_types.core_types = vec![CoreType::Creature];
        state
            .tracked_object_sets
            .insert(TrackedSetId(5), vec![stale]);
        state.next_tracked_set_id = 6;
        assert!(state.players[0].library.is_empty());

        let categories = vec![
            CoreType::Artifact,
            CoreType::Battle,
            CoreType::Creature,
            CoreType::Enchantment,
            CoreType::Instant,
            CoreType::Land,
            CoreType::Planeswalker,
            CoreType::Sorcery,
        ];
        let choose = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: categories.len() as u32,
                zone: Zone::Library,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: true,
                constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { categories }),
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let reveal = ResolvedAbility {
            sub_ability: Some(Box::new(choose)),
            ..ResolvedAbility::new(
                Effect::RevealTop {
                    player: TargetFilter::Controller,
                    count: 10,
                },
                vec![],
                source,
                PlayerId(0),
            )
        };

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &reveal, &mut events, 0).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
            "an empty reveal must not create a choice from an older tracked set"
        );
        assert_eq!(
            state.last_parent_target_missing_reason,
            Some(crate::types::ability::ParentTargetMissingReason::ChooseFromZone)
        );
        assert_eq!(state.objects[&stale].zone, Zone::Library);
    }

    #[test]
    fn atraxa_style_choice_puts_all_unchosen_cards_on_bottom() {
        use super::super::resolve_ability_chain;
        use crate::game::engine::apply;
        use crate::types::ability::{LibraryPosition, QuantityExpr, TargetFilter};
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Atraxa, Grand Unifier".to_string(),
            Zone::Battlefield,
        );
        let mut revealed = Vec::new();
        for i in 0..10 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Revealed Card {i}"),
                Zone::Library,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![match i % 3 {
                0 => CoreType::Creature,
                1 => CoreType::Instant,
                _ => CoreType::Land,
            }];
            revealed.push(id);
        }
        let padding = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Library Padding".to_string(),
            Zone::Library,
        );
        let mut ordered_library = revealed.clone();
        ordered_library.push(padding);
        state.players[0].library = ordered_library.into();

        let bottom = Box::new(ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ExiledBySource,
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Bottom,
            },
            vec![],
            source,
            PlayerId(0),
        ));
        let change_zone = Box::new(ResolvedAbility {
            sub_ability: Some(bottom),
            ..ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination: Zone::Hand,
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
                vec![],
                source,
                PlayerId(0),
            )
        });
        let choose = ResolvedAbility {
            sub_ability: Some(change_zone),
            ..ResolvedAbility::new(
                Effect::ChooseFromZone {
                    count: 8,
                    zone: Zone::Library,
                    additional_zones: Vec::new(),
                    zone_owner: ZoneOwner::Controller,
                    filter: None,
                    chooser: Chooser::Controller,
                    up_to: true,
                    constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes {
                        categories: vec![CoreType::Creature, CoreType::Instant, CoreType::Land],
                    }),
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                },
                vec![],
                source,
                PlayerId(0),
            )
        };
        let reveal = ResolvedAbility {
            sub_ability: Some(Box::new(choose)),
            ..ResolvedAbility::new(
                Effect::RevealTop {
                    player: TargetFilter::Controller,
                    count: 10,
                },
                vec![],
                source,
                PlayerId(0),
            )
        };

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &reveal, &mut events, 0).unwrap();
        let chosen = vec![revealed[0], revealed[1], revealed[2]];
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: chosen.clone(),
            },
        )
        .unwrap();

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        for id in &chosen {
            assert_eq!(state.objects[id].zone, Zone::Hand);
        }
        let mut bottom_cards: Vec<_> = state.players[0].library.iter().skip(1).copied().collect();
        let mut unchosen: Vec<_> = revealed
            .iter()
            .filter(|id| !chosen.contains(id))
            .copied()
            .collect();
        bottom_cards.sort_by_key(|id| id.0);
        unchosen.sort_by_key(|id| id.0);
        assert_eq!(state.players[0].library[0], padding);
        assert_eq!(bottom_cards, unchosen);
    }

    /// CR 608.2d (override): a random `ChooseFromZone` picks the card(s) itself
    /// (no interactive prompt) and writes them onto the ability's `targets` so
    /// the chain forwards them to the sub-ability. Deterministic under seed.
    #[test]
    fn resolve_random_in_chain_picks_without_prompting() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Exile,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1, card2]);
        state.next_tracked_set_id = 2;

        let mut ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Random,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let handled = resolve_random_in_chain(&mut state, &mut ability, &mut events);
        assert!(handled, "random ChooseFromZone must be handled inline");
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
            "random selection must not raise an interactive prompt"
        );
        assert_eq!(ability.targets.len(), 1, "exactly one card picked");
        match &ability.targets[0] {
            TargetRef::Object(id) => assert!(*id == card1 || *id == card2),
            other => panic!("expected an object target, got {other:?}"),
        }
    }

    #[test]
    fn resolve_random_in_chain_ignores_non_random() {
        // Building-block regression: a Chosen ChooseFromZone is left to the
        // interactive `resolve` path (returns false, raises nothing here).
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        assert!(!resolve_random_in_chain(
            &mut state,
            &mut ability,
            &mut events
        ));
    }

    /// Build a library card of a single color for the for-each-category tests.
    fn make_colored_card(
        state: &mut GameState,
        card_id: u64,
        name: &str,
        color: crate::types::mana::ManaColor,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            PlayerId(0),
            name.to_string(),
            Zone::Library,
        );
        state.objects.get_mut(&id).unwrap().color = vec![color];
        id
    }

    /// CR 105.1 + CR 608.2c: the per-color member candidate pool of
    /// `ForEachCategoryExile { Color }` is restricted to cards of the bound
    /// color. With a White and a Blue card in the revealed pool, the FIRST
    /// member prompt (White, WUBRG order) must offer ONLY the White card. This
    /// flips on revert: a non-iterating or filter-blind resolver would offer
    /// both cards (or none), not the single White card.
    #[test]
    fn for_each_color_exile_first_member_offers_only_that_color() {
        use crate::types::mana::ManaColor;
        let mut state = GameState::new_two_player(7);
        let white = make_colored_card(&mut state, 1, "White Card", ManaColor::White);
        let blue = make_colored_card(&mut state, 2, "Blue Card", ManaColor::Blue);
        // The revealed pool is the chain's tracked set.
        let set_id = TrackedSetId(1);
        state.tracked_object_sets.insert(set_id, vec![white, blue]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(set_id);

        let ability = ResolvedAbility::new(
            Effect::ForEachCategory {
                category: crate::types::ability::IterationCategory::Color,
                chooser: Chooser::Controller,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Library,
                    up_to: true,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_for_each_category(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                up_to,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(*up_to, "you-may exile is optional (0..=1)");
                assert_eq!(
                    cards,
                    &vec![white],
                    "White member must offer only the White card, got {cards:?}"
                );
            }
            other => panic!("expected ChooseFromZoneChoice for White member, got {other:?}"),
        }
        assert!(
            state.pending_per_category_zone_choice.is_some(),
            "iteration must be parked after the first member"
        );
    }

    /// CR 105.1 + CR 608.2c: driving the FULL iteration through the resolution
    /// pipeline (`apply` → `handle_resolution_choice` → drain) exiles exactly one
    /// card per color member and accumulates them into the chain tracked set.
    /// With a White, a Blue, and a Black card, selecting each in turn exiles all
    /// three; declining the empty members leaves them in the library. Reverting
    /// the fix (no per-member iteration) cannot reach this state.
    #[test]
    fn for_each_color_exile_iterates_each_member_through_apply() {
        use crate::types::actions::GameAction;
        use crate::types::mana::ManaColor;
        let mut state = GameState::new_two_player(7);
        let white = make_colored_card(&mut state, 1, "White Card", ManaColor::White);
        let blue = make_colored_card(&mut state, 2, "Blue Card", ManaColor::Blue);
        let black = make_colored_card(&mut state, 3, "Black Card", ManaColor::Black);
        let set_id = TrackedSetId(1);
        state
            .tracked_object_sets
            .insert(set_id, vec![white, blue, black]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(set_id);

        let ability = ResolvedAbility::new(
            Effect::ForEachCategory {
                category: crate::types::ability::IterationCategory::Color,
                chooser: Chooser::Controller,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Library,
                    up_to: true,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_for_each_category(&mut state, &ability, &mut events).unwrap();

        // WUBRG order: White, then Blue, then Black are prompted (Red/Green skip,
        // no candidate). Select the offered card at each prompt.
        for expected in [white, blue, black] {
            match &state.waiting_for {
                WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                    assert_eq!(
                        cards,
                        &vec![expected],
                        "member prompt must offer only its color's card"
                    );
                }
                other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
            }
            crate::game::engine::apply(
                &mut state,
                PlayerId(0),
                GameAction::SelectCards {
                    cards: vec![expected],
                },
            )
            .unwrap();
        }

        // CR 608.2c: every chosen card was exiled.
        for id in [white, blue, black] {
            assert_eq!(
                state.objects.get(&id).unwrap().zone,
                Zone::Exile,
                "card {id:?} should be exiled by its color member"
            );
        }
        // The iteration is complete (no parked member, no choice prompt).
        assert!(
            state.pending_per_category_zone_choice.is_none(),
            "iteration must be disposed after every member"
        );
        // The chain tracked set holds exactly the cards exiled this way.
        let chain_id = state
            .chain_tracked_set_id
            .expect("chain tracked set rebound to the exiled cards");
        let exiled = state.tracked_object_sets.get(&chain_id).unwrap();
        assert_eq!(
            exiled.len(),
            3,
            "all three exiled cards accumulate into the chain set, got {exiled:?}"
        );
        for id in [white, blue, black] {
            assert!(exiled.contains(&id), "exiled set must contain {id:?}");
        }
    }

    /// CR 608.2c: declining a member ("you may exile") leaves that color's card
    /// in the pool — an empty `SelectCards` for the White member must NOT exile
    /// the White card, and the next member still resolves over the remaining
    /// pool. Discriminates the optional (`up_to`) per-member semantics.
    #[test]
    fn for_each_color_exile_member_decline_keeps_card() {
        use crate::types::actions::GameAction;
        use crate::types::mana::ManaColor;
        let mut state = GameState::new_two_player(7);
        let white = make_colored_card(&mut state, 1, "White Card", ManaColor::White);
        let blue = make_colored_card(&mut state, 2, "Blue Card", ManaColor::Blue);
        let set_id = TrackedSetId(1);
        state.tracked_object_sets.insert(set_id, vec![white, blue]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(set_id);

        let ability = ResolvedAbility::new(
            Effect::ForEachCategory {
                category: crate::types::ability::IterationCategory::Color,
                chooser: Chooser::Controller,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Library,
                    up_to: true,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_for_each_category(&mut state, &ability, &mut events).unwrap();

        // Decline the White member (empty selection).
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![] },
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&white).unwrap().zone,
            Zone::Library,
            "declined White card must stay in the library"
        );

        // The Blue member is prompted next; exile the Blue card.
        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(cards, &vec![blue]);
            }
            other => panic!("expected Blue member prompt, got {other:?}"),
        }
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![blue] },
        )
        .unwrap();
        assert_eq!(state.objects.get(&blue).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&white).unwrap().zone, Zone::Library);
    }

    /// CR 608.2c (Portent of Calamity tail): after the LAST category member's
    /// pick resolves, the parked continuation ("put the rest of the exiled cards
    /// into your hand" — a `ChangeZoneAll { target: TrackedSet }` over "the cards
    /// exiled this way") MUST run, and resolution MUST return to priority. This
    /// is the regression for review finding #1: if the iteration left a stale
    /// `ChooseFromZoneChoice` parked, `drain_pending_continuation` would early-
    /// return and the tail would never execute. Discriminating: the exiled cards
    /// end in HAND (continuation ran) and `waiting_for` is `Priority` (not a
    /// dangling member prompt).
    #[test]
    fn for_each_color_exile_runs_tracked_set_continuation_after_last_member() {
        use crate::types::ability::TargetFilter;
        use crate::types::actions::GameAction;
        use crate::types::identifiers::TrackedSetId;
        use crate::types::mana::ManaColor;
        let mut state = GameState::new_two_player(7);
        let white = make_colored_card(&mut state, 1, "White Card", ManaColor::White);
        let blue = make_colored_card(&mut state, 2, "Blue Card", ManaColor::Blue);
        let set_id = TrackedSetId(1);
        state.tracked_object_sets.insert(set_id, vec![white, blue]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(set_id);

        // Continuation: move the cards exiled this way (the chain tracked set,
        // `id: 0` sentinel) from Exile into the controller's hand. Mirrors
        // Portent's "put the rest of the exiled cards into your hand" tail.
        let continuation = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Hand,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility {
            sub_ability: Some(Box::new(continuation)),
            ..ResolvedAbility::new(
                Effect::ForEachCategory {
                    category: crate::types::ability::IterationCategory::Color,
                    chooser: Chooser::Controller,
                    action: ForEachCategoryAction::ExileFromPool {
                        zone: Zone::Library,
                        up_to: true,
                    },
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
        };

        // Drive the whole chain through the real resolution pipeline so the
        // sub_ability is stashed as `pending_continuation` exactly as a cast
        // would (the parking happens because the first member parks a choice).
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Exile each colored card at its member prompt (White then Blue).
        for expected in [white, blue] {
            match &state.waiting_for {
                WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                    assert_eq!(cards, &vec![expected]);
                }
                other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
            }
            crate::game::engine::apply(
                &mut state,
                PlayerId(0),
                GameAction::SelectCards {
                    cards: vec![expected],
                },
            )
            .unwrap();
        }

        // CR 608.2c: the iteration is disposed and resolution returned to
        // priority — NOT a dangling member prompt.
        assert!(
            state.pending_per_category_zone_choice.is_none(),
            "iteration must be disposed"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "resolution must return to priority after the continuation, got {:?}",
            state.waiting_for
        );
        // The continuation ran: both exiled cards are now in hand.
        for id in [white, blue] {
            assert_eq!(
                state.objects.get(&id).unwrap().zone,
                Zone::Hand,
                "continuation must move the cards exiled this way into hand; {id:?} not in hand"
            );
        }
    }

    /// CR 608.2c (Portent of Calamity): the free-cast node's gate "if you exiled
    /// four or more cards this way" reads the count of cards the per-card-type
    /// exile placed into the chain tracked set. This drives the REAL exile
    /// pipeline over four distinct card types and evaluates the PARSED gate (the
    /// condition the parser attaches to the `CastFromZone` node) against the
    /// resulting production-populated state — NOT a seeded set.
    ///
    /// Discrimination: exiling four cards opens the gate (`TrackedSetSize >= 4`),
    /// exiling three keeps it closed. Reverting the parser wiring
    /// (`parse_exiled_this_way_count`) drops the gate to `None`, so the
    /// `.expect(...)` below fails; without the gate the free cast would fire
    /// unconditionally and the three-card case could never be denied.
    ///
    /// `ChangeZoneAll` (the intervening "put the rest into your graveyard")
    /// never republishes a tracked set (grep: 0 `publish_*tracked_set` calls in
    /// `change_zone.rs`), so the exile set is still the most-recent set when the
    /// free-cast node's gate resolves — evaluating at exile-complete is
    /// equivalent to evaluating at the `CastFromZone` node.
    #[test]
    fn portent_free_cast_gate_reads_exiled_this_way_count() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{AbilityKind, Chooser, Effect};
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::{CardId, TrackedSetId};

        const PORTENT: &str = "Reveal the top X cards of your library. For each card type, you may exile a card of that type from among them. Put the rest into your graveyard. You may cast a spell from among the exiled cards without paying its mana cost if you exiled four or more cards this way. Then put the rest of the exiled cards into your hand.";
        let def = parse_effect_chain(PORTENT, AbilityKind::Spell);
        let mut node = &def;
        let gate = loop {
            if matches!(&*node.effect, Effect::CastFromZone { .. }) {
                break node.condition.clone().expect(
                    "the free-cast node must carry the 'exiled four or more this way' gate",
                );
            }
            node = node
                .sub_ability
                .as_ref()
                .expect("Portent chain must reach a CastFromZone node");
        };

        let eval_after_exiling = |exile_count: usize| -> bool {
            let mut state = GameState::new_two_player(7);
            let types = [
                CoreType::Artifact,
                CoreType::Creature,
                CoreType::Enchantment,
                CoreType::Sorcery,
            ];
            let mut pool = Vec::new();
            for (i, ty) in types.iter().enumerate() {
                let id = create_object(
                    &mut state,
                    CardId(i as u64 + 1),
                    PlayerId(0),
                    format!("Card {i}"),
                    Zone::Library,
                );
                state.objects.get_mut(&id).unwrap().card_types.core_types = vec![*ty];
                pool.push(id);
            }
            // Producer (RevealTop) binding: the revealed pool as the chain set.
            let producer = TrackedSetId(1);
            state.tracked_object_sets.insert(producer, pool.clone());
            state.next_tracked_set_id = 2;
            state.chain_tracked_set_id = Some(producer);

            let ability = ResolvedAbility::new(
                Effect::ForEachCategory {
                    category: crate::types::ability::IterationCategory::CardType,
                    chooser: Chooser::Controller,
                    action: ForEachCategoryAction::ExileFromPool {
                        zone: Zone::Library,
                        up_to: true,
                    },
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            );
            let mut events = Vec::new();
            resolve_for_each_category(&mut state, &ability, &mut events).unwrap();

            // Exile the offered card at the first `exile_count` member prompts;
            // decline the remainder.
            let mut exiled = 0;
            while let WaitingFor::ChooseFromZoneChoice { cards, .. } = &state.waiting_for {
                let pick = if exiled < exile_count {
                    exiled += 1;
                    cards.clone()
                } else {
                    vec![]
                };
                crate::game::engine::apply(
                    &mut state,
                    PlayerId(0),
                    GameAction::SelectCards { cards: pick },
                )
                .unwrap();
            }

            let chain = state
                .chain_tracked_set_id
                .and_then(|id| state.tracked_object_sets.get(&id))
                .map(|s| s.len())
                .unwrap_or(0);
            assert_eq!(
                chain, exile_count,
                "the exile pipeline must leave exactly {exile_count} cards in the 'exiled this way' set"
            );

            super::super::evaluate_condition(&gate, &state, &ability)
        };

        assert!(
            eval_after_exiling(4),
            "exiling four cards this way must open the free-cast gate (TrackedSetSize >= 4)"
        );
        assert!(
            !eval_after_exiling(3),
            "exiling only three cards this way must keep the free-cast gate closed"
        );
    }

    /// CR 608.2c (review finding #2): when the player DECLINES every member,
    /// no card is exiled, so "the cards exiled this way" is the EMPTY set. A
    /// downstream continuation consuming the chain tracked set must therefore see
    /// nothing — it must NOT inherit the producer's revealed pool. Discriminating:
    /// declining both members leaves both cards in the library (continuation moved
    /// nothing into hand), and the chain tracked set is empty. Reverting the
    /// all-decline fresh-empty-set fix leaves the chain bound to the revealed pool
    /// and the continuation would scoop both revealed cards into hand.
    #[test]
    fn for_each_color_exile_all_decline_leaves_empty_tracked_set_for_continuation() {
        use crate::types::ability::TargetFilter;
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;
        use crate::types::identifiers::TrackedSetId;
        use crate::types::mana::ManaColor;
        let mut state = GameState::new_two_player(7);
        let white = make_colored_card(&mut state, 1, "White Card", ManaColor::White);
        let blue = make_colored_card(&mut state, 2, "Blue Card", ManaColor::Blue);
        // The producer (RevealUntil/RevealTop) already published the revealed pool
        // as the resolution chain's tracked set. `resolve_for_each_category` is the
        // sub-effect; the depth-0 chain reset already happened upstream, so the
        // producer binding is live here (this models the real RevealTop → ForEach
        // chain rather than re-entering at depth 0, which would wipe the binding).
        let producer_set = TrackedSetId(1);
        state
            .tracked_object_sets
            .insert(producer_set, vec![white, blue]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(producer_set);

        // Continuation: "put the rest of the exiled cards into your hand" — move
        // the cards exiled this way (chain tracked set, `id: 0` sentinel) from
        // the library into hand. Installed as the parked continuation exactly as
        // the chain machinery stashes a sub_ability when the iteration parks.
        let continuation = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        state.pending_continuation = Some(PendingContinuation::new(Box::new(continuation), &state));

        let ability = ResolvedAbility::new(
            Effect::ForEachCategory {
                category: crate::types::ability::IterationCategory::Color,
                chooser: Chooser::Controller,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Library,
                    up_to: true,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_for_each_category(&mut state, &ability, &mut events).unwrap();

        // Decline both member prompts (empty selection each).
        for _ in 0..2 {
            assert!(
                matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
                "expected a member prompt, got {:?}",
                state.waiting_for
            );
            crate::game::engine::apply(
                &mut state,
                PlayerId(0),
                GameAction::SelectCards { cards: vec![] },
            )
            .unwrap();
        }

        // The continuation ran (resolution returned to priority).
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "resolution must return to priority, got {:?}",
            state.waiting_for
        );
        // Nothing was exiled, so the continuation over "the cards exiled this
        // way" must move nothing: both cards stay in the library. If the chain
        // set still pointed at the producer's revealed pool (the bug), the
        // ChangeZoneAll would have scooped both cards into hand.
        for id in [white, blue] {
            assert_eq!(
                state.objects.get(&id).unwrap().zone,
                Zone::Library,
                "declined card {id:?} must stay put — continuation saw an empty exiled set"
            );
        }
        // The chain tracked set the continuation read is empty (a fresh empty
        // set), not the producer's revealed pool. (`ChangeZoneAll` consumes/
        // removes the set it reads, so an absent binding here also satisfies
        // "empty exiled set".)
        let chain_cards = state
            .chain_tracked_set_id
            .and_then(|id| state.tracked_object_sets.get(&id))
            .cloned()
            .unwrap_or_default();
        assert!(
            chain_cards.is_empty(),
            "all-decline must leave an empty 'cards exiled this way' set, got {chain_cards:?}"
        );
    }

    /// CR 205.2a (review finding #3): the `CardType` member set must offer EVERY
    /// printed card type, including Battle and Kindred. With a Battle card and a
    /// Creature card in the revealed pool, iterating `CardType` must reach a
    /// Battle member prompt that offers the Battle card. Reverting to the
    /// seven-main-type list (no Battle/Kindred) makes the Battle card permanently
    /// ineligible and this assertion fails.
    #[test]
    fn for_each_card_type_offers_battle_member() {
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::TrackedSetId;
        let mut state = GameState::new_two_player(7);
        let battle = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Invasion of Battle".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&battle)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Battle];
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
            .core_types = vec![CoreType::Creature];
        let set_id = TrackedSetId(1);
        state
            .tracked_object_sets
            .insert(set_id, vec![battle, creature]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(set_id);

        let ability = ResolvedAbility::new(
            Effect::ForEachCategory {
                category: crate::types::ability::IterationCategory::CardType,
                chooser: Chooser::Controller,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Library,
                    up_to: true,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_for_each_category(&mut state, &ability, &mut events).unwrap();

        // Walk every member prompt; record that a member offered the Battle card.
        let mut battle_offered = false;
        let mut creature_offered = false;
        while let WaitingFor::ChooseFromZoneChoice { cards, .. } = &state.waiting_for {
            if cards == &vec![battle] {
                battle_offered = true;
            }
            if cards == &vec![creature] {
                creature_offered = true;
            }
            // Decline each member to advance the iteration.
            crate::game::engine::apply(
                &mut state,
                PlayerId(0),
                GameAction::SelectCards { cards: vec![] },
            )
            .unwrap();
        }
        assert!(
            battle_offered,
            "a Battle member prompt must offer the Battle card (CR 205.2a)"
        );
        assert!(
            creature_offered,
            "a Creature member prompt must offer the Creature card"
        );
    }

    /// CR 205.2a + CR 308.1: Kindred is a distinct 9th card-type member of the
    /// for-each-card-type iteration. A Kindred Sorcery (core types {Kindred,
    /// Sorcery}) is exilable at BOTH the dedicated Kindred member and the Sorcery
    /// member; per CR 308.1 ("each kindred card has another card type") a kindred
    /// card always carries a second card type. This test drives the real
    /// for-each-card-type chain over a pool of one Kindred Sorcery + one plain
    /// Sorcery and asserts:
    ///   1. There are TWO distinct member prompts that can hold a Sorcery — the
    ///      Kindred member (offers only the Kindred Sorcery) and the Sorcery member.
    ///   2. Exiling at the Kindred prompt removes the Kindred Sorcery from the
    ///      pool (zone filter), so the later Sorcery prompt offers only the OTHER,
    ///      plain Sorcery — exiling there too lands TWO different cards in the
    ///      chain tracked set.
    ///   3. NEGATIVE: the plain Sorcery is NEVER offered at the Kindred member.
    ///
    /// Revert-fail: removing `TypeFilter::Kindred` from
    /// `IterationCategory::member_filters()` collapses the two Sorcery-bearing
    /// members into one (the Sorcery member). With `up_to = 1` per member, the
    /// Kindred Sorcery can then be exiled at most once and the plain Sorcery at
    /// most once — but the iteration would never present a Kindred prompt, so the
    /// `kindred_prompt_seen` assertion and the `exiled.len() == 2` assertion both
    /// fail. (Observed: with the variant removed from the member list the test
    /// FAILS at `kindred_prompt_seen`; restored, it PASSES.)
    #[test]
    fn for_each_card_type_offers_kindred_member_distinct_from_cotype() {
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::TrackedSetId;
        let mut state = GameState::new_two_player(7);

        // A Kindred Sorcery — core types {Kindred, Sorcery} (the current-rules
        // shape, mirroring the card_db "Elf Rite" fixture). Matches BOTH the
        // Kindred member and the Sorcery member.
        let kindred_sorcery = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kindred Sorcery".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&kindred_sorcery)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Kindred, CoreType::Sorcery];

        // A plain Sorcery — matches ONLY the Sorcery member.
        let plain_sorcery = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Plain Sorcery".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&plain_sorcery)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Sorcery];

        let set_id = TrackedSetId(1);
        state
            .tracked_object_sets
            .insert(set_id, vec![kindred_sorcery, plain_sorcery]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(set_id);

        let ability = ResolvedAbility::new(
            Effect::ForEachCategory {
                category: crate::types::ability::IterationCategory::CardType,
                chooser: Chooser::Controller,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Library,
                    up_to: true,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_for_each_category(&mut state, &ability, &mut events).unwrap();

        // Walk every member prompt in CR 205.2a order. The Kindred member (a
        // distinct prompt) must offer ONLY the Kindred Sorcery — never the plain
        // Sorcery (the negative case). The Sorcery member is a SEPARATE prompt;
        // by the time it is reached the Kindred Sorcery has left the library, so
        // it offers only the plain Sorcery. Exile at every prompt that has a card.
        let mut kindred_prompt_seen = false;
        let mut sorcery_prompt_seen = false;
        let mut kindred_sorcery_exiled_at_kindred_prompt = false;
        let mut plain_sorcery_exiled_at_sorcery_prompt = false;
        while let WaitingFor::ChooseFromZoneChoice { cards, .. } = &state.waiting_for {
            let offered = cards.clone();
            // A prompt that offers the Kindred Sorcery while it is still in the
            // library is the Kindred member (the Sorcery member only ever sees it
            // after it has been exiled at the Kindred member, i.e. never).
            let to_exile = if offered == vec![kindred_sorcery] {
                kindred_prompt_seen = true;
                // NEGATIVE: the Kindred member must NOT offer the plain Sorcery.
                assert!(
                    !offered.contains(&plain_sorcery),
                    "plain Sorcery (no Kindred type) must NOT be offered at the Kindred member"
                );
                kindred_sorcery_exiled_at_kindred_prompt = true;
                vec![kindred_sorcery]
            } else if offered == vec![plain_sorcery] {
                sorcery_prompt_seen = true;
                plain_sorcery_exiled_at_sorcery_prompt = true;
                vec![plain_sorcery]
            } else {
                // Any other member with an unexpected offer: decline.
                vec![]
            };
            crate::game::engine::apply(
                &mut state,
                PlayerId(0),
                GameAction::SelectCards { cards: to_exile },
            )
            .unwrap();
        }

        assert!(
            kindred_prompt_seen,
            "a distinct Kindred member prompt (CR 308.1) must offer the Kindred Sorcery"
        );
        assert!(
            sorcery_prompt_seen,
            "a distinct Sorcery member prompt must offer the remaining plain Sorcery"
        );
        assert!(
            kindred_sorcery_exiled_at_kindred_prompt,
            "the Kindred Sorcery was exiled at the Kindred member"
        );
        assert!(
            plain_sorcery_exiled_at_sorcery_prompt,
            "the plain Sorcery was exiled at the (separate) Sorcery member"
        );

        // Both cards were exiled — by two different prompts.
        assert_eq!(
            state.objects.get(&kindred_sorcery).unwrap().zone,
            Zone::Exile,
            "Kindred Sorcery exiled at the Kindred member"
        );
        assert_eq!(
            state.objects.get(&plain_sorcery).unwrap().zone,
            Zone::Exile,
            "plain Sorcery exiled at the Sorcery member"
        );

        // The chain tracked set holds exactly the two distinct exiled cards.
        // Reverting the Kindred member from `member_filters()` caps the exile at
        // one card (only the Sorcery member would remain, up_to = 1), so this
        // length assertion flips to <= 1 and the test fails.
        let chain_id = state
            .chain_tracked_set_id
            .expect("chain tracked set rebound to the exiled cards");
        let exiled = state.tracked_object_sets.get(&chain_id).unwrap();
        assert_eq!(
            exiled.len(),
            2,
            "two distinct cards exiled by two distinct member prompts, got {exiled:?}"
        );
        assert!(exiled.contains(&kindred_sorcery));
        assert!(exiled.contains(&plain_sorcery));
    }

    /// CR 603.7 + CR 608.2c (MED #4093): the FIRST resolution of a per-player
    /// `ChooseFromZone` iteration must rebind a fresh (possibly empty) chain
    /// tracked set EVEN when that first player declines. Otherwise an earlier
    /// same-chain producer's tracked set stays bound, and a downstream
    /// `ChangeZoneAll { TrackedSet }` over-acts on the prior producer's objects
    /// instead of this iteration's (empty) picks.
    ///
    /// REVERT PROBE: restore the original `if chosen.is_empty() { accumulated }`
    /// branch and the first decline does NOT rebind — `chain_tracked_set_id`
    /// stays pointed at `prior` (the non-empty producer set), so the two
    /// assertions below (fresh id distinct from `prior`; bound set size 0) both
    /// fail.
    #[test]
    fn per_player_first_decline_rebinds_fresh_empty_tracked_set() {
        let mut state = GameState::new_two_player(42);

        // An earlier SAME-CHAIN producer published a NON-EMPTY tracked set and
        // bound the chain to it (e.g. Breach the Multiverse's preceding mill).
        let prior = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(prior, vec![ObjectId(7), ObjectId(8)]);
        state.chain_tracked_set_id = Some(prior);

        // A per-player ChooseFromZone whose first (and only) player will decline.
        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Battlefield,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::EachOpponent,
                filter: None,
                chooser: Chooser::Controller,
                up_to: true,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        // First resolution: no players left to prompt afterwards, accumulated=false.
        state.pending_per_player_zone_choice =
            Some(crate::types::game_state::PendingPerPlayerZoneChoice {
                ability: Box::new(ability),
                remaining_players: vec![],
                accumulated: false,
            });

        let mut events = Vec::new();
        // The first player DECLINES — an empty "up to one" pick.
        drain_pending_per_player_zone_choice(&mut state, &[], &mut events);

        let bound = state
            .chain_tracked_set_id
            .expect("a fresh chain tracked set must be bound after the first decline");
        assert_ne!(
            bound, prior,
            "the first decline must rebind to a FRESH set, not stay on the prior producer's set"
        );
        assert_eq!(
            state.tracked_object_sets.get(&bound),
            Some(&vec![]),
            "the fresh per-player set must be empty (everyone declined)"
        );
        // The prior producer's set is untouched (still holds its two objects), so
        // a downstream ChangeZoneAll { TrackedSet } reads the empty fresh set.
        assert_eq!(
            state.tracked_object_sets.get(&prior),
            Some(&vec![ObjectId(7), ObjectId(8)]),
            "the prior producer's set must be untouched, never inherited by the iteration"
        );
    }

    /// CR 101.4 + CR 608.2c (MED #4093): the FIRST resolution of a per-player
    /// `ChooseFromZone` iteration must rebind a fresh (possibly empty) chain
    /// tracked set EVEN when NO iterated player has an eligible candidate (the
    /// loop exhausts in `prompt_next_each_player` with zero prompts ever parked).
    /// This is the no-prompt sibling of
    /// `per_player_first_decline_rebinds_fresh_empty_tracked_set` (which reaches
    /// the rebind via the drain path). Here the entry is `resolve`, which
    /// dispatches `EachOpponent` straight into `prompt_next_each_player` with
    /// `accumulated == false`; every opponent zone is empty so the loop exhausts.
    ///
    /// REVERT PROBE: remove the new `if !accumulated { publish_fresh_tracked_set }`
    /// call on the exhaustion branch and `chain_tracked_set_id` stays pointed at
    /// `prior` (the non-empty producer set) — both `assert_ne!(bound, prior)` and
    /// the empty-set assertion below fail (the bound set would hold `prior`'s two
    /// objects).
    #[test]
    fn per_player_no_eligible_player_rebinds_fresh_empty_tracked_set() {
        let mut state = GameState::new_two_player(42);

        // An earlier SAME-CHAIN producer published a NON-EMPTY tracked set and
        // bound the chain to it (e.g. Breach the Multiverse's preceding mill).
        let prior = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(prior, vec![ObjectId(7), ObjectId(8)]);
        state.chain_tracked_set_id = Some(prior);

        // A per-OPPONENT ChooseFromZone scanning the battlefield. The only
        // opponent of PlayerId(0) is PlayerId(1), whose battlefield is empty in a
        // fresh two-player state, so `collect_player_zone_cards` yields nothing for
        // every iterated player and `prompt_next_each_player` exhausts WITHOUT
        // parking a single prompt (the no-eligible-anyone path).
        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Battlefield,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::EachOpponent,
                filter: None,
                chooser: Chooser::Controller,
                up_to: true,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        // `resolve` routes EachOpponent directly into `prompt_next_each_player`
        // with accumulated=false; no opponent is eligible → exhaustion branch.
        resolve(&mut state, &ability, &mut events).unwrap();

        // No interactive prompt was raised (the loop never parked one).
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
            "no prompt must be parked when no iterated player is eligible, got {:?}",
            state.waiting_for
        );

        let bound = state
            .chain_tracked_set_id
            .expect("a fresh chain tracked set must be bound after the no-eligible exhaustion");
        assert_ne!(
            bound, prior,
            "the no-eligible exhaustion must rebind to a FRESH set, not stay on the prior producer's set"
        );
        assert_eq!(
            state.tracked_object_sets.get(&bound),
            Some(&vec![]),
            "the fresh per-player set must be empty (no player was eligible)"
        );
        // The prior producer's set is untouched, so a downstream
        // ChangeZoneAll { TrackedSet } reads the empty fresh set, not these objects.
        assert_eq!(
            state.tracked_object_sets.get(&prior),
            Some(&vec![ObjectId(7), ObjectId(8)]),
            "the prior producer's set must be untouched, never inherited by the iteration"
        );
    }
}
