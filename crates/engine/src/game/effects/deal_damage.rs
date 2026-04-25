use std::collections::HashSet;

use crate::game::ability_utils::append_to_sub_chain;
use crate::game::effects::{append_to_pending_continuation, mark_pending_continuation_parent};
use crate::game::filter;
use crate::game::keywords;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    DamageSource, Effect, EffectError, EffectKind, PlayerFilter, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{DamageRecord, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

/// Source attributes needed for damage application (CR 120.3).
/// Read from the source object before the mutable damage phase to avoid borrow conflicts.
pub(crate) struct DamageContext {
    pub(crate) source_id: ObjectId,
    pub(crate) controller: PlayerId,
    pub(crate) has_deathtouch: bool,
    pub(crate) has_lifelink: bool,
    pub(crate) has_wither: bool,
    pub(crate) has_infect: bool,
}

impl DamageContext {
    /// Build context by reading keywords from the source object.
    /// Returns None if source doesn't exist in state.
    pub(crate) fn from_source(state: &GameState, source_id: ObjectId) -> Option<Self> {
        state.objects.get(&source_id).map(|obj| Self {
            source_id,
            controller: obj.controller,
            has_deathtouch: obj.has_keyword(&Keyword::Deathtouch),
            has_lifelink: obj.has_keyword(&Keyword::Lifelink),
            has_wither: obj.has_keyword(&Keyword::Wither),
            has_infect: obj.has_keyword(&Keyword::Infect),
        })
    }

    /// Fallback context when source no longer exists (all keyword flags false).
    /// CR 702.15c: last known information should be used for lifelink, but if the
    /// source is truly gone with no LKI available, defaulting to false is safe.
    pub(crate) fn fallback(source_id: ObjectId, controller: PlayerId) -> Self {
        Self {
            source_id,
            controller,
            has_deathtouch: false,
            has_lifelink: false,
            has_wither: false,
            has_infect: false,
        }
    }
}

/// Outcome of applying damage through the replacement pipeline.
pub(crate) enum DamageResult {
    /// Damage applied (possibly modified/prevented). Contains post-replacement amount dealt.
    Applied(u32),
    /// A replacement effect requires a player choice before damage resolves.
    NeedsChoice,
}

/// CR 120.3 + CR 120.4b: Apply damage from a single source to a single target through
/// the full replacement/prevention pipeline.
///
/// Handles: protection (CR 702.16b), replacement effects (CR 120.4b), damage marking
/// (CR 120.3e), planeswalker loyalty (CR 120.3c / CR 306.8), wither (CR 702.80),
/// infect (CR 702.90), deathtouch (CR 702.2b), lifelink (CR 702.15b), and
/// DamageDealt event emission.
///
/// Event ordering: DamageDealt is emitted before lifelink LifeChanged.
/// EffectResolved is NOT emitted — that remains the caller's responsibility.
///
/// Returns `DamageResult::Applied(actual_amount)` or `DamageResult::NeedsChoice`.
pub(crate) fn apply_damage_to_target(
    state: &mut GameState,
    ctx: &DamageContext,
    target: TargetRef,
    amount: u32,
    is_combat: bool,
    events: &mut Vec<GameEvent>,
) -> Result<DamageResult, EffectError> {
    // CR 120.8: If a source would deal 0 damage, it does not deal damage at all.
    if amount == 0 {
        return Ok(DamageResult::Applied(0));
    }

    // CR 120.2: Source-side "can't deal damage" prohibition. The source deals
    // zero damage of any kind, regardless of target.
    if crate::game::static_abilities::object_has_static_other(
        state,
        ctx.source_id,
        "CantDealDamage",
    ) {
        return Ok(DamageResult::Applied(0));
    }

    // CR 120.1: Target-side "can't be dealt damage" prohibition (objects only;
    // `CantBeDealtDamage` in the static registry is object-scoped).
    if let TargetRef::Object(target_obj_id) = &target {
        if crate::game::static_abilities::object_has_static_other(
            state,
            *target_obj_id,
            "CantBeDealtDamage",
        ) {
            return Ok(DamageResult::Applied(0));
        }
    }

    // CR 702.16b + CR 702.16e: Protection prevents damage from sources with the matching quality.
    // Emits DamagePrevented so "when damage is prevented" triggers can fire.
    if let TargetRef::Object(target_obj_id) = &target {
        if let (Some(target_obj), Some(source_obj)) = (
            state.objects.get(target_obj_id),
            state.objects.get(&ctx.source_id),
        ) {
            if keywords::protection_prevents_from(target_obj, source_obj) {
                events.push(GameEvent::DamagePrevented {
                    source_id: ctx.source_id,
                    target: target.clone(),
                    amount,
                });
                return Ok(DamageResult::Applied(0));
            }
        }
    }

    // CR 702.16j + CR 615.1: "All damage that would be dealt to [a player with
    // protection from everything] is prevented." Mirror the object-protection
    // gate above for player targets. Emits DamagePrevented so prevention-
    // triggered abilities still observe the event.
    if let TargetRef::Player(player_id) = &target {
        if crate::game::static_abilities::player_has_protection_from_everything(state, *player_id) {
            events.push(GameEvent::DamagePrevented {
                source_id: ctx.source_id,
                target: target.clone(),
                amount,
            });
            return Ok(DamageResult::Applied(0));
        }
    }

    let proposed = ProposedEvent::Damage {
        source_id: ctx.source_id,
        target: target.clone(),
        amount,
        is_combat,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => Ok(apply_damage_after_replacement(
            state, ctx, event, is_combat, events,
        )),
        ReplacementResult::Prevented => {
            // CR 615.5: A prevention effect's additional effect (e.g.
            // Phyrexian Hydra's "Put a -1/-1 counter on ~ for each 1 damage
            // prevented this way") is stashed as `post_replacement_effect` by
            // the prevention applier. Resolve it inline here so the follow-up
            // takes place "immediately afterward" as the rule requires. The
            // applier already stamped `state.last_effect_count` with the
            // prevented amount so `EventContextAmount` resolves correctly.
            if let Some(effect_def) = state.post_replacement_effect.take() {
                let source = state.post_replacement_source.take();
                // CR 615.5 + CR 609.7: leave `post_replacement_event_source`
                // populated for the call so `TargetFilter::PostReplacementSourceController`
                // can resolve against the prevented event's damage source. Clear
                // after the call to prevent leakage into unrelated later
                // replacements.
                let _ = crate::game::engine_replacement::apply_post_replacement_effect(
                    state,
                    &effect_def,
                    source,
                    None,
                    events,
                );
                state.post_replacement_event_source = None;
            }
            Ok(DamageResult::Applied(0))
        }
        ReplacementResult::NeedsChoice(player) => {
            // Only set waiting_for for non-combat damage; combat damage cannot pause mid-resolution.
            if !is_combat {
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            }
            Ok(DamageResult::NeedsChoice)
        }
    }
}

/// CR 120.3 + CR 120.4b: Apply a post-replacement `ProposedEvent::Damage` to the game state.
///
/// Extracted from `apply_damage_to_target`'s Execute arm so the same logic can be
/// invoked by `handle_replacement_choice` when a player accepts a damage replacement
/// choice. Handles wither/infect (CR 702.80 / CR 702.90), planeswalker loyalty
/// (CR 120.3c / CR 306.8), creature damage marking (CR 120.3e), poison (CR 702.90),
/// life loss (CR 120.3a), excess damage (CR 120.10), damage record tracking, and
/// lifelink (CR 702.15b / CR 120.3f).
///
/// Caller is responsible for emitting `EffectResolved`. This helper only emits
/// `DamageDealt` (and downstream `LifeChanged` via the life helpers).
pub(crate) fn apply_damage_after_replacement(
    state: &mut GameState,
    ctx: &DamageContext,
    event: ProposedEvent,
    is_combat: bool,
    events: &mut Vec<GameEvent>,
) -> DamageResult {
    let ProposedEvent::Damage {
        target: ref t,
        amount: actual_amount,
        ..
    } = event
    else {
        debug_assert!(
            false,
            "apply_damage_after_replacement called with non-Damage ProposedEvent"
        );
        return DamageResult::Applied(0);
    };

    match t {
        TargetRef::Object(obj_id) => {
            if ctx.has_wither || ctx.has_infect {
                // CR 702.80 + CR 702.90: Wither/infect deals damage as -1/-1 counters.
                if let Some(target_obj) = state.objects.get_mut(obj_id) {
                    let entry = target_obj
                        .counters
                        .entry(CounterType::Minus1Minus1)
                        .or_insert(0);
                    *entry += actual_amount;
                    if ctx.has_deathtouch {
                        target_obj.dealt_deathtouch_damage = true;
                    }
                }
                state.layers_dirty = true;
            } else {
                // Classify the target before mutating so the post-classification
                // helper can take a fresh `&mut GameState` borrow.
                enum DamageKind {
                    Planeswalker,
                    Battle,
                    Creature,
                }
                let kind = state.objects.get(obj_id).map(|obj| {
                    if obj.card_types.core_types.contains(&CoreType::Planeswalker) {
                        DamageKind::Planeswalker
                    } else if obj.card_types.core_types.contains(&CoreType::Battle) {
                        DamageKind::Battle
                    } else {
                        DamageKind::Creature
                    }
                });
                match kind {
                    Some(DamageKind::Planeswalker) => {
                        // CR 120.3c + CR 306.8: Damage to a planeswalker removes that
                        // many loyalty counters. Routed through the single-authority
                        // resolver so replacement effects apply and obj.loyalty
                        // stays in sync with counters[Loyalty] (CR 306.5b).
                        super::counters::remove_counter_with_replacement(
                            state,
                            *obj_id,
                            CounterType::Loyalty,
                            actual_amount,
                            events,
                        );
                    }
                    Some(DamageKind::Battle) => {
                        // CR 120.3h + CR 310.6: Damage to a battle removes that many
                        // defense counters. Routed through the single-authority
                        // resolver so obj.defense stays in sync with counters[Defense]
                        // (CR 310.4c).
                        super::counters::remove_counter_with_replacement(
                            state,
                            *obj_id,
                            CounterType::Defense,
                            actual_amount,
                            events,
                        );
                    }
                    Some(DamageKind::Creature) => {
                        if let Some(target_obj) = state.objects.get_mut(obj_id) {
                            // CR 120.3e: Damage to a creature marks damage.
                            target_obj.damage_marked += actual_amount;
                            // CR 702.2b: Track deathtouch for SBA lethal-damage check.
                            if ctx.has_deathtouch {
                                target_obj.dealt_deathtouch_damage = true;
                            }
                        }
                    }
                    None => {}
                }
            }
        }
        TargetRef::Player(player_id) => {
            // Player-phasing exclusion: a phased-out player can't be affected
            // by damage (mirrors CR 702.26b for permanents). The damage is
            // simply not applied — no life loss, no poison counters, no
            // DamageDealt event for this routing pass.
            if state
                .players
                .iter()
                .find(|p| p.id == *player_id)
                .is_some_and(|p| p.is_phased_out())
            {
                return DamageResult::Applied(0);
            }
            if ctx.has_infect {
                // CR 702.90: Infect deals damage to players as poison counters.
                if let Some(player) = state.players.iter_mut().find(|p| p.id == *player_id) {
                    player.poison_counters += actual_amount;
                }
            } else {
                // CR 120.3a: Damage to a player causes life loss.
                if super::life::apply_damage_life_loss(state, *player_id, actual_amount, events)
                    .is_err()
                {
                    // CR 614.7: Life loss replacement needs player choice.
                    return DamageResult::NeedsChoice;
                }
            }
        }
    }

    // CR 120.10: Compute excess damage beyond lethal for creatures/planeswalkers.
    let excess = match &t {
        TargetRef::Object(obj_id) => state
            .objects
            .get(obj_id)
            .and_then(|obj| {
                if obj.card_types.core_types.contains(&CoreType::Creature) {
                    obj.toughness.map(|toughness| {
                        // damage_marked already includes actual_amount
                        let damage_before = obj.damage_marked.saturating_sub(actual_amount);
                        let lethal = if ctx.has_deathtouch {
                            // CR 702.2c: Any nonzero damage from deathtouch = lethal
                            if damage_before == 0 {
                                1u32
                            } else {
                                0
                            }
                        } else {
                            (toughness as u32).saturating_sub(damage_before)
                        };
                        actual_amount.saturating_sub(lethal)
                    })
                } else if obj.card_types.core_types.contains(&CoreType::Planeswalker) {
                    // CR 120.10: Excess for planeswalkers = damage beyond pre-hit loyalty.
                    // Loyalty was already decremented, so reconstruct pre-hit value.
                    let pre_loyalty = obj.loyalty.unwrap_or(0) + actual_amount;
                    Some(actual_amount.saturating_sub(pre_loyalty))
                } else if obj.card_types.core_types.contains(&CoreType::Battle) {
                    // CR 120.10: Excess for battles = damage beyond pre-hit defense.
                    // Defense was already decremented, so reconstruct pre-hit value.
                    let pre_defense = obj.defense.unwrap_or(0) + actual_amount;
                    Some(actual_amount.saturating_sub(pre_defense))
                } else {
                    Some(0)
                }
            })
            .unwrap_or(0),
        TargetRef::Player(_) => 0,
    };

    events.push(GameEvent::DamageDealt {
        source_id: ctx.source_id,
        target: t.clone(),
        amount: actual_amount,
        is_combat,
        excess,
    });

    // CR 120.1: Record damage for "was dealt damage by" condition queries.
    if actual_amount > 0 {
        state.damage_dealt_this_turn.push(DamageRecord {
            source_id: ctx.source_id,
            target: t.clone(),
            amount: actual_amount,
            is_combat,
        });
    }

    // CR 702.15b / CR 120.3f: Lifelink — controller gains life equal to damage dealt.
    if ctx.has_lifelink
        && actual_amount > 0
        && super::life::apply_life_gain(state, ctx.controller, actual_amount, events).is_err()
    {
        // CR 614.7: Life gain replacement needs player choice.
        // Damage was already dealt; lifelink gain is deferred.
        return DamageResult::NeedsChoice;
    }

    DamageResult::Applied(actual_amount)
}

/// CR 120.3 + CR 616.1e: Build a one-shot, single-target non-combat `DealDamage`
/// node for a remaining-target damage continuation. The node's `source_id` is set
/// to the original damage-source id so `DamageContext::from_source` reproduces the
/// original source's keywords at resume time; `amount` is captured as `Fixed` so
/// it does not re-resolve against mutated state.
fn build_remaining_damage_node(
    damage_source_id: ObjectId,
    controller: PlayerId,
    target: TargetRef,
    amount: u32,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed {
                value: amount as i32,
            },
            target: TargetFilter::Any,
            damage_source: None,
        },
        vec![target],
        damage_source_id,
        controller,
    )
}

/// CR 120.3 + CR 616.1e: Build a linked sub_ability chain from a sequence of
/// (target, amount) pairs and stash it as `pending_continuation`. If the parent
/// ability has an existing `sub_ability` chain, it is appended to the tail so
/// downstream effects still fire after the batch completes. `damage_source_id`
/// controls which object's keywords/LKI drive each resumed damage event.
fn stash_remaining_damage_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    damage_source_id: ObjectId,
    remaining: impl IntoIterator<Item = (TargetRef, u32)>,
) {
    let controller = ability.controller;
    let mut iter = remaining.into_iter();
    let Some((first_target, first_amount)) = iter.next() else {
        // No remaining batch work — still forward the parent's sub_ability so the
        // downstream chain resumes after the pending replacement choice resolves.
        if let Some(sub) = ability.sub_ability.as_ref() {
            append_to_pending_continuation(state, Some(Box::new(sub.as_ref().clone())));
        }
        return;
    };

    let mut head =
        build_remaining_damage_node(damage_source_id, controller, first_target, first_amount);
    for (target, amount) in iter {
        let node = build_remaining_damage_node(damage_source_id, controller, target, amount);
        append_to_sub_chain(&mut head, node);
    }
    if let Some(sub) = ability.sub_ability.as_ref() {
        append_to_sub_chain(&mut head, sub.as_ref().clone());
    }
    append_to_pending_continuation(state, Some(Box::new(head)));
}

/// CR 120.1: Deal N damage — reduces life for players, marks damage on creatures.
/// Reads amount from `Effect::DealDamage { amount }`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_dmg, damage_source, target_filter): (u32, Option<DamageSource>, &TargetFilter) =
        match &ability.effect {
            Effect::DealDamage {
                amount,
                damage_source,
                target,
            } => (
                resolve_quantity_with_targets(state, amount, ability) as u32,
                *damage_source,
                target,
            ),
            _ => return Err(EffectError::MissingParam("DealDamage amount".to_string())),
        };

    // CR 120.3: Determine damage source. When DamageSource::Target, the first resolved
    // object target is the damage source (e.g., "target creature deals damage to itself").
    let ctx = if matches!(damage_source, Some(DamageSource::Target)) {
        ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => DamageContext::from_source(state, *id),
                _ => None,
            })
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller))
    } else {
        DamageContext::from_source(state, ability.source_id)
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller))
    };

    // Resolve effective targets: use explicit targets if present, otherwise derive
    // implicit target from the TargetFilter for non-targeted damage ("to you", "to itself").
    let implicit;
    let effective_targets = if !ability.targets.is_empty() {
        &ability.targets
    } else {
        implicit = match target_filter {
            TargetFilter::Controller => vec![TargetRef::Player(ability.controller)],
            TargetFilter::SelfRef => vec![TargetRef::Object(ability.source_id)],
            _ => vec![],
        };
        &implicit
    };

    // CR 601.2d: If the caster distributed damage among targets at cast time,
    // apply per-target amounts from ability.distribution instead of uniform damage.
    if let Some(distribution) = &ability.distribution {
        for (i, (target, amount)) in distribution.iter().enumerate() {
            match apply_damage_to_target(state, &ctx, target.clone(), *amount, false, events)? {
                DamageResult::Applied(_) => {}
                DamageResult::NeedsChoice => {
                    // CR 120.3 + CR 616.1e: Remaining distributed targets must resume
                    // after the replacement choice resolves. Stash each as a chained
                    // DealDamage continuation keyed to the same damage-source id.
                    let remaining = distribution[i + 1..].iter().map(|(t, a)| (t.clone(), *a));
                    stash_remaining_damage_chain(state, ability, ctx.source_id, remaining);
                    return Ok(());
                }
            }
        }
    } else {
        for (i, target) in effective_targets.iter().enumerate() {
            match apply_damage_to_target(state, &ctx, target.clone(), num_dmg, false, events)? {
                DamageResult::Applied(_) => {}
                DamageResult::NeedsChoice => {
                    // CR 120.3 + CR 616.1e: Remaining targets must resume after the
                    // replacement choice resolves.
                    let remaining = effective_targets[i + 1..]
                        .iter()
                        .map(|t| (t.clone(), num_dmg));
                    stash_remaining_damage_chain(state, ability, ctx.source_id, remaining);
                    return Ok(());
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Deal uniform damage to every matching object and (optionally) every matching
/// player as a single simultaneous damage event from one source.
///
/// Reads amount, object filter, and optional player filter from
/// `Effect::DamageAll { amount, target, player_filter }`.
///
/// CR 120.3: Damage is dealt simultaneously to all affected objects and players
/// from a single source. The batch is one effect resolution, so prevention and
/// replacement shields that watch "the next damage dealt by [this source]"
/// (CR 609.7, CR 614, CR 615) observe one coherent event across the full set.
/// CR 120.3e: Non-combat damage from an effect is marked on each matching creature.
/// CR 120.3a: Damage dealt to a player causes that player to lose that much life.
/// CR 120.4b: Each per-target damage instance is routed through the replacement
/// pipeline individually (see `apply_damage_to_target`), but all share the same
/// `DamageContext` (single source, single set of keywords) and the same effect.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_dmg, target_filter, player_filter): (u32, TargetFilter, Option<PlayerFilter>) =
        match &ability.effect {
            Effect::DamageAll {
                amount,
                target,
                player_filter,
            } => {
                // CR 107.1b: Ability-context resolve so X-damage-to-all ("Deal X damage to each...")
                // reads the caster-chosen X.
                let dmg = resolve_quantity_with_targets(state, amount, ability).max(0) as u32;
                (dmg, target.clone(), *player_filter)
            }
            _ => return Err(EffectError::MissingParam("DamageAll amount".to_string())),
        };

    let target_filter = crate::game::effects::resolved_object_filter(ability, &target_filter);

    // Collect matching object IDs.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = filter::FilterContext::from_ability(ability);
    let matching_objects: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| filter::matches_target_filter(state, **id, &target_filter, &ctx))
        .copied()
        .collect();

    // CR 120.3: Collect matching player IDs when the effect also targets players.
    // The player set is part of the same damage event as the object set.
    let matching_players: Vec<PlayerId> = match player_filter {
        Some(pf) => collect_matching_players(state, pf, ability.controller, ability.source_id),
        None => Vec::new(),
    };

    // CR 120.3h: Damage to a battle in `matching_objects` is routed through
    // `apply_damage_to_target` below, which removes defense counters rather
    // than marking damage.
    let ctx = DamageContext::from_source(state, ability.source_id)
        .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller));

    // CR 120.3 + CR 609.7: Assemble the full simultaneous recipient list as a
    // uniform stream of `TargetRef`s. Objects first, then players — CR 120.3
    // does not specify an order within a simultaneous batch, but consistency
    // matters for replacement-drain resumption ordering.
    let mut recipients: Vec<TargetRef> =
        Vec::with_capacity(matching_objects.len() + matching_players.len());
    recipients.extend(matching_objects.iter().map(|&id| TargetRef::Object(id)));
    recipients.extend(matching_players.iter().map(|&pid| TargetRef::Player(pid)));

    for (i, target) in recipients.iter().enumerate() {
        match apply_damage_to_target(state, &ctx, target.clone(), num_dmg, false, events)? {
            DamageResult::Applied(_) => {}
            DamageResult::NeedsChoice => {
                // CR 120.3 + CR 616.1e: Remaining batch recipients must resume after
                // the replacement choice resolves — chain them as DealDamage
                // continuations keyed to the same damage-source id.
                let remaining = recipients[i + 1..].iter().map(|t| (t.clone(), num_dmg));
                stash_remaining_damage_chain(state, ability, ctx.source_id, remaining);
                // Tag the stashed chain with the parent `EffectKind::DamageAll` so the
                // drain re-emits the parent event the non-pause tail fires.
                mark_pending_continuation_parent(state, EffectKind::DamageAll);
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 120.3: Collect non-eliminated players matching the filter for simultaneous
/// damage from a single source. Mirrors the filter evaluation used by
/// `resolve_each_player` but returns only the matching ids.
fn collect_matching_players(
    state: &GameState,
    player_filter: PlayerFilter,
    source_controller: PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| {
            !p.is_eliminated
                && match player_filter {
                    PlayerFilter::Controller => p.id == source_controller,
                    PlayerFilter::All => true,
                    PlayerFilter::Opponent => p.id != source_controller,
                    PlayerFilter::OpponentLostLife => {
                        p.id != source_controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != source_controller && p.life_gained_this_turn > 0
                    }
                    PlayerFilter::HighestSpeed => {
                        let highest_speed = state
                            .players
                            .iter()
                            .filter(|player| !player.is_eliminated)
                            .map(|player| crate::game::speed::effective_speed(state, player.id))
                            .max()
                            .unwrap_or(0);
                        crate::game::speed::effective_speed(state, p.id) == highest_speed
                    }
                    PlayerFilter::ZoneChangedThisWay => state
                        .last_zone_changed_ids
                        .iter()
                        .any(|id| state.objects.get(id).is_some_and(|obj| obj.owner == p.id)),
                    PlayerFilter::OwnersOfCardsExiledBySource => {
                        crate::game::players::owns_card_exiled_by_source(state, p.id, source_id)
                    }
                    PlayerFilter::TriggeringPlayer => state
                        .current_trigger_event
                        .as_ref()
                        .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
                        .is_some_and(|pid| pid == p.id),
                    // CR 120.3 + CR 603.2c: Each opponent other than the triggering opponent.
                    // Falls back to plain Opponent semantics when no trigger event is in scope.
                    PlayerFilter::OpponentOtherThanTriggering => {
                        if p.id == source_controller {
                            return false;
                        }
                        let triggering = state.current_trigger_event.as_ref().and_then(|e| {
                            crate::game::targeting::extract_player_from_event(e, state)
                        });
                        triggering != Some(p.id)
                    }
                }
        })
        .map(|p| p.id)
        .collect()
}

/// CR 120.3: Deal damage to each player matching a filter, with per-player quantity.
/// Resolves `amount` for each player using `resolve_quantity_scoped()`.
/// Used for "deals damage to each player equal to [per-player quantity]" patterns.
pub fn resolve_each_player(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount_expr, player_filter) = match &ability.effect {
        Effect::DamageEachPlayer {
            amount,
            player_filter,
        } => (amount, *player_filter),
        _ => {
            return Err(EffectError::MissingParam(
                "DamageEachPlayer amount".to_string(),
            ))
        }
    };

    let ctx = DamageContext::from_source(state, ability.source_id)
        .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller));

    // Collect matching player IDs first to avoid borrow issues.
    let player_ids: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| {
            !p.is_eliminated
                && match &player_filter {
                    PlayerFilter::Controller => p.id == ability.controller,
                    PlayerFilter::All => true,
                    PlayerFilter::Opponent => p.id != ability.controller,
                    PlayerFilter::OpponentLostLife => {
                        p.id != ability.controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != ability.controller && p.life_gained_this_turn > 0
                    }
                    PlayerFilter::HighestSpeed => {
                        let highest_speed = state
                            .players
                            .iter()
                            .filter(|player| !player.is_eliminated)
                            .map(|player| crate::game::speed::effective_speed(state, player.id))
                            .max()
                            .unwrap_or(0);
                        crate::game::speed::effective_speed(state, p.id) == highest_speed
                    }
                    PlayerFilter::ZoneChangedThisWay => state
                        .last_zone_changed_ids
                        .iter()
                        .any(|id| state.objects.get(id).is_some_and(|obj| obj.owner == p.id)),
                    PlayerFilter::OwnersOfCardsExiledBySource => {
                        crate::game::players::owns_card_exiled_by_source(
                            state,
                            p.id,
                            ability.source_id,
                        )
                    }
                    PlayerFilter::TriggeringPlayer => state
                        .current_trigger_event
                        .as_ref()
                        .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
                        .is_some_and(|pid| pid == p.id),
                    // CR 120.3 + CR 603.2c: Each opponent other than the triggering opponent.
                    // Falls back to plain Opponent semantics when no trigger event is in scope.
                    PlayerFilter::OpponentOtherThanTriggering => {
                        if p.id == ability.controller {
                            return false;
                        }
                        let triggering = state.current_trigger_event.as_ref().and_then(|e| {
                            crate::game::targeting::extract_player_from_event(e, state)
                        });
                        triggering != Some(p.id)
                    }
                }
        })
        .map(|p| p.id)
        .collect();

    for (i, pid) in player_ids.iter().enumerate() {
        // CR 120.3: Resolve quantity scoped to this player.
        let dmg = crate::game::quantity::resolve_quantity_scoped(
            state,
            amount_expr,
            ability.source_id,
            *pid,
        )
        .max(0) as u32;
        if dmg > 0 {
            match apply_damage_to_target(state, &ctx, TargetRef::Player(*pid), dmg, false, events)?
            {
                DamageResult::Applied(_) => {}
                DamageResult::NeedsChoice => {
                    // CR 120.3 + CR 616.1e: Remaining players must resume after the
                    // replacement choice resolves. Pre-resolve per-player amounts now
                    // so each continuation node carries a Fixed quantity.
                    let remaining: Vec<(TargetRef, u32)> = player_ids[i + 1..]
                        .iter()
                        .filter_map(|&next_pid| {
                            let next_dmg = crate::game::quantity::resolve_quantity_scoped(
                                state,
                                amount_expr,
                                ability.source_id,
                                next_pid,
                            )
                            .max(0) as u32;
                            (next_dmg > 0).then_some((TargetRef::Player(next_pid), next_dmg))
                        })
                        .collect();
                    stash_remaining_damage_chain(state, ability, ctx.source_id, remaining);
                    // Tag the stashed chain with the parent `EffectKind::DamageEachPlayer`
                    // so the drain re-emits the parent event the non-pause tail fires.
                    mark_pending_continuation_parent(state, EffectKind::DamageEachPlayer);
                    return Ok(());
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_ability(num_dmg: u32, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed {
                    value: num_dmg as i32,
                },
                target: TargetFilter::Any,
                damage_source: None,
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn deal_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability(3, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].damage_marked, 3);
    }

    #[test]
    fn deal_damage_to_player() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 15);
    }

    #[test]
    fn deal_damage_emits_events() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(2, vec![TargetRef::Player(PlayerId(0))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { amount: 2, .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    /// CR 702.16j + CR 615.1: A player with protection from everything has
    /// all damage to them prevented. The player's life total is unchanged and
    /// a `DamagePrevented` event is emitted.
    #[test]
    fn deal_damage_to_player_with_protection_from_everything_prevented() {
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::keywords::{Keyword, ProtectionTarget};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Teferi's Protection source".to_string(),
            Zone::Battlefield,
        );
        state.add_transient_continuous_effect(
            source,
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );

        let life_before = state.players[1].life;
        let ability = make_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[1].life, life_before,
            "protected player's life must be unchanged"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 5, .. })),
            "expected DamagePrevented event, got {:?}",
            events
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::DamageDealt { .. })),
            "must not emit DamageDealt for prevented damage"
        );
    }

    /// CR 615.5: Phyrexian Hydra — "If damage would be dealt to ~, prevent
    /// that damage. Put a -1/-1 counter on ~ for each 1 damage prevented this
    /// way." The prevention applier emits `DamagePrevented`, stamps
    /// `last_effect_count`, and the post-replacement follow-up resolves
    /// `EventContextAmount` against the prevented amount, putting one -1/-1
    /// counter on the Hydra per prevented point of damage.
    #[test]
    fn phyrexian_hydra_prevention_puts_minus_counters_for_prevented_amount() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, DamageTargetFilter, PreventionAmount, QuantityExpr,
            QuantityRef, ReplacementDefinition,
        };
        use crate::types::counter::CounterType;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let hydra = create_object(
            &mut state,
            CardId(42),
            PlayerId(1),
            "Phyrexian Hydra".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&hydra).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(7);
            obj.toughness = Some(7);
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .damage_target_filter(DamageTargetFilter::CreatureOnly)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            counter_type: "M1M1".to_string(),
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::EventContextAmount,
                            },
                            target: TargetFilter::SelfRef,
                        },
                    ))
                    .description("Phyrexian Hydra prevention shield".to_string()),
            );
        }

        let ability = make_ability(3, vec![TargetRef::Object(hydra)]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let hydra_obj = state.objects.get(&hydra).unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 3, .. })),
            "expected DamagePrevented event with amount 3, got {:?}",
            events
        );
        assert_eq!(
            hydra_obj.damage_marked, 0,
            "prevention must absorb the damage (no marked damage)"
        );
        assert_eq!(
            hydra_obj
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            3,
            "expected 3 -1/-1 counters (one per damage prevented), events: {:?}",
            events
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::DamageDealt { .. })),
            "must not emit DamageDealt for fully prevented damage"
        );
    }

    /// CR 615.5: A 0-damage event should not fire the post-replacement
    /// follow-up — there is nothing to prevent and nothing to count.
    #[test]
    fn phyrexian_hydra_zero_damage_adds_zero_counters() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, DamageTargetFilter, PreventionAmount, QuantityExpr,
            QuantityRef, ReplacementDefinition,
        };
        use crate::types::counter::CounterType;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let hydra = create_object(
            &mut state,
            CardId(42),
            PlayerId(1),
            "Phyrexian Hydra".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&hydra).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(7);
            obj.toughness = Some(7);
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .damage_target_filter(DamageTargetFilter::CreatureOnly)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            counter_type: "M1M1".to_string(),
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::EventContextAmount,
                            },
                            target: TargetFilter::SelfRef,
                        },
                    ))
                    .description("Phyrexian Hydra prevention shield".to_string()),
            );
        }

        let ability = make_ability(0, vec![TargetRef::Object(hydra)]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let hydra_obj = state.objects.get(&hydra).unwrap();
        assert_eq!(
            hydra_obj
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            0,
            "0 damage prevented → 0 counters added"
        );
    }

    #[test]
    fn damage_all_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear1].damage_marked, 2);
        assert_eq!(state.objects[&bear2].damage_marked, 2);
    }

    #[test]
    fn damage_to_planeswalker_removes_loyalty() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            // CR 306.5b: loyalty field and counter map mirror each other.
            obj.loyalty = Some(5);
            obj.counters.insert(CounterType::Loyalty, 5);
        }
        let ability = make_ability(3, vec![TargetRef::Object(pw_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Damage removes loyalty, not damage_marked
        assert_eq!(state.objects[&pw_id].loyalty, Some(2)); // 5 - 3
        assert_eq!(state.objects[&pw_id].damage_marked, 0);
    }

    #[test]
    fn lethal_damage_to_planeswalker_sets_loyalty_zero() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Liliana".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            // CR 306.5b: loyalty field and counter map mirror each other.
            obj.loyalty = Some(2);
            obj.counters.insert(CounterType::Loyalty, 2);
        }
        let ability = make_ability(5, vec![TargetRef::Object(pw_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Damage exceeds loyalty: clamped to 0 via saturating_sub
        assert_eq!(state.objects[&pw_id].loyalty, Some(0));
    }

    /// CR 120.3h + CR 310.6: Damage to a battle removes defense counters equal
    /// to the damage (not damage marked, not loyalty).
    #[test]
    fn damage_to_battle_removes_defense_counters() {
        let mut state = GameState::new_two_player(42);
        let battle_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Test Siege".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&battle_id).unwrap();
            obj.card_types.core_types.push(CoreType::Battle);
            obj.card_types.subtypes.push("Siege".to_string());
            obj.defense = Some(5);
            obj.base_defense = Some(5);
            obj.counters.insert(CounterType::Defense, 5);
        }
        let ability = make_ability(3, vec![TargetRef::Object(battle_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&battle_id];
        assert_eq!(obj.defense, Some(2), "5 - 3 = 2");
        assert_eq!(obj.counters.get(&CounterType::Defense).copied(), Some(2));
        assert_eq!(obj.damage_marked, 0, "battles don't mark damage");
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::CounterRemoved {
                    counter_type: CounterType::Defense,
                    count: 3,
                    ..
                }
            )),
            "CounterRemoved event for 3 defense counters should be emitted"
        );
    }

    /// CR 120.3h: When damage exceeds the battle's defense, it saturates at 0 —
    /// the Siege is not "destroyed" by the damage itself. The zero-defense SBA
    /// (CR 704.5v, tested separately) is what moves it to the graveyard.
    #[test]
    fn lethal_damage_to_battle_clamps_defense_to_zero() {
        let mut state = GameState::new_two_player(42);
        let battle_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Fragile Siege".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&battle_id).unwrap();
            obj.card_types.core_types.push(CoreType::Battle);
            obj.defense = Some(2);
            obj.base_defense = Some(2);
            obj.counters.insert(CounterType::Defense, 2);
        }
        let ability = make_ability(5, vec![TargetRef::Object(battle_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&battle_id].defense, Some(0));
        assert_eq!(
            state.objects[&battle_id]
                .counters
                .get(&CounterType::Defense)
                .copied(),
            Some(0)
        );
    }

    fn make_source_with_keyword(
        state: &mut GameState,
        keyword: crate::types::keywords::Keyword,
    ) -> ObjectId {
        let source_id = create_object(
            state,
            CardId(50),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.keywords.push(keyword);
        source_id
    }

    fn make_ability_with_source(
        num_dmg: u32,
        targets: Vec<TargetRef>,
        source_id: ObjectId,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed {
                    value: num_dmg as i32,
                },
                target: TargetFilter::Any,
                damage_source: None,
            },
            targets,
            source_id,
            PlayerId(0),
        )
    }

    #[test]
    fn lifelink_spell_damage_to_player() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Lifelink);
        let ability = make_ability_with_source(3, vec![TargetRef::Player(PlayerId(1))], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 702.15b: Source controller gains life equal to damage dealt.
        assert_eq!(state.players[1].life, 17); // 20 - 3
        assert_eq!(state.players[0].life, 23); // 20 + 3
    }

    #[test]
    fn lifelink_spell_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Lifelink);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability_with_source(3, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 3);
        // CR 702.15b: Lifelink triggers on creature damage too.
        assert_eq!(state.players[0].life, 23); // 20 + 3
    }

    #[test]
    fn deathtouch_spell_damage_tracked() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Deathtouch);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability_with_source(1, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 1);
        // CR 702.2b: Deathtouch damage tracked for SBA.
        assert!(state.objects[&target_id].dealt_deathtouch_damage);
    }

    #[test]
    fn resolve_all_planeswalker_loyalty() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            // CR 306.5b: loyalty field and counter map mirror each other.
            obj.loyalty = Some(4);
            obj.counters.insert(CounterType::Loyalty, 4);
        }

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Planeswalker],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // CR 120.3c: Damage to planeswalker removes loyalty, not damage_marked.
        assert_eq!(state.objects[&pw_id].loyalty, Some(2));
        assert_eq!(state.objects[&pw_id].damage_marked, 0);
    }

    #[test]
    fn resolve_all_deathtouch_tracked() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Deathtouch);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // CR 702.2b: Deathtouch tracked even through area damage.
        assert!(state.objects[&target_id].dealt_deathtouch_damage);
    }

    #[test]
    fn excess_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(2);
        }
        let ability = make_ability(5, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 120.10: 5 damage to 2-toughness creature = 3 excess
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 3);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn excess_damage_with_deathtouch() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Deathtouch);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Dragon".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(5);
        }
        let ability = make_ability_with_source(3, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 702.2c: Deathtouch makes 1 damage lethal, so 3 - 1 = 2 excess
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 2);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn excess_damage_with_preexisting_damage() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(3);
            obj.damage_marked = 1; // Pre-existing damage
        }
        let ability = make_ability(4, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 120.10: toughness=3, pre-damage=1, lethal=(3-1)=2, excess=4-2=2
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 2);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn excess_damage_to_player_is_zero() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Players don't have excess damage
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 0);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn wither_spell_damage_applies_counters() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Wither);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability_with_source(2, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 702.80: Wither applies -1/-1 counters instead of marking damage.
        assert_eq!(state.objects[&target_id].damage_marked, 0);
        assert_eq!(
            state.objects[&target_id]
                .counters
                .get(&crate::types::counter::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            2
        );
    }

    #[test]
    fn cant_deal_damage_suppresses_source_damage() {
        // CR 120.2: A source with "Can't deal damage" deals zero damage.
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantDealDamage".to_string()))
                    .affected(TargetFilter::SelfRef),
            );
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability_with_source(3, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 0);
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
    }

    #[test]
    fn cant_be_dealt_damage_suppresses_target_damage() {
        // CR 120.1: A target object with "Can't be dealt damage" receives zero damage.
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Ward of Lights".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantBeDealtDamage".to_string()))
                    .affected(TargetFilter::SelfRef),
            );
        let ability = make_ability(3, vec![TargetRef::Object(target_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 0);
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
    }

    #[test]
    fn cant_deal_damage_and_cant_be_dealt_damage_compose() {
        // Bidirectional — both prohibitions active simultaneously still results
        // in zero damage (either guard suffices).
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Inert Attacker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantDealDamage".to_string()))
                    .affected(TargetFilter::SelfRef),
            );
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Shielded Defender".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantBeDealtDamage".to_string()))
                    .affected(TargetFilter::SelfRef),
            );

        let ability = make_ability_with_source(4, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 0);
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
    }

    /// Helper: install an Optional DamageDone replacement on a fresh battlefield
    /// object so every damage event pauses for a player choice.
    fn install_optional_damage_replacement(state: &mut GameState) -> ObjectId {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{ReplacementDefinition, ReplacementMode};
        use crate::types::replacements::ReplacementEvent;

        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut shield = GameObject::new(
            id,
            CardId(999),
            PlayerId(1),
            "Shield".to_string(),
            Zone::Battlefield,
        );
        shield.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Shield".to_string()),
        );
        state.objects.insert(id, shield);
        state.battlefield.push_back(id);
        id
    }

    /// Walk a sub_ability chain and collect each node's (source_id, target, amount).
    /// Used to verify a stashed batch continuation encodes the expected remaining work.
    fn collect_chain_summary(head: &ResolvedAbility) -> Vec<(ObjectId, TargetRef, i32)> {
        let mut out = Vec::new();
        let mut cursor = Some(head);
        while let Some(node) = cursor {
            if let Effect::DealDamage {
                amount: QuantityExpr::Fixed { value },
                ..
            } = &node.effect
            {
                let target = node
                    .targets
                    .first()
                    .cloned()
                    .expect("chain node must carry a target");
                out.push((node.source_id, target, *value));
            }
            cursor = node.sub_ability.as_deref();
        }
        out
    }

    /// CR 120.3 + CR 616.1e: When a DamageAll batch pauses on a replacement
    /// choice after the first target, remaining targets must be stashed as a
    /// chained continuation — not silently dropped. Previously the batch
    /// returned early with no continuation, losing 2/3 of the damage.
    ///
    /// NOTE: This verifies the continuation structure only. End-to-end resume
    /// through `handle_replacement_choice` for Damage events is blocked by a
    /// separate gap in that handler (it only re-applies ZoneChange events).
    #[test]
    fn damage_all_with_replacement_on_first_target() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear1".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear2".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let bear3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear3".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear3)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let source_id = ObjectId(100);
        install_optional_damage_replacement(&mut state);

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        // First target paused on the replacement choice.
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let cont = state
            .pending_continuation
            .as_ref()
            .expect("expected pending_continuation for remaining batch targets");

        // Every remaining creature must be encoded as its own chain node.
        let summary = collect_chain_summary(&cont.chain);
        assert_eq!(
            summary.len(),
            2,
            "two remaining creatures after the paused first; got {summary:?}"
        );
        let expected_targets: Vec<TargetRef> =
            vec![TargetRef::Object(bear2), TargetRef::Object(bear3)];
        let actual_targets: Vec<TargetRef> = summary.iter().map(|(_, t, _)| t.clone()).collect();
        assert_eq!(actual_targets, expected_targets);
        for (node_source, _, amount) in &summary {
            assert_eq!(
                *node_source, source_id,
                "continuation preserves damage source"
            );
            assert_eq!(*amount, 2, "continuation preserves amount");
        }
    }

    /// CR 120.3 + CR 616.1e: DamageEachPlayer must stash remaining players as
    /// continuation nodes after the first player pauses on a replacement choice.
    ///
    /// NOTE: Structural assertion only — see `damage_all_with_replacement_on_first_target`.
    #[test]
    fn damage_each_player_with_replacement() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        install_optional_damage_replacement(&mut state);

        let ability = ResolvedAbility::new(
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Fixed { value: 2 },
                player_filter: PlayerFilter::All,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_each_player(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let cont = state
            .pending_continuation
            .as_ref()
            .expect("expected pending_continuation for remaining-player damage");

        let summary = collect_chain_summary(&cont.chain);
        assert_eq!(
            summary.len(),
            1,
            "one remaining player (PlayerId(1)) after the paused first; got {summary:?}"
        );
        assert_eq!(summary[0].1, TargetRef::Player(PlayerId(1)));
        assert_eq!(summary[0].2, 2);
        assert_eq!(summary[0].0, source_id);
    }

    /// CR 120.3 + CR 616.1e: Multi-target `DealDamage` ("deal 1 to any number of
    /// targets") must stash remaining targets after the first pauses.
    ///
    /// NOTE: Structural assertion only — see `damage_all_with_replacement_on_first_target`.
    #[test]
    fn deal_damage_multi_target_with_replacement_on_first_target() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        install_optional_damage_replacement(&mut state);

        let ability = make_ability(1, vec![TargetRef::Object(a), TargetRef::Object(b)]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let cont = state
            .pending_continuation
            .as_ref()
            .expect("expected pending_continuation for remaining multi-target damage");
        let summary = collect_chain_summary(&cont.chain);
        assert_eq!(summary.len(), 1, "one remaining target; got {summary:?}");
        assert_eq!(summary[0].1, TargetRef::Object(b));
        assert_eq!(summary[0].2, 1);
    }

    /// CR 120.3: DamageAll paused mid-resolution by a replacement proposal must
    /// re-emit `EffectKind::DamageAll` after the drain so trigger matchers keyed
    /// on the parent kind observe the event on the pause-and-resume path the
    /// same way they do on the non-pause tail.
    #[test]
    fn damage_all_replacement_accepted_emits_parent_effect_resolved() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(1);
        // Two creatures on the battlefield so DamageAll has at least one
        // follow-up target queued behind the paused first target.
        let grizzly = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&grizzly)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let ogre = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ogre".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&ogre)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Single-use Optional DamageDone replacement — the first damage event
        // surfaces a ReplacementChoice prompt.
        install_optional_damage_replacement(&mut state);

        state.priority_player = PlayerId(0);
        state.active_player = PlayerId(0);

        // "Deal 1 damage to each creature".
        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: None,
            },
            vec![],
            ogre,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).expect("DamageAll initial resolve");

        assert_eq!(
            state
                .pending_continuation
                .as_ref()
                .and_then(|c| c.parent_kind),
            Some(EffectKind::DamageAll),
            "the stashed continuation must carry EffectKind::DamageAll so the drain re-emits the parent event",
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));

        // Accept the replacement — the drain resolves the chain and emits DamageAll.
        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept DamageAll replacement");
        let damage_all_events = result
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::DamageAll,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            damage_all_events, 1,
            "DamageAll parent event must fire exactly once on pause-and-resume; got events = {:#?}",
            result.events,
        );
        assert!(
            state.pending_continuation.is_none(),
            "continuation must be consumed after drain"
        );
    }

    /// CR 120.3: DamageEachPlayer paused mid-resolution by a replacement must
    /// re-emit `EffectKind::DamageEachPlayer` after the drain.
    #[test]
    fn damage_each_player_replacement_accepted_emits_parent_effect_resolved() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::PlayerFilter;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(2);
        state.priority_player = PlayerId(0);
        state.active_player = PlayerId(0);

        // Optional DamageDone shield — first player damaged in APNAP order
        // surfaces a ReplacementChoice prompt.
        install_optional_damage_replacement(&mut state);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Fixed { value: 1 },
                player_filter: PlayerFilter::All,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_each_player(&mut state, &ability, &mut events)
            .expect("DamageEachPlayer initial resolve");

        assert_eq!(
            state
                .pending_continuation
                .as_ref()
                .and_then(|c| c.parent_kind),
            Some(EffectKind::DamageEachPlayer),
            "the stashed continuation must carry EffectKind::DamageEachPlayer",
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));

        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept DamageEachPlayer replacement");
        let each_player_events = result
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::DamageEachPlayer,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            each_player_events, 1,
            "DamageEachPlayer parent event must fire exactly once on pause-and-resume; got events = {:#?}",
            result.events,
        );
        assert!(
            state.pending_continuation.is_none(),
            "continuation must be consumed after drain"
        );
    }

    /// CR 120.3 + CR 609.7: Goblin Chainwhirler-style mixed damage — a single
    /// `DamageAll` with both `target` (objects) and `player_filter` (players)
    /// populated must deal damage to the full recipient set from ONE source as
    /// one simultaneous effect. This is what allows replacement/prevention
    /// shields like Awe Strike ("the next time a source would deal damage …")
    /// to observe the whole batch as one coherent event.
    #[test]
    fn damage_all_mixed_players_and_objects_single_source() {
        use crate::types::ability::PlayerFilter;
        use crate::types::ability::TypedFilter;

        let mut state = GameState::new_two_player(42);

        // Source controlled by PlayerId(0); opponents are just PlayerId(1) here.
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chainwhirler".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Opponent's creature and planeswalker — both must take damage.
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opp_pw = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&opp_pw).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(5);
            obj.counters.insert(CounterType::Loyalty, 5);
        }

        // Controller's own creature MUST NOT take damage — controller=Opponent.
        let own_creature = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Own Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&own_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Mirror the parser output for "deals 1 damage to each opponent and each
        // creature and planeswalker they control".
        use crate::types::ability::{ControllerRef, TypeFilter};
        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::Opponent),
                            properties: vec![],
                        }),
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Planeswalker],
                            controller: Some(ControllerRef::Opponent),
                            properties: vec![],
                        }),
                    ],
                },
                player_filter: Some(PlayerFilter::Opponent),
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        let life_before = state.players[1].life;
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).expect("mixed DamageAll resolves");

        // CR 120.3a: opponent lost 1 life.
        assert_eq!(state.players[1].life, life_before - 1);
        // CR 120.3e: opponent's creature marked 1.
        assert_eq!(state.objects[&opp_creature].damage_marked, 1);
        // CR 120.3c: opponent's planeswalker lost 1 loyalty (5 - 1).
        assert_eq!(state.objects[&opp_pw].loyalty, Some(4));
        // controller's own creature untouched.
        assert_eq!(state.objects[&own_creature].damage_marked, 0);

        // CR 609.7: ALL damage events must share one source_id — the single
        // damage source that replacement shields (Awe Strike et al.) watch.
        let damage_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamageDealt { source_id, .. } => Some(*source_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            damage_events.len(),
            3,
            "expected 3 DamageDealt events (opponent player + creature + planeswalker), got {damage_events:?}",
        );
        for src in &damage_events {
            assert_eq!(
                *src, source_id,
                "every damage event in the batch must carry the single source id",
            );
        }

        // CR 120.3: exactly ONE `EffectResolved { DamageAll }` — the whole batch
        // is one effect resolution, not two.
        let effect_resolved_count = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::DamageAll,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            effect_resolved_count, 1,
            "mixed DamageAll must produce exactly one EffectResolved event",
        );
    }

    /// CR 120.3 + CR 603.2c: `PlayerFilter::OpponentOtherThanTriggering` excludes
    /// the controller AND the triggering player (extracted from `current_trigger_event`).
    /// Hydra Omnivore: combat damage to opponent A → trigger fires →
    /// "deals that much damage to each other opponent" hits opponents B and C
    /// but skips A (already took combat damage).
    #[test]
    fn damage_each_other_opponent_excludes_triggering_player_in_3p() {
        use crate::types::ability::PlayerFilter;
        use crate::types::events::GameEvent;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let controller = PlayerId(0);
        let triggered_opponent = PlayerId(1);
        let other_opponent = PlayerId(2);
        let source_id = ObjectId(1000);

        // Set the triggering event to "DamageDealt to opponent A". The
        // resolver reads this via extract_player_from_event to identify the
        // triggering player.
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Player(triggered_opponent),
            amount: 5,
            is_combat: true,
            excess: 0,
        });

        let ability = ResolvedAbility::new(
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Fixed { value: 5 },
                player_filter: PlayerFilter::OpponentOtherThanTriggering,
            },
            vec![],
            source_id,
            controller,
        );

        let life_controller_before = state.players[controller.0 as usize].life;
        let life_triggered_before = state.players[triggered_opponent.0 as usize].life;
        let life_other_before = state.players[other_opponent.0 as usize].life;

        let mut events = Vec::new();
        resolve_each_player(&mut state, &ability, &mut events)
            .expect("DamageEachPlayer{OpponentOtherThanTriggering} resolves cleanly");

        // Controller never takes damage (always excluded from any opponent filter).
        assert_eq!(
            state.players[controller.0 as usize].life, life_controller_before,
            "controller must not take damage"
        );
        // Triggered opponent did NOT take additional damage from the trigger
        // body (already took combat damage as the trigger event).
        assert_eq!(
            state.players[triggered_opponent.0 as usize].life, life_triggered_before,
            "triggering opponent must be excluded from each-other-opponent damage"
        );
        // Other opponent took full 5 damage.
        assert_eq!(
            state.players[other_opponent.0 as usize].life,
            life_other_before - 5,
            "non-triggering opponent must receive the source's damage"
        );
    }

    /// Without `current_trigger_event` set, `OpponentOtherThanTriggering`
    /// degrades to plain `Opponent` semantics — every opponent except the
    /// controller is hit. Verifies the safety fallback for non-trigger
    /// activation paths.
    #[test]
    fn damage_each_other_opponent_falls_back_to_opponent_when_no_trigger_event() {
        use crate::types::ability::PlayerFilter;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        // No current_trigger_event set — fallback path.
        assert!(state.current_trigger_event.is_none());

        let source_id = ObjectId(1000);
        let ability = ResolvedAbility::new(
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Fixed { value: 3 },
                player_filter: PlayerFilter::OpponentOtherThanTriggering,
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        let life_p0_before = state.players[0].life;
        let life_p1_before = state.players[1].life;
        let life_p2_before = state.players[2].life;

        let mut events = Vec::new();
        resolve_each_player(&mut state, &ability, &mut events).expect("fallback resolves cleanly");

        assert_eq!(
            state.players[0].life, life_p0_before,
            "controller unchanged"
        );
        assert_eq!(
            state.players[1].life,
            life_p1_before - 3,
            "opponent 1 takes damage in fallback"
        );
        assert_eq!(
            state.players[2].life,
            life_p2_before - 3,
            "opponent 2 takes damage in fallback"
        );
    }

    /// CR 120.3: `DamageAll` with `player_filter: Some(Opponent)` deals damage
    /// to BOTH the typed object set and every opponent. Omnath, Locus of
    /// Creation 3rd-branch shape: 4 damage to each opponent + 4 damage to
    /// each non-controller planeswalker.
    #[test]
    fn damage_all_with_player_filter_opponent_hits_both_sets_in_3p() {
        use crate::types::ability::{ControllerRef, PlayerFilter, TypeFilter};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let controller = PlayerId(0);
        let opp_a = PlayerId(1);
        let opp_b = PlayerId(2);

        // Set up: controller's own planeswalker (must NOT take damage),
        // opponent A's planeswalker, opponent B's planeswalker.
        let make_pw = |state: &mut GameState, owner: PlayerId, name: &str| -> ObjectId {
            let id = create_object(
                state,
                CardId(state.objects.len() as u64 + 1),
                owner,
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(6);
            obj.counters.insert(CounterType::Loyalty, 6);
            id
        };
        let own_pw = make_pw(&mut state, controller, "Own PW");
        let opp_a_pw = make_pw(&mut state, opp_a, "A PW");
        let opp_b_pw = make_pw(&mut state, opp_b, "B PW");

        let source_id = ObjectId(2000);
        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Planeswalker],
                    controller: Some(ControllerRef::Opponent),
                    properties: vec![],
                }),
                player_filter: Some(PlayerFilter::Opponent),
            },
            vec![],
            source_id,
            controller,
        );

        let life_controller_before = state.players[controller.0 as usize].life;
        let life_opp_a_before = state.players[opp_a.0 as usize].life;
        let life_opp_b_before = state.players[opp_b.0 as usize].life;

        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).expect("composite DamageAll resolves");

        // Controller: untouched (life and own PW).
        assert_eq!(
            state.players[controller.0 as usize].life,
            life_controller_before
        );
        assert_eq!(state.objects[&own_pw].loyalty, Some(6));
        // Both opponents: -4 life.
        assert_eq!(state.players[opp_a.0 as usize].life, life_opp_a_before - 4);
        assert_eq!(state.players[opp_b.0 as usize].life, life_opp_b_before - 4);
        // Both opponents' planeswalkers: -4 loyalty (6 - 4 = 2).
        assert_eq!(state.objects[&opp_a_pw].loyalty, Some(2));
        assert_eq!(state.objects[&opp_b_pw].loyalty, Some(2));
    }
}
