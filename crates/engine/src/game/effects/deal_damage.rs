use std::collections::HashSet;

use crate::game::ability_utils::append_to_sub_chain;
use crate::game::effects::player_counter;
use crate::game::effects::{append_to_pending_continuation, mark_pending_continuation_parent};
use crate::game::filter;
use crate::game::keywords;
use crate::game::quantity::{
    quantity_expr_uses_recipient, resolve_quantity_with_targets,
    resolve_quantity_with_targets_and_recipient, resolve_quantity_with_targets_slice,
};
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    DamageContextSnapshot, DamageSource, Effect, EffectError, EffectKind, PlayerFilter,
    QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{DamageRecord, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::KeywordKind;
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::proposed_event::ProposedEvent;

/// Source attributes needed for damage application (CR 120.3).
/// Read from the source object before the mutable damage phase to avoid borrow conflicts.
#[derive(Clone, Copy)]
pub(crate) struct DamageContext {
    pub(crate) source_id: ObjectId,
    pub(crate) controller: PlayerId,
    pub(crate) source_is_creature: bool,
    pub(crate) has_deathtouch: bool,
    pub(crate) has_lifelink: bool,
    pub(crate) has_wither: bool,
    pub(crate) has_infect: bool,
    pub(crate) combat_damage_poison: u32,
}

fn player_context_target(
    state: &GameState,
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
) -> Option<TargetRef> {
    if matches!(target_filter, TargetFilter::SourceChosenPlayer) {
        // CR 607.2d + CR 608.2c: Resolve "the chosen player" from the
        // source's linked persisted choice.
        return crate::game::game_object::source_chosen_player(state, ability.source_id)
            .map(TargetRef::Player);
    }

    // CR 608.2c + CR 109.4: A "that player" / "its controller" anaphor bound to a
    // chosen parent target (e.g. Star Athlete's "choose up to one target nonland
    // permanent. Its controller may sacrifice it. If they don't, this creature
    // deals 5 damage to that player.") has no referent when no target was chosen
    // — "up to one target" with zero chosen. The damage must then do nothing.
    // Resolve strictly from the chosen parent target so an absent referent yields
    // `None` (no damage), rather than routing through `resolve_player_for_context_ref`
    // whose event-context fallback would mis-deal the damage to the trigger
    // source's own controller. The normal (target-chosen) path is unchanged:
    // `resolve_player_for_context_ref` already resolves this case via
    // `parent_target_controller` first. `ParentTargetOwner` is intentionally NOT
    // routed here — it relies on its AttachedTo fallback for Aura phase triggers
    // (Enslave's "enchanted creature deals 1 damage to its owner").
    if matches!(target_filter, TargetFilter::ParentTargetController) {
        return crate::game::ability_utils::parent_target_controller(ability, state)
            .map(TargetRef::Player);
    }

    if matches!(
        target_filter,
        TargetFilter::Controller
            | TargetFilter::OriginalController
            | TargetFilter::ScopedPlayer
            | TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringPlayer
            | TargetFilter::DefendingPlayer
            | TargetFilter::ParentTargetOwner
            | TargetFilter::PostReplacementSourceController
    ) {
        Some(TargetRef::Player(super::resolve_player_for_context_ref(
            state,
            ability,
            target_filter,
        )))
    } else {
        None
    }
}

impl DamageContext {
    /// Build context by reading keywords from the source object.
    /// Returns None if source doesn't exist in state.
    pub(crate) fn from_source(state: &GameState, source_id: ObjectId) -> Option<Self> {
        state.objects.get(&source_id).map(|obj| Self {
            source_id,
            controller: obj.controller,
            source_is_creature: obj.card_types.core_types.contains(&CoreType::Creature),
            // CR 613.1f + CR 702.2 + CR 702.15 + CR 702.80 + CR 702.90:
            // Off-battlefield keyword grants (e.g. Judith's "that spell gains
            // deathtouch and lifelink") live in transient continuous effects and
            // are visible via `object_has_effective_keyword_kind`, not printed
            // `obj.keywords`.
            has_deathtouch: keywords::object_has_effective_keyword_kind(
                state,
                source_id,
                KeywordKind::Deathtouch,
            ),
            has_lifelink: keywords::object_has_effective_keyword_kind(
                state,
                source_id,
                KeywordKind::Lifelink,
            ),
            has_wither: keywords::object_has_effective_keyword_kind(
                state,
                source_id,
                KeywordKind::Wither,
            ),
            has_infect: keywords::object_has_effective_keyword_kind(
                state,
                source_id,
                KeywordKind::Infect,
            ),
            // CR 702.164b: total toxic value = sum of N over ALL effective toxic
            // instances (printed + granted, on/off battlefield), matching the
            // sibling effective-keyword flags above rather than reading printed
            // `obj.keywords` directly.
            combat_damage_poison: keywords::effective_total_toxic_value(state, source_id),
        })
    }

    /// Fallback context when source no longer exists (all keyword flags false).
    /// CR 702.15c: last known information should be used for lifelink, but if the
    /// source is truly gone with no LKI available, defaulting to false is safe.
    pub(crate) fn fallback(source_id: ObjectId, controller: PlayerId) -> Self {
        Self {
            source_id,
            controller,
            source_is_creature: false,
            has_deathtouch: false,
            has_lifelink: false,
            has_wither: false,
            has_infect: false,
            combat_damage_poison: 0,
        }
    }
}

impl From<DamageContextSnapshot> for DamageContext {
    fn from(snapshot: DamageContextSnapshot) -> Self {
        Self {
            source_id: snapshot.source_id,
            controller: snapshot.controller,
            source_is_creature: snapshot.source_is_creature,
            has_deathtouch: snapshot.has_deathtouch,
            has_lifelink: snapshot.has_lifelink,
            has_wither: snapshot.has_wither,
            has_infect: snapshot.has_infect,
            combat_damage_poison: snapshot.combat_damage_poison,
        }
    }
}

impl From<&DamageContext> for DamageContextSnapshot {
    fn from(ctx: &DamageContext) -> Self {
        Self {
            source_id: ctx.source_id,
            controller: ctx.controller,
            source_is_creature: ctx.source_is_creature,
            has_deathtouch: ctx.has_deathtouch,
            has_lifelink: ctx.has_lifelink,
            has_wither: ctx.has_wither,
            has_infect: ctx.has_infect,
            combat_damage_poison: ctx.combat_damage_poison,
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
/// infect (CR 702.90), toxic (CR 702.164c), deathtouch (CR 702.2b),
/// lifelink (CR 702.15b), and
/// DamageDealt event emission.
///
/// Event ordering: DamageDealt is emitted before lifelink LifeChanged.
/// EffectResolved is NOT emitted — that remains the caller's responsibility.
///
/// Returns `DamageResult::Applied(actual_amount)` or `DamageResult::NeedsChoice`.
/// CR 120.2 + CR 120.8 + CR 702.16: Pre-replacement damage gate. Applies the
/// "would deal 0", source-side `CantDealDamage`, target-side `CantBeDealtDamage`,
/// object protection, and player protection-from-everything checks that run
/// *before* the CR 614/615 replacement pipeline.
///
/// Returns `Some(ProposedEvent::Damage)` to proceed into the replacement
/// pipeline, or `None` when the damage is fully gated (the gate has already
/// pushed a `DamagePrevented` event where the rules require one). Shared by the
/// single-source `apply_damage_to_target` path and the combat-damage batch path
/// so both run identical pre-pipeline gating.
pub(crate) fn pre_replacement_damage_gate(
    state: &GameState,
    ctx: &DamageContext,
    target: &TargetRef,
    amount: u32,
    is_combat: bool,
    events: &mut Vec<GameEvent>,
) -> Option<ProposedEvent> {
    // CR 120.8: If a source would deal 0 damage, it does not deal damage at all.
    if amount == 0 {
        return None;
    }

    // CR 120.2: Source-side "can't deal damage" prohibition. The source deals
    // zero damage of any kind, regardless of target.
    if crate::game::static_abilities::object_has_static_other(
        state,
        ctx.source_id,
        "CantDealDamage",
    ) {
        return None;
    }

    // CR 120.1: Target-side "can't be dealt damage" prohibition (objects only;
    // `CantBeDealtDamage` in the static registry is object-scoped).
    if let TargetRef::Object(target_obj_id) = target {
        if crate::game::static_abilities::object_has_static_other(
            state,
            *target_obj_id,
            "CantBeDealtDamage",
        ) {
            return None;
        }
    }

    // CR 702.16b + CR 702.16e: Protection prevents damage from sources with the matching quality.
    // Emits DamagePrevented so "when damage is prevented" triggers can fire.
    if let TargetRef::Object(target_obj_id) = target {
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
                return None;
            }
        }
    }

    // CR 702.16e + CR 615.1: "All damage that would be dealt to [a player with
    // protection from the damage source] is prevented." Mirror the object-
    // protection gate above for player targets. Emits DamagePrevented so
    // prevention-triggered abilities still observe the event.
    if let TargetRef::Player(player_id) = target {
        if crate::game::static_abilities::player_protection_from(
            state,
            *player_id,
            Some(ctx.source_id),
        ) {
            events.push(GameEvent::DamagePrevented {
                source_id: ctx.source_id,
                target: target.clone(),
                amount,
            });
            return None;
        }
    }

    Some(ProposedEvent::Damage {
        source_id: ctx.source_id,
        target: target.clone(),
        amount,
        is_combat,
        applied: HashSet::new(),
    })
}

pub(crate) fn apply_damage_to_target(
    state: &mut GameState,
    ctx: &DamageContext,
    target: TargetRef,
    amount: u32,
    is_combat: bool,
    events: &mut Vec<GameEvent>,
) -> Result<DamageResult, EffectError> {
    let Some(proposed) =
        pre_replacement_damage_gate(state, ctx, &target, amount, is_combat, events)
    else {
        return Ok(DamageResult::Applied(0));
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => Ok(apply_damage_after_replacement(
            state, ctx, event, is_combat, events,
        )),
        ReplacementResult::Prevented => {
            // CR 615.5: A prevention effect's additional effect (e.g.
            // Phyrexian Hydra's "Put a -1/-1 counter on ~ for each 1 damage
            // prevented this way") is stashed as `post_replacement_continuation`
            // by the prevention applier. Resolve it inline here so the follow-up
            // takes place "immediately afterward" as the rule requires. The
            // applier already stamped `state.last_effect_count` with the
            // prevented amount so `EventContextAmount` resolves correctly.
            //
            // CR 510.2 + CR 615.13: Combat damage is exempt from this inline
            // path — combat damage resolves as a simultaneous batch, and its
            // prevention riders fire once post-batch in `combat_damage.rs`
            // against the aggregate prevented amount. Firing inline here would
            // re-fire the rider once per attacker against a fragmented count.
            if !is_combat && state.post_replacement_continuation.is_some() {
                // CR 615.5 + CR 609.7: leave `post_replacement_event_source`
                // populated for the call so `TargetFilter::PostReplacementSourceController`
                // can resolve against the prevented event's damage source. Clear
                // after the call to prevent leakage into unrelated later
                // replacements.
                let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
                    state, None, None, None, events,
                );
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
/// (CR 120.3c / CR 306.8), creature damage marking (CR 120.3e), poison
/// (CR 702.90 / CR 702.164c),
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

    // CR 120.10: Excess damage to a planeswalker/battle is measured against its
    // loyalty/defense *before* this damage was dealt. Damage application removes
    // those counters (clamping at 0), which destroys the pre-hit value, so capture
    // it here before mutating. (Creatures mark — not clamp — damage, so their
    // excess is reconstructed from `damage_marked` below.)
    let (is_creature, is_planeswalker, is_battle, loyalty_before, defense_before) = match t {
        TargetRef::Object(obj_id) => state
            .objects
            .get(obj_id)
            .map(|obj| {
                (
                    obj.card_types.core_types.contains(&CoreType::Creature),
                    obj.card_types.core_types.contains(&CoreType::Planeswalker),
                    obj.card_types.core_types.contains(&CoreType::Battle),
                    obj.loyalty,
                    obj.defense,
                )
            })
            .unwrap_or((false, false, false, None, None)),
        TargetRef::Player(_) => (false, false, false, None, None),
    };

    match t {
        TargetRef::Object(obj_id) => {
            if is_planeswalker {
                // CR 120.3c + CR 306.8: Damage to a planeswalker removes that
                // many loyalty counters. Routed through the single-authority
                // resolver so replacement effects apply and obj.loyalty stays
                // in sync with counters[Loyalty] (CR 306.5b).
                super::counters::remove_counter_with_replacement(
                    state,
                    *obj_id,
                    CounterType::Loyalty,
                    actual_amount,
                    events,
                );
            }

            if is_battle {
                // CR 120.3h + CR 310.6: Damage to a battle removes that many
                // defense counters. Routed through the single-authority resolver
                // so obj.defense stays in sync with counters[Defense] (CR 310.4c).
                super::counters::remove_counter_with_replacement(
                    state,
                    *obj_id,
                    CounterType::Defense,
                    actual_amount,
                    events,
                );
            }

            if is_creature && (ctx.has_wither || ctx.has_infect) {
                // CR 120.3d + CR 702.80 + CR 702.90: Wither/infect damage to a
                // creature is dealt as -1/-1 counters.
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
                crate::game::layers::mark_layers_full(state);
            } else if is_creature {
                if let Some(target_obj) = state.objects.get_mut(obj_id) {
                    // CR 120.3e: Damage to a creature marks damage.
                    target_obj.damage_marked += actual_amount;
                    // CR 702.2b: Track deathtouch for SBA lethal-damage check.
                    if ctx.has_deathtouch {
                        target_obj.dealt_deathtouch_damage = true;
                    }
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
                // CR 120.3b + CR 614.17: Infect deals damage to players as poison
                // counters. Route through the player-counter replacement pipeline
                // so "players can't get poison counters" / poison-doublers apply;
                // the actor is the source's controller.
                if !player_counter::add_player_counter_with_replacement(
                    state,
                    ctx.controller,
                    *player_id,
                    PlayerCounterKind::Poison,
                    actual_amount,
                    events,
                ) {
                    return DamageResult::NeedsChoice;
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
            if is_combat
                && actual_amount > 0
                && ctx.source_is_creature
                && ctx.combat_damage_poison > 0
            {
                // CR 120.3g + CR 702.164c + CR 614.17: Toxic adds poison counters
                // when a creature deals combat damage to a player. Route through
                // the player-counter replacement pipeline (prevention/doublers);
                // the actor is the source's controller.
                if !player_counter::add_player_counter_with_replacement(
                    state,
                    ctx.controller,
                    *player_id,
                    PlayerCounterKind::Poison,
                    ctx.combat_damage_poison,
                    events,
                ) {
                    return DamageResult::NeedsChoice;
                }
            }
        }
    }

    // CR 120.10: Compute excess damage beyond lethal/loyalty/defense. If a
    // permanent has multiple card types among creature, planeswalker, and battle,
    // its excess damage is the greatest calculated amount among those types.
    let excess = match &t {
        TargetRef::Object(obj_id) => state
            .objects
            .get(obj_id)
            .map(|obj| {
                let mut excess = 0;

                if obj.card_types.core_types.contains(&CoreType::Creature) {
                    if let Some(toughness) = obj.toughness {
                        // damage_marked already includes actual_amount.
                        let damage_before = obj.damage_marked.saturating_sub(actual_amount);
                        let lethal = if ctx.has_deathtouch {
                            // CR 702.2c: Any nonzero damage from deathtouch = lethal.
                            if damage_before == 0 {
                                1u32
                            } else {
                                0
                            }
                        } else {
                            (toughness as u32).saturating_sub(damage_before)
                        };
                        excess = excess.max(actual_amount.saturating_sub(lethal));
                    }
                }

                if obj.card_types.core_types.contains(&CoreType::Planeswalker) {
                    // CR 120.10: Excess for a planeswalker = damage beyond the
                    // loyalty it had before the damage was dealt (captured above,
                    // since the loyalty counters have since been removed).
                    excess = excess.max(actual_amount.saturating_sub(loyalty_before.unwrap_or(0)));
                }

                if obj.card_types.core_types.contains(&CoreType::Battle) {
                    // CR 120.10: Excess for a battle = damage beyond the defense it
                    // had before the damage was dealt (captured above).
                    excess = excess.max(actual_amount.saturating_sub(defense_before.unwrap_or(0)));
                }

                excess
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
        let target_controller = match t {
            TargetRef::Player(player_id) => *player_id,
            TargetRef::Object(object_id) => state
                .objects
                .get(object_id)
                .map(|object| object.controller)
                .unwrap_or(ctx.controller),
        };
        // CR 608.2i + CR 608.2h: Snapshot the damage source's characteristics at
        // damage time so look-back source-filter queries ("opponents who were
        // dealt combat damage by ~ or a Dragon this turn") evaluate against the
        // source as it was when the damage was dealt — the source may later
        // change type, leave the battlefield (CR 113.7a LKI), or be removed.
        let src = state.objects.get(&ctx.source_id);
        let mut record = DamageRecord {
            source_id: ctx.source_id,
            source_controller: ctx.controller,
            target: t.clone(),
            target_controller,
            amount: actual_amount,
            is_combat,
            // CR 120.10: Record excess so "was dealt excess damage this turn"
            // intervening-if conditions can query without re-computing lethal.
            excess,
            // CR 608.2i + CR 608.2h: the obj-derived source snapshot below
            // overwrites these when the source still exists; the empty/default
            // tail (Default::default()) covers the source-already-gone case.
            source_controller_snapshot: ctx.controller,
            source_owner: ctx.controller,
            ..Default::default()
        };
        if let Some(obj) = src {
            record.source_name = obj.name.clone();
            record.source_core_types = obj.card_types.core_types.clone();
            record.source_subtypes = obj.card_types.subtypes.clone();
            record.source_supertypes = obj.card_types.supertypes.clone();
            record.source_keywords = obj.keywords.clone();
            record.source_power = obj.power;
            record.source_toughness = obj.toughness;
            record.source_colors = obj.color.clone();
            // CR 202.3e: include cost_x_paid for on-stack spells.
            record.source_mana_value = obj.mana_cost.mana_value_with_x(obj.zone, obj.cost_x_paid);
            record.source_controller_snapshot = obj.controller;
            record.source_owner = obj.owner;
            // CR 608.2i: snapshot the source's zone (Stack for a spell,
            // Battlefield for a permanent) so a zone-discriminating look-back
            // source filter evaluates against the zone as it was at damage time.
            record.source_zone = obj.zone;
        }
        state.damage_dealt_this_turn.push_back(record);
        // CR 120.3 + CR 120.6 + CR 702.11b + CR 613.1f: Mark the source as having
        // actually dealt damage (this branch is gated on `actual_amount > 0`, i.e.
        // a nonzero amount actually dealt per CR 120.3/120.6, not the would-be
        // amount of CR 120.1a). Only battlefield-resident sources carry this
        // sticky flag, since the reset in `apply_zone_exit_cleanup` is gated on a
        // battlefield exit. When the id is NEWLY inserted, mark layers fully dirty
        // so `flush_layers` recomputes the materialized keyword set that
        // `has_hexproof` reads — otherwise the conditional hexproof grant for
        // "has hexproof if it hasn't dealt damage yet" would never drop at the
        // targeting check (a Clean `layers_dirty` no-ops `flush_layers`).
        if state
            .objects
            .get(&ctx.source_id)
            .is_some_and(|obj| obj.zone == crate::types::zones::Zone::Battlefield)
            && state.objects_that_dealt_damage.insert(ctx.source_id)
        {
            state.layers_dirty.mark_full();
        }
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

/// CR 120.4b: Build a one-shot continuation that applies an already-replaced
/// damage event. Unlike `build_remaining_damage_node`, this does not route back
/// through the replacement pipeline.
fn build_post_replacement_damage_node(
    ctx: &DamageContext,
    event: &ProposedEvent,
) -> Option<ResolvedAbility> {
    let ProposedEvent::Damage {
        target,
        amount,
        is_combat,
        ..
    } = event
    else {
        return None;
    };

    Some(ResolvedAbility::new(
        Effect::ApplyPostReplacementDamage {
            context: DamageContextSnapshot::from(ctx),
            target: target.clone(),
            amount: *amount,
            is_combat: *is_combat,
        },
        Vec::new(),
        ctx.source_id,
        ctx.controller,
    ))
}

/// CR 120.4b + CR 616.1e: Stash remaining already-replaced damage survivors so
/// a nested life/lifelink replacement choice can resolve before Phase C
/// continues, without re-running damage replacement/prevention selection.
fn stash_remaining_post_replacement_damage(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining: &[(&DamageContext, ProposedEvent)],
) {
    let mut iter = remaining
        .iter()
        .filter_map(|(ctx, event)| build_post_replacement_damage_node(ctx, event));
    let Some(mut head) = iter.next() else {
        if let Some(sub) = ability.sub_ability.as_ref() {
            append_to_pending_continuation(state, Some(Box::new(sub.as_ref().clone())));
        }
        return;
    };

    for node in iter {
        append_to_sub_chain(&mut head, node);
    }
    if let Some(sub) = ability.sub_ability.as_ref() {
        append_to_sub_chain(&mut head, sub.as_ref().clone());
    }
    append_to_pending_continuation(state, Some(Box::new(head)));
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

/// CR 120.1 + CR 616.1e: Stash a remaining-source damage continuation where each
/// node carries its OWN damage-source id. Unlike `stash_remaining_damage_chain`
/// (single source for every node), this preserves PER-SOURCE identity through a
/// replacement pause in the `EachTarget` simultaneous batch: each resumed
/// `DealDamage` node reproduces its own source's keywords/LKI via
/// `DamageContext::from_source` (CR 120.1: each source is independent), so a
/// granted deathtouch/lifelink/wither/infect on one paused source is not lost or
/// mis-attributed to another source on resume.
fn stash_remaining_each_source_damage(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining: impl IntoIterator<Item = (ObjectId, TargetRef, u32)>,
) {
    let controller = ability.controller;
    let mut iter = remaining.into_iter();
    let Some((first_source, first_target, first_amount)) = iter.next() else {
        // No remaining batch work — forward the parent's sub_ability so the
        // downstream chain resumes after the pending replacement choice resolves.
        if let Some(sub) = ability.sub_ability.as_ref() {
            append_to_pending_continuation(state, Some(Box::new(sub.as_ref().clone())));
        }
        return;
    };

    let mut head =
        build_remaining_damage_node(first_source, controller, first_target, first_amount);
    for (source_id, target, amount) in iter {
        let node = build_remaining_damage_node(source_id, controller, target, amount);
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
                resolve_quantity_with_targets(state, amount, ability).max(0) as u32,
                *damage_source,
                target,
            ),
            _ => return Err(EffectError::MissingParam("DealDamage amount".to_string())),
        };

    // CR 120.1 + CR 601.2c + CR 208.1 + CR 608.2: Multi-source per-power damage —
    // "up to N / any number of target creatures you control each deal damage
    // equal to their power to <recipient>". Every leading object target is an
    // independent source; the LAST object target is the shared recipient. Each
    // source deals damage equal to its OWN power (CR 208.1: power is a modifiable
    // characteristic; CR 608.2: read as the effect resolves), re-resolved against
    // a single-source target slice so `Power{Target}` reads that member, not the
    // first slot. Diverges from the single-source `Target` path (which reads
    // `targets[0]` and damages `targets[1..]`), so it is handled separately.
    if matches!(damage_source, Some(DamageSource::EachTarget)) {
        return resolve_each_target_power_damage(state, ability, target_filter, events);
    }

    // CR 120.3: Determine damage source.
    let ctx = match damage_source {
        // "Target creature deals damage..." — the first resolved object target
        // is the damage source, not the ability source.
        Some(DamageSource::Target) => ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => DamageContext::from_source(state, *id),
                _ => None,
            })
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller)),
        // "That creature/permanent deals damage..." inside a triggered ability
        // binds the damage source to the triggering event object.
        Some(DamageSource::TriggeringSource) => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| DamageContext::from_source(state, id))
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller)),
        None => DamageContext::from_source(state, ability.source_id)
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller)),
        // CR 120.1: multi-source per-power damage is dispatched to
        // `resolve_each_target_power_damage` above (each source has its own
        // `DamageContext`), so this single-source `ctx` match is never reached.
        Some(DamageSource::EachTarget) => {
            unreachable!("EachTarget handled by resolve_each_target_power_damage")
        }
    };

    // CR 120.1 + CR 608.2c: Resolve effective damage targets.
    //
    // `SelfRef` is the printed-name anaphor (`~`) — always resolves to the
    // source object regardless of `ability.targets`. Short-circuit BEFORE the
    // `ability.targets.is_empty()` fallback so chained
    // `DealDamage { target: SelfRef }` sub-abilities don't inherit the
    // parent's targets via chain propagation in
    // `effects::mod.rs::resolve_ability_chain` (issue #323 class).
    //
    // Other implicit-target filters (`Controller`) keep the pre-existing
    // "fall back when targets are empty" semantic.
    let implicit;
    let effective_targets: &[TargetRef] = if matches!(target_filter, TargetFilter::SelfRef) {
        implicit = vec![TargetRef::Object(ability.source_id)];
        &implicit
    } else if let Some(target) = player_context_target(state, ability, target_filter) {
        implicit = vec![target];
        &implicit
    } else if !ability.targets.is_empty() {
        if matches!(damage_source, Some(DamageSource::Target)) && ability.targets.len() > 1 {
            &ability.targets[1..]
        } else {
            &ability.targets
        }
    } else {
        implicit = match target_filter {
            TargetFilter::Controller => vec![TargetRef::Player(ability.controller)],
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

/// CR 120.3 + CR 120.4b: Resume applying a damage event that already completed
/// replacement/prevention selection. Used by internal continuations only.
pub fn resolve_post_replacement(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (context, target, amount, is_combat) = match &ability.effect {
        Effect::ApplyPostReplacementDamage {
            context,
            target,
            amount,
            is_combat,
        } => (*context, target.clone(), *amount, *is_combat),
        _ => {
            return Err(EffectError::MissingParam(
                "ApplyPostReplacementDamage".to_string(),
            ));
        }
    };
    let ctx = DamageContext::from(context);
    let event = ProposedEvent::Damage {
        source_id: ctx.source_id,
        target,
        amount,
        is_combat,
        applied: HashSet::new(),
    };

    if matches!(
        apply_damage_after_replacement(state, &ctx, event, is_combat, events),
        DamageResult::NeedsChoice
    ) {
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 120.1 + CR 601.2c + CR 208.1 + CR 608.2: Resolve "each [of N target
/// creatures] deals damage equal to their power to <recipient>"
/// (`DamageSource::EachTarget`).
///
/// `ability.targets` is laid out `[source_0, …, source_{n-1}, recipient]` — the
/// variable-count source set (chosen via `multi_target` on the `TargetOnly`
/// picker or the prior sentence) followed by the single shared recipient slot.
/// Every source object deals damage equal to ITS OWN power: the amount is
/// re-resolved for each source against a one-element target slice so the
/// `Power{Target}` ref reads that member (CR 208.1: power is a modifiable
/// characteristic; CR 608.2: current value at resolution), then routed through
/// the full replacement/prevention pipeline against the recipient (CR 120.4b).
///
/// CR 120.1: each member is an independent damage source, so each builds its own
/// `DamageContext` (its own deathtouch/wither/infect/lifelink keywords apply).
///
/// SIMULTANEOUS BATCH (CR 120.4a + CR 120.6 + CR 120.10). All sources deal their
/// damage as one event set, mirroring the combat simultaneous-damage primitive
/// (`combat_damage.rs` `apply_combat_damage`, CR 510.2) but for a non-combat
/// spell effect, reusing its decomposed primitives directly:
///
/// - Every source's own-power amount is resolved up front, BEFORE any damage is
///   marked, so all amounts read the same pre-batch power (CR 608.2).
/// - **Phase A (Collect):** each source builds its own `DamageContext` and a
///   `ProposedEvent::Damage` through `pre_replacement_damage_gate` (0-damage,
///   protection, prohibition gates) without applying anything yet.
/// - **Phase B (Replace):** each proposed event runs `replacement::replace_event`
///   (the non-combat pipeline). A `NeedsChoice` here means a replacement on the
///   recipient needs a player ordering choice — the batch pauses and the
///   remaining sources (this paused source first) are stashed with PER-SOURCE
///   identity so each resumes through its OWN source id's keywords/LKI.
/// - **Phase C (Apply):** each surviving post-replacement event is applied via
///   `apply_damage_after_replacement` with that source's context. Because all
///   marks accumulate onto the one recipient before SBAs run (CR 704), combined
///   marked damage (CR 120.6 lethal) and combined excess (CR 120.10) are
///   correct, and deathtouch/wither/lifelink/infect apply per source.
fn resolve_each_target_power_damage(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount = match &ability.effect {
        Effect::DealDamage { amount, .. } => amount,
        _ => return Err(EffectError::MissingParam("DealDamage amount".to_string())),
    };

    // Partition the object targets into sources (all but the last) and the
    // shared recipient (the last object target). A non-object implicit recipient
    // (e.g. "deal damage to <player>") is resolved via `player_context_target`.
    let object_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();

    let (recipient, source_ids): (TargetRef, &[ObjectId]) =
        if let Some(player_recipient) = player_context_target(state, ability, target_filter) {
            // CR 120.3a: implicit/context player recipient (damage to a player
            // causes life loss) — every object target is a source.
            (player_recipient, object_targets.as_slice())
        } else if let Some((last, sources)) = object_targets.split_last() {
            (TargetRef::Object(*last), sources)
        } else {
            // No targets chosen ("up to N" with zero chosen) — nothing happens.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        };

    // CR 608.2 + CR 208.1: read every source's own power up front, before any
    // damage is marked, so the simultaneous batch reads the pre-batch power for
    // each member (one source dealing damage must not change another's power, and
    // marking damage on the recipient must not change source powers).
    let batch: Vec<(ObjectId, DamageContext, u32)> = source_ids
        .iter()
        .map(|&source_id| {
            let ctx = DamageContext::from_source(state, source_id)
                .unwrap_or_else(|| DamageContext::fallback(source_id, ability.controller));
            // Resolve the amount against a single-element slice so `Power{Target}`
            // binds to THIS source (CR 120.1: each is an independent source).
            let source_slice = [TargetRef::Object(source_id)];
            let amt = resolve_quantity_with_targets_slice(
                state,
                amount,
                ability.controller,
                source_id,
                &source_slice,
            )
            .max(0) as u32;
            (source_id, ctx, amt)
        })
        .collect();

    // --- Phase A: Collect proposed damage events (no application yet). ---
    // CR 120.8 / CR 702.16: per-source pre-replacement gate. Fully gated sources
    // (0 damage, protection, prohibition) contribute nothing; the gate already
    // emitted any required DamagePrevented.
    let mut entries: Vec<(usize, &DamageContext)> = Vec::with_capacity(batch.len());
    let mut proposed_events: Vec<ProposedEvent> = Vec::with_capacity(batch.len());
    for (idx, (_, ctx, amt)) in batch.iter().enumerate() {
        if let Some(proposed) =
            pre_replacement_damage_gate(state, ctx, &recipient, *amt, false, events)
        {
            entries.push((idx, ctx));
            proposed_events.push(proposed);
        }
    }

    // --- Phase B: Replacement (per-event, non-combat pipeline). ---
    // CR 120.4b + CR 614 + CR 615. Collected so Phase C applies all survivors
    // only after every replacement has resolved (true simultaneity from the
    // replacement perspective, matching the combat batch's two-pass structure).
    let mut survivors: Vec<(&DamageContext, ProposedEvent)> = Vec::with_capacity(entries.len());
    for (&(idx, ctx), proposed) in entries.iter().zip(proposed_events) {
        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => survivors.push((ctx, event)),
            ReplacementResult::Prevented => {
                // CR 615.5: A prevention rider (e.g. "for each 1 damage prevented
                // this way") resolves immediately afterward for non-combat damage.
                if state.post_replacement_continuation.is_some() {
                    let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
                        state, None, None, None, events,
                    );
                }
            }
            ReplacementResult::NeedsChoice(player) => {
                // CR 120.4b + CR 616.1e: A replacement on THIS source's damage to
                // the recipient needs a player ordering choice — pause the batch.
                // The paused source's own damage event lives in `pending_replacement`
                // and resumes through `handle_replacement_choice` (which rebuilds
                // its `DamageContext` from the event's own `source_id`). Apply the
                // survivors already chosen this pass, then stash only the sources
                // AFTER this one (`batch[idx + 1..]`) — each with PER-SOURCE
                // identity so it resumes through its OWN source id's keywords/LKI,
                // never flattened to a single source (the exact defect this batch
                // replaces). Including `idx` would double-deal the paused source.
                apply_batch_survivors_before_replacement_pause(state, &survivors, events);
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                let remaining = batch
                    .iter()
                    .skip(idx + 1)
                    .map(|(src, _, amt)| (*src, recipient.clone(), *amt));
                stash_remaining_each_source_damage(state, ability, remaining);
                return Ok(());
            }
        }
    }

    // --- Phase C: Apply survivors + record per-source identity (CR 120.3). ---
    if !apply_batch_survivors(state, ability, &survivors, events) {
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Apply each surviving post-replacement `Damage` event with its source's own
/// context. CR 120.6 + CR 120.10: marks accumulate onto the shared recipient so
/// combined lethal/excess is computed correctly once all sources have marked.
fn apply_batch_survivors(
    state: &mut GameState,
    ability: &ResolvedAbility,
    survivors: &[(&DamageContext, ProposedEvent)],
    events: &mut Vec<GameEvent>,
) -> bool {
    for (idx, (ctx, event)) in survivors.iter().enumerate() {
        // CR 120.3 + CR 120.4b: apply the post-replacement event WITHOUT
        // re-running the replacement pipeline. If damage application itself
        // pauses on a nested life/lifelink replacement choice, park the remaining
        // post-replacement survivors so the resume path does not consult CR
        // 614/615 damage replacements a second time.
        if matches!(
            apply_damage_after_replacement(state, ctx, event.clone(), false, events),
            DamageResult::NeedsChoice
        ) {
            stash_remaining_post_replacement_damage(state, ability, &survivors[idx + 1..]);
            return false;
        }
    }
    true
}

/// CR 120.4b + CR 616.1e: Apply already-selected survivors before surfacing a
/// Phase B replacement ordering choice. That choice is already parked in
/// `state.pending_replacement`, so this path must not allow a nested Phase C
/// life/lifelink prompt to overwrite it. The terminal Phase C path uses
/// `apply_batch_survivors` and preserves nested pauses with post-replacement
/// continuations.
fn apply_batch_survivors_before_replacement_pause(
    state: &mut GameState,
    survivors: &[(&DamageContext, ProposedEvent)],
    events: &mut Vec<GameEvent>,
) {
    for (ctx, event) in survivors {
        let _ = apply_damage_after_replacement(state, ctx, event.clone(), false, events);
    }
}

/// Deal uniform damage to every matching object and (optionally) every matching
/// player as a single simultaneous damage event from one source.
///
/// Reads amount, object filter, and optional player filter from
/// `Effect::DamageAll { amount, target, player_filter, damage_source }`.
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
    let (amount, target_filter, player_filter, damage_source): (
        &QuantityExpr,
        TargetFilter,
        Option<PlayerFilter>,
        Option<DamageSource>,
    ) = match &ability.effect {
        Effect::DamageAll {
            amount,
            target,
            player_filter,
            damage_source,
        } => (
            amount,
            target.clone(),
            player_filter.clone(),
            *damage_source,
        ),
        _ => return Err(EffectError::MissingParam("DamageAll amount".to_string())),
    };
    // CR 107.1b: Ability-context resolve so X-damage-to-all ("Deal X damage to each...")
    // reads the caster-chosen X. Recipient-relative quantities defer resolution
    // into the per-recipient loop below.
    let amount_uses_recipient = quantity_expr_uses_recipient(amount);
    let shared_num_dmg = if amount_uses_recipient {
        None
    } else {
        Some(resolve_quantity_with_targets(state, amount, ability).max(0) as u32)
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

    // CR 120.1 + CR 608.2c: Determine damage source. When `damage_source` is
    // `Some(Target)`, the chosen target object — not the ability's source
    // permanent — is the damage source for protection (CR 702.16), wither/infect
    // (CR 120.3b/d), and damage-source replacements (CR 614). Mirrors the
    // `DealDamage` resolver above so wrap_target_subject_damage works uniformly
    // for both single-recipient and batch damage shapes (Chandra's Ignition
    // class). CR 120.3h: Damage to a battle in `matching_objects` is routed
    // through `apply_damage_to_target` below, which removes defense counters
    // rather than marking damage.
    let ctx = match damage_source {
        Some(DamageSource::Target) => ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => DamageContext::from_source(state, *id),
                _ => None,
            })
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller)),
        Some(DamageSource::TriggeringSource) => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| DamageContext::from_source(state, id))
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller)),
        None => DamageContext::from_source(state, ability.source_id)
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller)),
        // CR 120.1: `EachTarget` (multi-source per-power) is only produced by the
        // parser for the single-recipient `DealDamage` shape; the batch
        // `DamageAll` form is never constructed with it.
        Some(DamageSource::EachTarget) => {
            unreachable!("EachTarget is only produced for DealDamage, not DamageAll")
        }
    };

    // CR 120.3 + CR 609.7: Assemble the full simultaneous recipient list as a
    // uniform stream of `TargetRef`s. Objects first, then players — CR 120.3
    // does not specify an order within a simultaneous batch, but consistency
    // matters for replacement-drain resumption ordering.
    let mut recipients: Vec<TargetRef> =
        Vec::with_capacity(matching_objects.len() + matching_players.len());
    recipients.extend(matching_objects.iter().map(|&id| TargetRef::Object(id)));
    recipients.extend(matching_players.iter().map(|&pid| TargetRef::Player(pid)));

    let recipient_amounts: Vec<(TargetRef, u32)> = recipients
        .into_iter()
        .map(|target| {
            let dmg = match (shared_num_dmg, &target) {
                (Some(dmg), _) => dmg,
                (None, TargetRef::Object(id)) => {
                    resolve_quantity_with_targets_and_recipient(state, amount, ability, *id).max(0)
                        as u32
                }
                (None, TargetRef::Player(_)) => {
                    resolve_quantity_with_targets(state, amount, ability).max(0) as u32
                }
            };
            (target, dmg)
        })
        .collect();

    for (i, (target, num_dmg)) in recipient_amounts.iter().enumerate() {
        match apply_damage_to_target(state, &ctx, target.clone(), *num_dmg, false, events)? {
            DamageResult::Applied(_) => {}
            DamageResult::NeedsChoice => {
                // CR 120.3 + CR 616.1e: Remaining batch recipients must resume after
                // the replacement choice resolves — chain them as DealDamage
                // continuations keyed to the same damage-source id.
                let remaining = recipient_amounts[i + 1..].iter().cloned();
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
                    PlayerFilter::DefendingPlayer => {
                        crate::game::targeting::resolve_event_context_target_for_event_or_state(
                            state,
                            &TargetFilter::DefendingPlayer,
                            source_id,
                            state.current_trigger_event.as_ref(),
                        )
                        .is_some_and(
                            |target| matches!(target, TargetRef::Player(pid) if pid == p.id),
                        )
                    }
                    PlayerFilter::OpponentLostLife => {
                        p.id != source_controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != source_controller && p.life_gained_this_turn > 0
                    }
                    // CR 104.5 / CR 800.4: Players who lost have left the game;
                    // this filter is quantity-only and has no live damage recipient.
                    PlayerFilter::HasLostTheGame => false,
                    // CR 506.2 + CR 508.6: Count-only filter (Suppressor Skyguard);
                    // it has no live damage-recipient meaning.
                    PlayerFilter::OpponentOfTriggeringPlayerNotAttacked => false,
                    // CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i: Each opponent
                    // who was dealt combat damage this turn, optionally
                    // restricted to a matching source.
                    PlayerFilter::OpponentDealtCombatDamage { ref source } => {
                        crate::game::quantity::opponent_dealt_combat_damage_matches(
                            state,
                            p.id,
                            source_controller,
                            source,
                            source_id,
                        )
                    }
                    // CR 508.6: opponent the subject attacked within scope.
                    PlayerFilter::OpponentAttacked { subject, scope } => {
                        p.id != source_controller
                            && state.opponent_attacked(
                                subject,
                                scope,
                                source_controller,
                                source_id,
                                p.id,
                            )
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
                    PlayerFilter::PerformedActionThisWay { relation, action } => {
                        crate::game::players::matches_relation(
                            state,
                            p.id,
                            source_controller,
                            relation,
                        ) && crate::game::players::performed_action_this_way(state, p.id, action)
                    }
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
                        if !crate::game::players::is_opponent(state, source_controller, p.id) {
                            return false;
                        }
                        let triggering = state.current_trigger_event.as_ref().and_then(|e| {
                            crate::game::targeting::extract_player_from_event(e, state)
                        });
                        triggering != Some(p.id)
                    }
                    // CR 608.2c + CR 701.38: Match each player who cast a vote
                    // for the recorded choice index. Mirrors the
                    // `ZoneChangedThisWay` arm — consults the transient
                    // `last_vote_ballots` ledger.
                    PlayerFilter::VotedFor { choice_index } => state
                        .last_vote_ballots
                        .iter()
                        .any(|(voter, idx)| *voter == p.id && *idx == choice_index),
                    // CR 109.4 + CR 108.3: the parent-object-target anchors and
                    // the resolution-scoped chosen-player anchor have no meaning
                    // for a damage-each-player effect (no parent object target /
                    // chosen player is in scope here); never matches.
                    PlayerFilter::ParentObjectTargetController
                    | PlayerFilter::ParentObjectTargetOwner
                    | PlayerFilter::ChosenPlayer { .. } => false,
                    // CR 109.4 + CR 109.5: "each [player class] who controls
                    // [comparator] [count] [filter]" — candidate satisfies both
                    // `relation` and the controlled-permanent count comparison.
                    PlayerFilter::ControlsCount {
                        ref relation,
                        ref filter,
                        ref comparator,
                        ref count,
                    } => {
                        let threshold = crate::game::quantity::resolve_quantity(
                            state,
                            count,
                            source_controller,
                            source_id,
                        );
                        crate::game::players::matches_relation(
                            state,
                            p.id,
                            source_controller,
                            *relation,
                        ) && crate::game::effects::player_control_count_compares(
                            state,
                            p.id,
                            filter,
                            *comparator,
                            threshold,
                            source_id,
                        )
                    }
                    // CR 402.1 / 119.1 / 122.1f / 404.1: "each [player class]
                    // whose [scalar attr] [comparator] [value]" — candidate
                    // satisfies both `relation` and the per-candidate scalar
                    // comparison. `attr` is read directly off `p`; `value` is
                    // the controller-relative threshold, resolved once.
                    PlayerFilter::PlayerAttribute {
                        ref relation,
                        ref attr,
                        ref comparator,
                        ref value,
                    } => {
                        let threshold = crate::game::quantity::resolve_quantity(
                            state,
                            value,
                            source_controller,
                            source_id,
                        );
                        crate::game::players::matches_relation(
                            state,
                            p.id,
                            source_controller,
                            *relation,
                        ) && crate::game::effects::candidate_player_scalar_with_state(
                            state,
                            p,
                            source_controller,
                            attr,
                        )
                        .is_some_and(|lhs| comparator.evaluate(lhs, threshold))
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
        } => (amount, player_filter.clone()),
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
                    PlayerFilter::DefendingPlayer => {
                        crate::game::targeting::resolve_event_context_target_for_event_or_state(
                            state,
                            &TargetFilter::DefendingPlayer,
                            ability.source_id,
                            state.current_trigger_event.as_ref(),
                        )
                        .is_some_and(
                            |target| matches!(target, TargetRef::Player(pid) if pid == p.id),
                        )
                    }
                    PlayerFilter::OpponentLostLife => {
                        p.id != ability.controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != ability.controller && p.life_gained_this_turn > 0
                    }
                    // CR 104.5 / CR 800.4: Players who lost have left the game;
                    // this filter is quantity-only and has no live damage recipient.
                    PlayerFilter::HasLostTheGame => false,
                    // CR 506.2 + CR 508.6: Count-only filter (Suppressor Skyguard);
                    // it has no live damage-recipient meaning.
                    PlayerFilter::OpponentOfTriggeringPlayerNotAttacked => false,
                    // CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i: Each opponent
                    // who was dealt combat damage this turn, optionally
                    // restricted to a matching source.
                    PlayerFilter::OpponentDealtCombatDamage { source } => {
                        crate::game::quantity::opponent_dealt_combat_damage_matches(
                            state,
                            p.id,
                            ability.controller,
                            source,
                            ability.source_id,
                        )
                    }
                    // CR 508.6: opponent the subject attacked within scope.
                    PlayerFilter::OpponentAttacked { subject, scope } => {
                        p.id != ability.controller
                            && state.opponent_attacked(
                                *subject,
                                *scope,
                                ability.controller,
                                ability.source_id,
                                p.id,
                            )
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
                    PlayerFilter::PerformedActionThisWay { relation, action } => {
                        crate::game::players::matches_relation(
                            state,
                            p.id,
                            ability.controller,
                            *relation,
                        ) && crate::game::players::performed_action_this_way(state, p.id, *action)
                    }
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
                        if !crate::game::players::is_opponent(state, ability.controller, p.id) {
                            return false;
                        }
                        let triggering = state.current_trigger_event.as_ref().and_then(|e| {
                            crate::game::targeting::extract_player_from_event(e, state)
                        });
                        triggering != Some(p.id)
                    }
                    // CR 608.2c + CR 701.38: Match each player who cast a vote
                    // for the recorded choice index in the most recent vote.
                    PlayerFilter::VotedFor { choice_index } => state
                        .last_vote_ballots
                        .iter()
                        .any(|(voter, idx)| *voter == p.id && *idx == *choice_index),
                    // CR 109.4 + CR 108.3: the parent-object-target anchors and
                    // the resolution-scoped chosen-player anchor have no meaning
                    // for a damage-each-player effect (no parent object target /
                    // chosen player is in scope here); never matches.
                    PlayerFilter::ParentObjectTargetController
                    | PlayerFilter::ParentObjectTargetOwner
                    | PlayerFilter::ChosenPlayer { .. } => false,
                    // CR 109.4 + CR 109.5: "each [player class] who controls
                    // [comparator] [count] [filter]" — candidate satisfies both
                    // `relation` and the controlled-permanent count comparison.
                    PlayerFilter::ControlsCount {
                        relation,
                        filter,
                        comparator,
                        count,
                    } => {
                        let threshold = crate::game::quantity::resolve_quantity(
                            state,
                            count,
                            ability.controller,
                            ability.source_id,
                        );
                        crate::game::players::matches_relation(
                            state,
                            p.id,
                            ability.controller,
                            *relation,
                        ) && crate::game::effects::player_control_count_compares(
                            state,
                            p.id,
                            filter,
                            *comparator,
                            threshold,
                            ability.source_id,
                        )
                    }
                    // CR 402.1 / 119.1 / 122.1f / 404.1: "each [player class]
                    // whose [scalar attr] [comparator] [value]" — candidate
                    // satisfies both `relation` and the per-candidate scalar
                    // comparison. `attr` is read directly off `p`; `value` is
                    // the controller-relative threshold, resolved once.
                    PlayerFilter::PlayerAttribute {
                        relation,
                        attr,
                        comparator,
                        value,
                    } => {
                        let threshold = crate::game::quantity::resolve_quantity(
                            state,
                            value,
                            ability.controller,
                            ability.source_id,
                        );
                        crate::game::players::matches_relation(
                            state,
                            p.id,
                            ability.controller,
                            *relation,
                        ) && crate::game::effects::candidate_player_scalar_with_state(
                            state,
                            p,
                            ability.controller,
                            attr,
                        )
                        .is_some_and(|lhs| comparator.evaluate(lhs, threshold))
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

/// CR 120.1 + CR 120.3: "Up to two target creatures you control each deal damage
/// equal to their power to target creature" (Band Together, Allies at Last,
/// Friendly Rivalry, Graceful Takedown). Combo Attack's "your team controls"
/// (Two-Headed Giant team scope, CR 810) is out of scope and fails closed.
///
/// Each chosen source creature deals damage equal to ITS OWN power to the single
/// recipient, with that source creature as the damage source (CR 120.1). The
/// per-source `DamageContext` is built from each source object so its keywords
/// (deathtouch, lifelink, …) and identity travel with its damage. Power is read
/// from the live object, falling back to last known information (CR 113.7a) when
/// a source has left the battlefield between announcement and resolution.
///
/// Target layout: the source slots are surfaced first (0..=2) and the recipient
/// slot last by `ability_utils::collect_target_slots`, so `ability.targets` is
/// `[source.., recipient]`. The recipient is the final object target; every
/// earlier object target is a source.
pub fn resolve_each_deals_equal_to_power(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(ability.effect, Effect::EachDealsDamageEqualToPower { .. }) {
        return Err(EffectError::MissingParam(
            "EachDealsDamageEqualToPower".to_string(),
        ));
    }

    // CR 115.1: collect the chosen object targets in declaration order.
    let object_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();

    // The recipient is the last object target; the sources are the rest. With
    // zero sources chosen ("up to two" → 0) there is exactly one object target
    // (the recipient) and no damage is dealt. Fewer than one object target means
    // the recipient became illegal and the spell did nothing (CR 608.2b).
    let Some((&recipient_id, source_ids)) = object_targets.split_last() else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };

    for (i, &source_id) in source_ids.iter().enumerate() {
        // CR 113.7a + CR 208.3: power from the live object, falling back to LKI
        // when the source left the battlefield after targets were chosen.
        let power = object_power_with_lki(state, source_id);
        if power <= 0 {
            continue;
        }
        // CR 120.1: each source creature is the source of its own damage.
        let ctx = DamageContext::from_source(state, source_id)
            .unwrap_or_else(|| DamageContext::fallback(source_id, ability.controller));
        match apply_damage_to_target(
            state,
            &ctx,
            TargetRef::Object(recipient_id),
            power as u32,
            false,
            events,
        )? {
            DamageResult::Applied(_) => {}
            DamageResult::NeedsChoice => {
                // CR 120.3 + CR 616.1e: This source's damage paused for a
                // replacement choice. Each remaining source deals its own power
                // from its own object, so stash one continuation node per
                // remaining source (each carrying its own `source_id`), then the
                // parent sub_ability tail.
                stash_remaining_each_deals_chain(
                    state,
                    ability,
                    &source_ids[i + 1..],
                    recipient_id,
                );
                mark_pending_continuation_parent(state, EffectKind::EachDealsDamageEqualToPower);
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

/// CR 208.3 + CR 113.7a: A creature's power from current state, falling back to
/// Last Known Information when the object has left the battlefield. Mirrors the
/// `ObjectScope::Source` arm of `quantity::resolve_object_pt`.
fn object_power_with_lki(state: &GameState, id: ObjectId) -> i32 {
    state
        .objects
        .get(&id)
        .and_then(|obj| obj.power)
        .or_else(|| state.lki_cache.get(&id).and_then(|lki| lki.power))
        .unwrap_or(0)
}

/// CR 120.1 + CR 616.1e: Build one continuation node per remaining source after a
/// replacement choice paused mid-resolution. Each node deals that source's power
/// to the recipient with that source creature as the damage source (its own
/// `source_id`). The parent's sub_ability tail is appended last so any downstream
/// chain resumes once the choice resolves.
fn stash_remaining_each_deals_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining_sources: &[ObjectId],
    recipient_id: ObjectId,
) {
    let mut head: Option<ResolvedAbility> = None;
    for &source_id in remaining_sources {
        let power = object_power_with_lki(state, source_id);
        if power <= 0 {
            continue;
        }
        let node = build_remaining_damage_node(
            source_id,
            ability.controller,
            TargetRef::Object(recipient_id),
            power as u32,
        );
        match head.as_mut() {
            Some(h) => append_to_sub_chain(h, node),
            None => head = Some(node),
        }
    }
    match head {
        Some(mut h) => {
            if let Some(sub) = ability.sub_ability.as_ref() {
                append_to_sub_chain(&mut h, sub.as_ref().clone());
            }
            append_to_pending_continuation(state, Some(Box::new(h)));
        }
        None => {
            if let Some(sub) = ability.sub_ability.as_ref() {
                append_to_pending_continuation(state, Some(Box::new(sub.as_ref().clone())));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ChosenAttribute, ContinuousModification, ControllerRef, Duration, FilterProp, ObjectScope,
        QuantityExpr, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{WaitingFor, ZoneChangeRecord};
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

    /// CR 120.1 + CR 120.3: Band Together — casting "Up to two target creatures
    /// you control each deal damage equal to their power to another target
    /// creature" with a 3/2 and a 2/2 source each deals its own power (3 and 2)
    /// to a single recipient, which therefore takes 5 marked damage. Each source
    /// is its own damage source (CR 120.1); the recipient is the last target.
    #[test]
    fn band_together_two_sources_deal_combined_power_to_recipient() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::phase::Phase;

        const BAND_TOGETHER: &str = "Up to two target creatures you control each deal damage equal to their power to another target creature.";

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let src_a = scenario.add_creature(P0, "Striker A", 3, 2).id();
        let src_b = scenario.add_creature(P0, "Striker B", 2, 2).id();
        // CR 120.3e: a 0/9 recipient survives 5 marked damage, so the marked
        // total is directly observable post-resolution.
        let recipient = scenario.add_creature(P1, "Big Wall", 0, 9).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Band Together", false, BAND_TOGETHER)
            .id();
        // Fund the pool generously; the source/recipient choice is the only
        // interaction under test.
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 6],
        );
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P0;
            state.priority_player = P0;
        }

        // CR 601.2c: source slots first (src_a, src_b), recipient slot last.
        let outcome = runner
            .cast(spell)
            .target_objects(&[src_a, src_b, recipient])
            .resolve();

        // CR 120.1 + CR 120.3e: 3 (Striker A) + 2 (Striker B) = 5 marked on the
        // recipient.
        assert_eq!(
            outcome.damage_marked(recipient),
            5,
            "recipient should take each source's power (3 + 2) = 5"
        );
        // CR 120.1: the sources are not damaged (one-directional, unlike fight).
        assert_eq!(outcome.damage_marked(src_a), 0);
        assert_eq!(outcome.damage_marked(src_b), 0);
    }

    /// CR 120.3f + CR 120.4b + CR 616.1e: In the `EachTarget` simultaneous batch,
    /// Phase C can still pause after damage is dealt if lifelink life gain needs a
    /// replacement choice. Remaining already-replaced damage survivors must be
    /// parked as post-replacement continuations, not fresh `DealDamage` nodes that
    /// would re-run damage replacement/prevention selection.
    #[test]
    fn each_target_phase_c_lifelink_pause_stashes_post_replacement_survivors() {
        use crate::types::ability::{ReplacementDefinition, ReplacementMode};
        use crate::types::keywords::Keyword;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let source_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lifelink Source A".to_string(),
            Zone::Battlefield,
        );
        let source_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Lifelink Source B".to_string(),
            Zone::Battlefield,
        );
        for (source, power) in [(source_a, 2), (source_b, 3)] {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(power);
            obj.base_power = Some(power);
            obj.toughness = Some(2);
            obj.base_toughness = Some(2);
            obj.keywords.push(Keyword::Lifelink);
        }
        let recipient = create_object(
            &mut state,
            CardId(12),
            PlayerId(1),
            "Large Recipient".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&recipient).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(0);
            obj.base_power = Some(0);
            obj.toughness = Some(9);
            obj.base_toughness = Some(9);
        }

        let replacement_host = create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Life Replacement".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement_host)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::GainLife)
                    .mode(ReplacementMode::Optional { decline: None })
                    .description("Life replacement".to_string()),
            );

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Target,
                    },
                },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: Some(DamageSource::EachTarget),
            },
            vec![
                TargetRef::Object(source_a),
                TargetRef::Object(source_b),
                TargetRef::Object(recipient),
            ],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(
            state.objects[&recipient].damage_marked, 2,
            "first source's post-replacement damage applies before lifelink pauses"
        );
        let cont = state
            .pending_continuation
            .as_ref()
            .expect("remaining post-replacement survivor must be stashed");
        match &cont.chain.effect {
            Effect::ApplyPostReplacementDamage {
                context,
                target,
                amount,
                is_combat,
            } => {
                assert_eq!(context.source_id, source_b);
                assert_eq!(context.controller, PlayerId(0));
                assert!(context.has_lifelink);
                assert_eq!(*target, TargetRef::Object(recipient));
                assert_eq!(*amount, 3);
                assert!(!*is_combat);
            }
            other => panic!("expected post-replacement damage continuation, got {other:?}"),
        }
    }

    /// CR 122.1c: damage to a permanent with a shield counter is prevented and
    /// one shield counter is removed (non-combat / single-source path).
    #[test]
    fn shield_counter_prevents_noncombat_damage_and_is_consumed() {
        use crate::types::counter::CounterType;
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Shielded Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .counters
            .insert(CounterType::Shield, 1);

        let ability = make_ability(3, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&obj_id].damage_marked, 0,
            "shield counter prevents the damage"
        );
        assert_eq!(
            state.objects[&obj_id].counters.get(&CounterType::Shield),
            None,
            "the shield counter is consumed"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
            "a DamagePrevented event is emitted"
        );
    }

    /// CR 702.11b + CR 613.1f + CR 120.3: DISCRIMINATING runtime test for
    /// "has hexproof if it hasn't dealt damage yet" (Palladia-Mors, the Ruiner;
    /// Karakyk Guardian). Proves the gate is read AT THE TARGETING CHECK:
    ///   1. Before dealing damage, an opponent's bolt CANNOT target the creature
    ///      (hexproof active → not in `target_slots[0].legal_targets`).
    ///   2. After the creature deals combat damage, the same bolt CAN target it
    ///      (hexproof gone) — this passes only because the `mark_full()` in the
    ///      damage chokepoint forces `flush_layers` to recompute the materialized
    ///      keyword set `has_hexproof` reads. Reverting that `mark_full()` leaves
    ///      `layers_dirty == Clean`, the keyword set stale, and this step FAILS.
    ///   3. A noncombat-damage source separately sets the same sticky flag.
    ///   4. After the creature leaves and re-enters the battlefield (flicker), the
    ///      flag is cleared and hexproof is restored (illegal target again).
    #[test]
    fn hexproof_if_hasnt_dealt_damage_drops_at_targeting_after_damage_restored_on_flicker() {
        use crate::game::keywords::has_hexproof;
        use crate::game::layers::flush_layers;
        use crate::game::scenario::GameScenario;
        use crate::game::zones::move_to_zone;
        use crate::types::actions::GameAction;
        use crate::types::game_state::CastPaymentMode;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);
        const P1: PlayerId = PlayerId(1);

        // P1 controls the conditional-hexproof creature; P0 is the opponent.
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let guardian = scenario
            .add_creature_from_oracle(
                P1,
                "Karakyk Guardian",
                3,
                3,
                "Flying, vigilance, trample\nThis creature has hexproof if it hasn't dealt damage yet.",
            )
            .id();
        // A vanilla P1 creature for the guardian to deal noncombat-style damage to.
        let victim = scenario.add_creature(P1, "Bear", 2, 2).id();
        let bolt1 = scenario.add_bolt_to_hand(P0);
        let bolt2 = scenario.add_bolt_to_hand(P0);
        let mut runner = scenario.build();

        // Layer 6 (CR 613.1f): the conditional grant is active while the gate
        // (Not(SourceHasDealtDamage)) holds — i.e. before any damage is dealt.
        flush_layers(runner.state_mut());
        assert!(
            has_hexproof(&runner.state().objects[&guardian]),
            "guardian must have hexproof before it has dealt damage"
        );

        // STEP 1 — at the targeting check, an opponent's bolt cannot target it.
        let bolt1_card = runner.state().objects[&bolt1].card_id;
        let before = runner
            .act(GameAction::CastSpell {
                object_id: bolt1,
                card_id: bolt1_card,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast bolt 1 (players are still legal targets)");
        if let WaitingFor::TargetSelection { target_slots, .. } = &before.waiting_for {
            assert!(
                !target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(guardian)),
                "hexproof creature must NOT be a legal target before dealing damage"
            );
        } else {
            panic!("expected TargetSelection, got {:?}", before.waiting_for);
        }
        // Resolve bolt 1 harmlessly at a player so it leaves the stack.
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("retarget bolt 1 at a player");

        // STEP 2 — the guardian deals COMBAT damage. Drive the production
        // chokepoint (`apply_damage_after_replacement`) so the BLOCKER-1
        // `mark_full()` path executes.
        {
            let state = runner.state_mut();
            let ctx = DamageContext::from_source(state, guardian).expect("guardian source ctx");
            let event = ProposedEvent::Damage {
                source_id: guardian,
                target: TargetRef::Object(victim),
                amount: 1,
                is_combat: true,
                applied: HashSet::new(),
            };
            let mut events = Vec::new();
            apply_damage_after_replacement(state, &ctx, event, true, &mut events);
            assert!(
                state.objects_that_dealt_damage.contains(&guardian),
                "sticky flag set after combat damage actually dealt"
            );
            // BLOCKER 1: the chokepoint marked layers fully dirty on first insert.
            assert!(
                state.layers_dirty.is_dirty(),
                "deal_damage must mark layers dirty so the keyword set recomputes"
            );
        }

        // The next action flushes layers; hexproof must now be gone AT the
        // targeting check (the bolt can now target the guardian).
        let bolt2_card = runner.state().objects[&bolt2].card_id;
        let after = runner
            .act(GameAction::CastSpell {
                object_id: bolt2,
                card_id: bolt2_card,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast bolt 2");
        if let WaitingFor::TargetSelection { target_slots, .. } = &after.waiting_for {
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(guardian)),
                "hexproof must be GONE at the targeting check after the creature dealt damage"
            );
        } else {
            panic!("expected TargetSelection, got {:?}", after.waiting_for);
        }
        assert!(
            !has_hexproof(&runner.state().objects[&guardian]),
            "materialized keywords must no longer include hexproof after damage"
        );
        // Clear the in-flight bolt 2 selection by retargeting at a player.
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("retarget bolt 2 at a player");

        // STEP 3 — a NONCOMBAT-damage source also sets the sticky flag. Use a
        // fresh creature so combat vs. noncombat is isolated.
        {
            let state = runner.state_mut();
            let id = create_object(
                state,
                CardId(9001),
                P1,
                "Noncombat Pinger".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            let ctx = DamageContext::from_source(state, id).expect("pinger source ctx");
            let event = ProposedEvent::Damage {
                source_id: id,
                target: TargetRef::Player(P0),
                amount: 1,
                is_combat: false,
                applied: HashSet::new(),
            };
            let mut events = Vec::new();
            apply_damage_after_replacement(state, &ctx, event, false, &mut events);
            assert!(
                state.objects_that_dealt_damage.contains(&id),
                "noncombat damage also sets the sticky flag"
            );
        }

        // STEP 4 — flicker the guardian (battlefield → exile → battlefield). The
        // sticky flag is cleared on the battlefield exit, restoring hexproof.
        {
            let state = runner.state_mut();
            let mut events = Vec::new();
            move_to_zone(state, guardian, Zone::Exile, &mut events);
            assert!(
                !state.objects_that_dealt_damage.contains(&guardian),
                "flag cleared when the guardian leaves the battlefield"
            );
            move_to_zone(state, guardian, Zone::Battlefield, &mut events);
            flush_layers(state);
        }
        assert!(
            has_hexproof(&runner.state().objects[&guardian]),
            "hexproof restored after flicker (clean slate, no damage dealt yet)"
        );
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
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let ability = make_ability(3, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].damage_marked, 3);
    }

    #[test]
    fn damage_record_snapshots_object_target_controller() {
        let mut state = GameState::new_two_player(42);
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ctx = DamageContext::fallback(ObjectId(100), PlayerId(0));
        let event = ProposedEvent::Damage {
            source_id: ctx.source_id,
            target: TargetRef::Object(target),
            amount: 2,
            is_combat: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();

        apply_damage_after_replacement(&mut state, &ctx, event, false, &mut events);

        assert_eq!(state.damage_dealt_this_turn.len(), 1);
        assert_eq!(
            state.damage_dealt_this_turn[0].target_controller,
            PlayerId(1)
        );
    }

    #[test]
    fn damage_record_snapshots_player_target_as_its_own_controller() {
        let mut state = GameState::new_two_player(42);
        let ctx = DamageContext::fallback(ObjectId(100), PlayerId(0));
        let event = ProposedEvent::Damage {
            source_id: ctx.source_id,
            target: TargetRef::Player(PlayerId(1)),
            amount: 2,
            is_combat: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();

        apply_damage_after_replacement(&mut state, &ctx, event, false, &mut events);

        assert_eq!(state.damage_dealt_this_turn.len(), 1);
        assert_eq!(
            state.damage_dealt_this_turn[0].target_controller,
            PlayerId(1)
        );
    }

    #[test]
    fn target_damage_source_damages_recipient_targets_only() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Source Creature".to_string(),
            Zone::Battlefield,
        );
        let recipient = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Recipient Creature".to_string(),
            Zone::Battlefield,
        );
        for id in [source, recipient] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.base_power = Some(3);
        }
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Target,
                    },
                },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: Some(DamageSource::Target),
            },
            vec![TargetRef::Object(source), TargetRef::Object(recipient)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&source].damage_marked, 0);
        assert_eq!(state.objects[&recipient].damage_marked, 3);
    }

    /// #699 (one-sided fight — full Ambuscade-class shape). The boosted creature
    /// is `targets[0]` (power 5), the opponent's creature is `targets[1]`
    /// (power 2). With `damage_source: Some(Target)` + `amount: Power{Target}`
    /// the boosted creature deals damage equal to ITS OWN power (5, read from
    /// targets[0], NOT the recipient's 2) to the recipient. Asserts: recipient
    /// (targets[1]) takes 5, boosted creature (targets[0]) takes 0. Proves the
    /// amount reads the boosted creature (Target == targets[0]) and the recipient
    /// is targets[1] — the exact slot ordering the #699 parser fix relies on.
    /// CR 120.1: the boosted creature is the damage source.
    #[test]
    fn one_sided_fight_amount_reads_boosted_creature_recipient_takes_damage() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Boosted Creature".to_string(),
            Zone::Battlefield,
        );
        let recipient = create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source).unwrap();
            s.card_types.core_types.push(CoreType::Creature);
            s.power = Some(5);
            s.base_power = Some(5);
        }
        {
            let r = state.objects.get_mut(&recipient).unwrap();
            r.card_types.core_types.push(CoreType::Creature);
            r.power = Some(2);
            r.base_power = Some(2);
        }
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Target,
                    },
                },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: Some(DamageSource::Target),
            },
            vec![TargetRef::Object(source), TargetRef::Object(recipient)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Boosted creature deals its power (5), not the recipient's (2);
        // recipient (targets[1]) receives it, boosted (targets[0]) is unscathed.
        assert_eq!(state.objects[&source].damage_marked, 0);
        assert_eq!(state.objects[&recipient].damage_marked, 5);
    }

    /// #699 (one-sided fight — `Power{Anaphoric}` amount). Same shape as the
    /// `Power{Target}` test above, but the amount is `Power{Anaphoric}` with
    /// `effect_context_object = None` (the real Ambuscade-class AST after the
    /// Option-b fix: the parser leaves "its power" as Anaphoric). With no
    /// effect-context / event-source / cost referent to seed it, the anaphor
    /// MUST resolve through the runtime one-sided-fight fallback to `targets[0]`
    /// (the boosted creature, power 5) — NOT to 0. Asserts the boosted creature
    /// (targets[0]) takes 0 and the opponent (targets[1]) takes 5. This is the
    /// assertion that flips (5 → 0 on the recipient) if the new
    /// `resolve_object_pt` Anaphoric fallback is reverted.
    /// CR 608.2c + CR 120.1: the boosted creature is the anaphoric "It" and the
    /// damage source.
    #[test]
    fn one_sided_fight_anaphoric_amount_reads_boosted_creature() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Boosted Creature".to_string(),
            Zone::Battlefield,
        );
        let recipient = create_object(
            &mut state,
            CardId(31),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source).unwrap();
            s.card_types.core_types.push(CoreType::Creature);
            s.power = Some(5);
            s.base_power = Some(5);
        }
        {
            let r = state.objects.get_mut(&recipient).unwrap();
            r.card_types.core_types.push(CoreType::Creature);
            r.power = Some(2);
            r.base_power = Some(2);
        }
        // ResolvedAbility::new leaves effect_context_object = None and
        // cost_paid_object = None, so only the new targets[0] fallback can seed
        // the anaphoric power.
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Anaphoric,
                    },
                },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: Some(DamageSource::Target),
            },
            vec![TargetRef::Object(source), TargetRef::Object(recipient)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&source].damage_marked, 0);
        assert_eq!(state.objects[&recipient].damage_marked, 5);
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
                            counter_type: CounterType::Minus1Minus1,
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

    /// CR 615.1a + CR 615.5 + CR 122.1 + CR 608.2h: Protean Hydra class — "If
    /// damage would be dealt to ~, prevent that damage and remove that many
    /// +1/+1 counters from it." The prevention shield absorbs the damage and
    /// the rider removes one +1/+1 counter per damage prevented, resolving
    /// `EventContextAmount` on `Effect::RemoveCounter.count` exactly as the
    /// Phyrexian Hydra cohort does on `Effect::PutCounter.count`.
    #[test]
    fn protean_hydra_prevention_removes_that_many_plus_counters() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, PreventionAmount, QuantityExpr, QuantityRef,
            ReplacementDefinition,
        };
        use crate::types::counter::CounterType;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let hydra = create_object(
            &mut state,
            CardId(42),
            PlayerId(1),
            "Protean Hydra".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&hydra).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(5);
            obj.toughness = Some(5);
            // Enters with five +1/+1 counters.
            obj.counters.insert(CounterType::Plus1Plus1, 5);
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .valid_card(TargetFilter::SelfRef)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::RemoveCounter {
                            counter_type: Some(CounterType::Plus1Plus1),
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::EventContextAmount,
                            },
                            target: TargetFilter::SelfRef,
                        },
                    ))
                    .description("Protean Hydra prevention shield".to_string()),
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
            "expected DamagePrevented event with amount 3, got {events:?}"
        );
        assert_eq!(
            hydra_obj.damage_marked, 0,
            "prevention must absorb the damage (no marked damage)"
        );
        assert_eq!(
            hydra_obj
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
            "5 starting counters minus 3 prevented damage = 2 remaining, events: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::DamageDealt { .. })),
            "must not emit DamageDealt for fully prevented damage"
        );
    }

    /// CR 615.5: Crumbling Sanctuary-class prevention follow-ups resolve "that
    /// player" from the prevented damage event's target and "that many" from
    /// the prevented damage amount.
    #[test]
    fn damage_to_player_prevention_exiles_from_that_players_library() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, DamageTargetFilter, DamageTargetPlayerScope,
            PreventionAmount, QuantityExpr, QuantityRef, ReplacementDefinition,
        };
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let sanctuary = create_object(
            &mut state,
            CardId(42),
            PlayerId(0),
            "Crumbling Sanctuary".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&sanctuary)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .damage_target_filter(DamageTargetFilter::Player {
                        player: DamageTargetPlayerScope::Any,
                    })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ExileTop {
                            player: TargetFilter::PostReplacementDamageTarget,
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::EventContextAmount,
                            },
                            face_down: false,
                        },
                    ))
                    .description("Crumbling Sanctuary prevention shield".to_string()),
            );

        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "First card".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Second card".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Third card".to_string(),
            Zone::Library,
        );

        let life_before = state.players[1].life;
        let ability = make_ability(2, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, life_before);
        assert_eq!(
            state.objects.get(&first).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&second).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&third).map(|obj| obj.zone),
            Some(Zone::Library)
        );
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::DamagePrevented {
                    target: TargetRef::Player(PlayerId(1)),
                    amount: 2,
                    ..
                }
            )
        }));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::DamageDealt { .. })));
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
                            counter_type: CounterType::Minus1Minus1,
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
                damage_source: None,
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
    fn damage_all_resolves_recipient_relative_amount_per_creature() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Baki's Curse".to_string(),
            Zone::Battlefield,
        );
        let bear1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear 1".to_string(),
            Zone::Battlefield,
        );
        let bear2 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Bear 2".to_string(),
            Zone::Battlefield,
        );
        let bear3 = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Bear 3".to_string(),
            Zone::Battlefield,
        );
        for id in [bear1, bear2, bear3] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        for (card_id, host) in [(5, bear1), (6, bear1), (7, bear2)] {
            let aura = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Aura {card_id}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(host.into());
        }

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Multiply {
                    factor: 2,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Subtype("Aura".to_string())],
                                controller: None,
                                properties: vec![FilterProp::AttachedToRecipient],
                            }),
                        },
                    }),
                },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: None,
                damage_source: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear1].damage_marked, 4);
        assert_eq!(state.objects[&bear2].damage_marked, 2);
        assert_eq!(state.objects[&bear3].damage_marked, 0);
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
        assert!(
            !state.objects[&battle_id]
                .counters
                .contains_key(&CounterType::Defense),
            "zero-count defense entry should be pruned after damage removes the last counter"
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
    fn triggering_source_damage_uses_event_source_context() {
        let mut state = GameState::new_two_player(42);
        let ability_source = create_object(
            &mut state,
            CardId(51),
            PlayerId(0),
            "Ability Source".to_string(),
            Zone::Battlefield,
        );
        let triggering_source =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Lifelink);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: triggering_source,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                triggering_source,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        });
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: Some(DamageSource::TriggeringSource),
            },
            vec![TargetRef::Player(PlayerId(1))],
            ability_source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 18);
        assert_eq!(
            state.players[0].life, 22,
            "lifelink must come from the triggering source, not the ability source"
        );
    }

    /// Issue #1670 (zero-target follow-up) — Star Athlete: "Whenever this
    /// creature attacks, choose up to one target nonland permanent. Its
    /// controller may sacrifice it. If they don't, this creature deals 5 damage
    /// to that player." When zero targets are chosen ("up to one target"), the
    /// "that player" anaphor has no referent, so the damage must do nothing
    /// (CR 608.2c). It must NOT fall back through event-context to the attacking
    /// creature's own controller (the pre-fix behavior this guards against).
    #[test]
    fn parent_target_controller_damage_no_ops_when_no_target_chosen() {
        let mut state = GameState::new_two_player(42);
        let star_athlete = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Star Athlete".to_string(),
            Zone::Battlefield,
        );
        // The attacks trigger is live: its source is Star Athlete, controlled by
        // PlayerId(0). Resolving ParentTargetController through event-context
        // would therefore (wrongly) pick PlayerId(0) as "that player".
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![star_athlete],
            defending_player: PlayerId(1),
            attacks: vec![],
        });
        // Zero targets chosen for "up to one target nonland permanent".
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTargetController,
                damage_source: None,
            },
            vec![],
            star_athlete,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life, 20,
            "no chosen target → no referent for 'that player'; the attacker's \
             controller must not take the damage"
        );
        assert_eq!(
            state.players[1].life, 20,
            "no chosen target → the damage does nothing for any player"
        );
    }

    /// Issue #1670 — companion positive case: when a nonland permanent IS chosen,
    /// "that player" binds to that permanent's controller (CR 109.4) and the 5
    /// damage is dealt to that player, not to the attacker's controller. Proves
    /// the no-op fix above leaves the normal (target-chosen) path intact.
    #[test]
    fn parent_target_controller_damage_hits_chosen_permanents_controller() {
        let mut state = GameState::new_two_player(42);
        let star_athlete = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Star Athlete".to_string(),
            Zone::Battlefield,
        );
        // The chosen "target nonland permanent" is controlled by PlayerId(1).
        let chosen = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Chosen Permanent".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![star_athlete],
            defending_player: PlayerId(1),
            attacks: vec![],
        });
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTargetController,
                damage_source: None,
            },
            vec![TargetRef::Object(chosen)],
            star_athlete,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[1].life, 15,
            "'that player' is the chosen permanent's controller (PlayerId(1)), who takes 5"
        );
        assert_eq!(
            state.players[0].life, 20,
            "the attacker's controller must not take the damage"
        );
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
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let ability = make_ability_with_source(3, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 3);
        // CR 702.15b: Lifelink triggers on creature damage too.
        assert_eq!(state.players[0].life, 23); // 20 + 3
    }

    /// Issue #2013: Judith grants deathtouch/lifelink to the cast instant/sorcery via a
    /// `SpecificObject` transient effect; damage must read effective keywords, not printed.
    #[test]
    fn transient_spell_keyword_grants_apply_to_damage() {
        let mut state = GameState::new_two_player(42);
        let spell_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        state.add_transient_continuous_effect(
            spell_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: spell_id },
            vec![ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Lifelink,
            }],
            None,
        );
        let ability = make_ability_with_source(2, vec![TargetRef::Player(PlayerId(1))], spell_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 18);
        assert_eq!(
            state.players[0].life, 22,
            "lifelink from a transient spell grant must apply when the spell deals damage"
        );
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
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
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
                damage_source: None,
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

    /// Issue #3293: Joyful Stormsculptor — opponent+battle compound damage must
    /// not hit opponent-controlled creatures.
    #[test]
    fn resolve_all_opponent_and_battle_they_protect_skips_creatures() {
        let mut state = GameState::new_two_player(42);
        let creature_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(3);
            obj.base_toughness = Some(3);
        }
        let battle_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Protected Siege".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&battle_id).unwrap();
            obj.card_types.core_types.push(CoreType::Battle);
            obj.defense = Some(3);
            obj.base_defense = Some(3);
            obj.counters.insert(CounterType::Defense, 3);
            obj.chosen_attributes
                .push(ChosenAttribute::Player(PlayerId(1)));
        }

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Battle).properties(vec![
                    FilterProp::ProtectorMatches {
                        controller: ControllerRef::Opponent,
                    },
                ])),
                player_filter: Some(PlayerFilter::Opponent),
                damage_source: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let p1_life_before = state.players[1].life;

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&creature_id].damage_marked, 0,
            "opponent creatures must not be damaged"
        );
        assert_eq!(state.objects[&battle_id].defense, Some(2));
        assert_eq!(state.players[1].life, p1_life_before - 1);
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
                damage_source: None,
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

    /// CR 120.10: excess damage to a planeswalker = damage beyond the loyalty it
    /// had before the hit. 6 damage to a 3-loyalty planeswalker → 3 excess. The
    /// loyalty counter is removed (clamped at 0) before excess is computed, so the
    /// pre-hit loyalty must be captured beforehand; reconstructing it as
    /// `post_loyalty + damage` yields 0 excess for any overkill.
    #[test]
    fn excess_damage_to_planeswalker() {
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
            obj.loyalty = Some(3);
            obj.counters.insert(CounterType::Loyalty, 3);
        }
        let ability = make_ability(6, vec![TargetRef::Object(pw_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 3, "6 damage - 3 loyalty = 3 excess");
        } else {
            panic!("expected DamageDealt event");
        }
    }

    /// CR 120.10: excess damage to a battle = damage beyond its defense before the
    /// hit. 5 damage to a 2-defense battle → 3 excess.
    #[test]
    fn excess_damage_to_battle() {
        let mut state = GameState::new_two_player(42);
        let battle_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Siege".to_string(),
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

        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 3, "5 damage - 2 defense = 3 excess");
        } else {
            panic!("expected DamageDealt event");
        }
    }

    /// CR 120.3 + CR 120.10: a permanent with multiple listed card types gets
    /// each applicable damage result, and its excess damage is the greatest
    /// amount among the creature/planeswalker/battle calculations.
    #[test]
    fn damage_to_creature_planeswalker_applies_both_results_and_uses_greatest_excess() {
        let mut state = GameState::new_two_player(42);
        let permanent_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Gideon".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&permanent_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.toughness = Some(5);
            obj.loyalty = Some(3);
            obj.counters.insert(CounterType::Loyalty, 3);
        }
        let ability = make_ability(6, vec![TargetRef::Object(permanent_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&permanent_id).unwrap();
        assert_eq!(obj.damage_marked, 6, "creature damage must be marked");
        assert_eq!(obj.loyalty, Some(0), "planeswalker loyalty must be removed");

        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(
                *excess, 3,
                "max(creature excess 1, planeswalker excess 3) = 3"
            );
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
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
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
                damage_source: None,
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

    #[test]
    fn damage_all_recipient_relative_amounts_preserved_in_replacement_continuation() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Baki's Curse".to_string(),
            Zone::Battlefield,
        );
        let bear1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear1".to_string(),
            Zone::Battlefield,
        );
        let bear2 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear2".to_string(),
            Zone::Battlefield,
        );
        let bear3 = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Bear3".to_string(),
            Zone::Battlefield,
        );
        for id in [bear1, bear2, bear3] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        for (card_id, host) in [(5, bear1), (6, bear2), (7, bear2), (8, bear3)] {
            let aura = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Aura {card_id}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(host.into());
        }
        install_optional_damage_replacement(&mut state);

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Multiply {
                    factor: 2,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter {
                                type_filters: vec![TypeFilter::Subtype("Aura".to_string())],
                                controller: None,
                                properties: vec![FilterProp::AttachedToRecipient],
                            }),
                        },
                    }),
                },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: None,
                damage_source: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let cont = state
            .pending_continuation
            .as_ref()
            .expect("expected pending_continuation for remaining batch targets");
        let summary = collect_chain_summary(&cont.chain);
        assert_eq!(
            summary,
            vec![
                (source_id, TargetRef::Object(bear2), 4),
                (source_id, TargetRef::Object(bear3), 2),
            ]
        );
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
                damage_source: None,
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
                damage_source: None,
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
                damage_source: None,
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

    /// CR 120.3 + CR 119.3a: Pyrohemia / Pestilence runtime behavior — when
    /// `DamageAll { player_filter: Some(PlayerFilter::All) }` resolves, every
    /// creature on the battlefield (including the controller's own) takes
    /// damage AND every player (including the controller) loses life. This
    /// verifies the parser's new compound shape is honored end-to-end.
    #[test]
    fn damage_all_each_creature_and_each_player_hits_controller_too() {
        use crate::types::ability::{PlayerFilter, TypeFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);

        let source_id = create_object(
            &mut state,
            CardId(1),
            controller,
            "Pyrohemia".to_string(),
            Zone::Battlefield,
        );

        // Controller's creature — must take damage (no controller restriction).
        let own_creature = create_object(
            &mut state,
            CardId(2),
            controller,
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

        // Opponent's creature — must also take damage.
        let opp_creature = create_object(
            &mut state,
            CardId(3),
            opponent,
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

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                player_filter: Some(PlayerFilter::All),
                damage_source: None,
            },
            vec![],
            source_id,
            controller,
        );

        let life_controller_before = state.players[controller.0 as usize].life;
        let life_opponent_before = state.players[opponent.0 as usize].life;

        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).expect("DamageAll resolves");

        // CR 120.3a: BOTH players lost 1 life.
        assert_eq!(
            state.players[controller.0 as usize].life,
            life_controller_before - 1,
            "controller must take damage from PlayerFilter::All"
        );
        assert_eq!(
            state.players[opponent.0 as usize].life,
            life_opponent_before - 1
        );
        // CR 120.3e: BOTH creatures marked 1 (controller=None means no exclusion).
        assert_eq!(state.objects[&own_creature].damage_marked, 1);
        assert_eq!(state.objects[&opp_creature].damage_marked, 1);
    }

    // --- "deals damage equal to <source quantity> to any other target" ---
    //
    // CR 120.1 + CR 115.4: A creature's ability that deals damage "equal to his
    // power" / "equal to the number of +1/+1 counters on him" to "any other
    // target" parses to `Effect::DealDamage { amount: <source quantity>, target:
    // Typed{ properties: [Another] } }`. Per CR 115.4 "another target" excludes
    // the ability's own source from the legal targets (the `Another` marker),
    // while the runtime still enumerates players + creatures/planeswalkers/
    // battles (Screaming Nemesis precedent). These tests drive the real parsed
    // output through the activation/resolution pipeline.

    /// Extract the activated ability definition that a parsed "gains
    /// \"{T}: …\" until end of turn" trigger grants.
    #[cfg(test)]
    fn extract_granted_ability(
        oracle: &str,
        card_name: &str,
    ) -> crate::types::ability::AbilityDefinition {
        use crate::types::ability::{ContinuousModification, Effect};
        let parsed = crate::parser::oracle::parse_oracle_text(
            oracle,
            card_name,
            &[],
            &["Creature".into()],
            &[],
        );
        let trigger = parsed
            .triggers
            .into_iter()
            .next()
            .expect("cast trigger present");
        let execute = trigger.execute.expect("trigger has an execute ability");
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*execute.effect
        else {
            panic!(
                "expected GenericEffect granting an ability, got {:?}",
                execute.effect
            );
        };
        let modification = static_abilities
            .iter()
            .flat_map(|s| s.modifications.iter())
            .find_map(|m| match m {
                ContinuousModification::GrantAbility { definition } => Some((**definition).clone()),
                _ => None,
            });
        modification.expect("a GrantAbility modification")
    }

    /// CR 120.1 + CR 115.4 + CR 208.3 — DISCRIMINATING runtime gate for Iron
    /// Fist, Living Weapon. Its cast-trigger grants "{T}: ~ deals damage equal
    /// to his power to any other target". Parsing the full card, attaching the
    /// granted ability to a 4-power Iron Fist, and activating it at an opponent
    /// creature must deal exactly 4 damage. Reverting the gendered-pronoun
    /// quantity fix makes "his power" fall to `Effect::Unimplemented`, so the
    /// granted ability deals no damage and `ActivateAbility` never reaches a
    /// damage resolution — this assertion flips.
    #[test]
    fn iron_fist_granted_ability_deals_damage_equal_to_power() {
        use crate::game::scenario::GameScenario;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);
        const P1: PlayerId = PlayerId(1);

        let granted = extract_granted_ability(
            "Whenever you cast a spell that targets a creature you control, Iron Fist gains \
             \"{T}: Iron Fist deals damage equal to his power to any other target\" until end of turn.",
            "Iron Fist, Living Weapon",
        );

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let iron_fist = scenario
            .add_creature(P0, "Iron Fist, Living Weapon", 4, 4)
            .with_ability_definition(granted)
            .id();
        let victim = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
        let mut runner = scenario.build();

        let ability_index = runner.state().objects[&iron_fist].abilities.len() - 1;
        let outcome = runner
            .activate(iron_fist, ability_index)
            .target_object(victim)
            .resolve();

        // CR 208.3: damage equals the source's power (4).
        assert_eq!(
            outcome.state().objects[&victim].damage_marked,
            4,
            "Iron Fist must deal damage equal to his power (4) to the chosen other target"
        );
    }

    /// CR 115.4 — DISCRIMINATING runtime gate that "any OTHER target" (i.e.
    /// "another target") excludes the source. Iron Fist's granted ability must
    /// NOT offer Iron Fist itself as a legal target. If the `Another` exclusion
    /// were lost, the source would appear in the legal-target set.
    #[test]
    fn iron_fist_granted_ability_excludes_itself_as_target() {
        use crate::game::scenario::GameScenario;
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::phase::Phase;
        use crate::types::TargetRef;

        const P0: PlayerId = PlayerId(0);
        const P1: PlayerId = PlayerId(1);

        let granted = extract_granted_ability(
            "Whenever you cast a spell that targets a creature you control, Iron Fist gains \
             \"{T}: Iron Fist deals damage equal to his power to any other target\" until end of turn.",
            "Iron Fist, Living Weapon",
        );

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let iron_fist = scenario
            .add_creature(P0, "Iron Fist, Living Weapon", 4, 4)
            .with_ability_definition(granted)
            .id();
        let victim = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
        let mut runner = scenario.build();

        let ability_index = runner.state().objects[&iron_fist].abilities.len() - 1;
        runner
            .act(GameAction::ActivateAbility {
                source_id: iron_fist,
                ability_index,
            })
            .expect("activation announced");

        let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
            panic!(
                "expected TargetSelection after activation, got {:?}",
                runner.state().waiting_for
            );
        };
        let legal = &target_slots[0].legal_targets;
        assert!(
            !legal.contains(&TargetRef::Object(iron_fist)),
            "'any other target' must exclude the source (Iron Fist), got {legal:?}"
        );
        assert!(
            legal.contains(&TargetRef::Object(victim)),
            "the opponent creature must be a legal target, got {legal:?}"
        );
    }

    /// CR 120.1 + CR 122.1 + CR 115.4 — DISCRIMINATING runtime gate for Red
    /// Hulk's Enrage reflex. Its reflexive "he deals damage equal to the number
    /// of +1/+1 counters on him to any other target" parses to
    /// `DealDamage { amount: CountersOn{Source, P1P1}, target: Another }`.
    /// Resolving that effect against a Red Hulk carrying 3 +1/+1 counters must
    /// deal 3 damage to the chosen other target, and must exclude Red Hulk
    /// itself. Reverting the "on him" source-pronoun fix routes the reflex to
    /// `Effect::Unimplemented`, dealing 0 damage — this flips.
    #[test]
    fn red_hulk_reflex_deals_damage_equal_to_counter_count() {
        use crate::game::scenario::GameScenario;
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;
        use crate::types::game_state::WaitingFor;
        use crate::types::phase::Phase;
        use crate::types::TargetRef;

        const P0: PlayerId = PlayerId(0);
        const P1: PlayerId = PlayerId(1);

        // Pull the reflexive damage effect out of the parsed enrage trigger so the
        // runtime test exercises the real parser output.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Reach, trample\nEnrage — Whenever Red Hulk is dealt damage, put a +1/+1 \
             counter on him. When you do, he deals damage equal to the number of +1/+1 \
             counters on him to any other target.",
            "Red Hulk",
            &[],
            &["Creature".into()],
            &[],
        );
        let trigger = parsed.triggers.into_iter().next().expect("enrage trigger");
        let execute = trigger.execute.expect("trigger execute");
        let reflex = execute
            .sub_ability
            .as_deref()
            .expect("reflexive when-you-do sub-ability")
            .clone();
        assert!(
            matches!(
                &*reflex.effect,
                crate::types::ability::Effect::DealDamage {
                    amount: crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::CountersOn {
                            scope: crate::types::ability::ObjectScope::Source,
                            counter_type: Some(CounterType::Plus1Plus1),
                        },
                    },
                    ..
                }
            ),
            "reflex must deal damage equal to +1/+1 counters on the source, got {:?}",
            reflex.effect
        );

        // Attach the reflexive damage as a no-cost activated ability so it can be
        // driven through the activation/resolution pipeline; Red Hulk carries 3
        // counters.
        let mut ability = reflex;
        ability.kind = crate::types::ability::AbilityKind::Activated;
        ability.cost = None;
        ability.condition = None;

        // Build a fresh scenario for each assertion: the announce-and-inspect
        // pass leaves a target prompt open, so the resolve pass uses its own
        // runner.
        let build_red_hulk = |ability: crate::types::ability::AbilityDefinition| {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let red_hulk = {
                let mut builder = scenario.add_creature(P0, "Red Hulk", 7, 7);
                builder.with_plus_counters(3);
                builder.with_ability_definition(ability);
                builder.id()
            };
            let victim = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
            (scenario.build(), red_hulk, victim)
        };

        // CR 115.4: "any other target" ("another target") excludes the source.
        {
            let (mut runner, red_hulk, victim) = build_red_hulk(ability.clone());
            let ability_index = runner.state().objects[&red_hulk].abilities.len() - 1;
            runner
                .act(GameAction::ActivateAbility {
                    source_id: red_hulk,
                    ability_index,
                })
                .expect("activation announced");
            let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for
            else {
                panic!(
                    "expected TargetSelection, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert!(
                !target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(red_hulk)),
                "'any other target' must exclude Red Hulk itself"
            );
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(victim)),
                "the opponent creature must be a legal target"
            );
        }

        // CR 122.1: damage equals the number of +1/+1 counters on the source (3).
        {
            let (mut runner, red_hulk, victim) = build_red_hulk(ability);
            let ability_index = runner.state().objects[&red_hulk].abilities.len() - 1;
            let outcome = runner
                .activate(red_hulk, ability_index)
                .target_object(victim)
                .resolve();
            assert_eq!(
                outcome.state().objects[&victim].damage_marked,
                3,
                "Red Hulk's reflex must deal damage equal to its +1/+1 counter count (3)"
            );
        }
        // Silence the unused CounterType import on builds where the matches! arm
        // already consumed it via the qualified path.
        let _ = CounterType::Plus1Plus1;
    }
}
