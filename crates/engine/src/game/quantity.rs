//! Dynamic quantity resolution for QuantityExpr values.
//!
//! Evaluates QuantityRef variants (ObjectCount, PlayerCount, CountersOnSelf, etc.)
//! against the current game state at resolution time. Used by effect resolvers
//! to support "for each [X]" patterns on Draw, DealDamage, GainLife, LoseLife, Mill.

use std::collections::HashSet;

use crate::game::arithmetic::{u32_to_i32_saturating, usize_to_i32_saturating};
use crate::game::filter::{
    matches_target_filter, spell_record_matches_filter, type_filter_matches, FilterContext,
};
use crate::game::speed::effective_speed;
use crate::types::ability::{
    AggregateFunction, ControllerRef, CountScope, ObjectProperty, PlayerFilter, QuantityExpr,
    QuantityRef, ResolvedAbility, RoundingMode, TargetRef, TypeFilter, ZoneRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::parse_counter_type;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Scope information for quantity resolution.
///
/// Some `QuantityRef` variants need to distinguish between "the static ability
/// source" and "the object entering the battlefield" — e.g., Wildgrowth
/// Archaic's `ColorsSpentOnSelf` during an ETB replacement refers to the
/// *entering* creature's paid colors, not the Archaic itself. Most callers
/// resolve against the source only and go through `resolve_quantity`; the
/// replacement pipeline threads a richer context via `resolve_quantity_with_ctx`.
#[derive(Debug, Clone, Copy)]
pub struct QuantityContext {
    /// The object entering the battlefield, when in an ETB-scoped replacement.
    /// `None` outside that context.
    pub entering: Option<ObjectId>,
    /// The static ability source (always set).
    pub source: ObjectId,
}

impl QuantityContext {
    /// Object to resolve "self"-scoped spell refs (e.g., colors spent to cast)
    /// against: the entering object when in ETB scope, else the static source.
    fn self_object(&self) -> ObjectId {
        self.entering.unwrap_or(self.source)
    }
}

/// Resolve a QuantityExpr to a concrete integer value.
///
/// `controller` is the player who controls the ability (used for relative filters).
/// `source_id` is the object that owns the ability (used for CountersOnSelf, filter matching).
pub fn resolve_quantity(
    state: &GameState,
    expr: &QuantityExpr,
    controller: PlayerId,
    source_id: ObjectId,
) -> i32 {
    resolve_quantity_with_ctx(
        state,
        expr,
        controller,
        QuantityContext {
            entering: None,
            source: source_id,
        },
    )
}

/// Resolve a QuantityExpr with an explicit `QuantityContext` so variants like
/// `ColorsSpentOnSelf` can distinguish entering-object scope from static-source
/// scope. Used by the replacement pipeline for ETB-counter effects.
pub fn resolve_quantity_with_ctx(
    state: &GameState,
    expr: &QuantityExpr,
    controller: PlayerId,
    ctx: QuantityContext,
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(state, qty, controller, ctx, &[], None, None),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let base = resolve_quantity_with_ctx(state, inner, controller, ctx);
            half_rounded(base, *rounding)
        }
        QuantityExpr::Offset { inner, offset } => {
            resolve_quantity_with_ctx(state, inner, controller, ctx) + offset
        }
        QuantityExpr::Multiply { factor, inner } => {
            factor * resolve_quantity_with_ctx(state, inner, controller, ctx)
        }
    }
}

/// CR 603.4: Resolve a `QuantityExpr` for an intervening-if check using an
/// explicit `trigger_event` override. `state.current_trigger_event` is not
/// populated at trigger-detection time (it is only set at resolution via
/// `stack::resolve_top`), so event-scoped refs like
/// `QuantityRef::ManaSpentOnTriggeringSpell` would otherwise resolve to 0
/// during the detection-time condition check. This function substitutes the
/// event-scoped value from the passed `event` before delegating to
/// `resolve_quantity` for everything else.
pub(crate) fn resolve_quantity_for_trigger_check(
    state: &GameState,
    expr: &QuantityExpr,
    controller: PlayerId,
    source_id: ObjectId,
    event: Option<&crate::types::events::GameEvent>,
) -> i32 {
    // Fast path: when current_trigger_event is already set (resolution-time
    // re-check in stack::resolve_top), the default resolver reads it directly.
    if state.current_trigger_event.is_some() {
        return resolve_quantity(state, expr, controller, source_id);
    }
    if let Some(event) = event {
        if let Some(value) = resolve_event_scoped_ref(state, expr, event) {
            return value;
        }
        // CR 603.4: Make the triggering event visible to the resolver for
        // detection-time `ObjectCount` checks that need to subtract the
        // triggering object ("other <type>" intervening-if patterns). The TLS
        // override avoids a full `GameState` clone (which would be O(objects))
        // every time a trigger condition is checked.
        return with_detection_trigger_event(event, || {
            resolve_quantity(state, expr, controller, source_id)
        });
    }
    resolve_quantity(state, expr, controller, source_id)
}

std::thread_local! {
    /// Detection-time trigger event override. Populated only inside
    /// `resolve_quantity_for_trigger_check` when `state.current_trigger_event`
    /// is `None`. Consumed by `ObjectCount` evaluation (see `resolve_ref`) to
    /// implement `FilterProp::OtherThanTriggerObject` semantics.
    static DETECTION_TRIGGER_EVENT: std::cell::RefCell<Option<crate::types::events::GameEvent>>
        = const { std::cell::RefCell::new(None) };
}

fn with_detection_trigger_event<R>(
    event: &crate::types::events::GameEvent,
    f: impl FnOnce() -> R,
) -> R {
    DETECTION_TRIGGER_EVENT.with(|slot| {
        let prev = slot.replace(Some(event.clone()));
        let result = f();
        slot.replace(prev);
        result
    })
}

/// Read the detection-time trigger event override, if set. Returns `None`
/// outside `resolve_quantity_for_trigger_check`.
fn detection_trigger_event() -> Option<crate::types::events::GameEvent> {
    DETECTION_TRIGGER_EVENT.with(|slot| slot.borrow().clone())
}

/// CR 603.4 + CR 109.3: Recursively check whether a `TargetFilter` carries
/// `FilterProp::OtherThanTriggerObject` anywhere in its property tree. Used
/// by the `ObjectCount` resolver to decide whether to subtract the triggering
/// object from a count (Valakut, the Molten Pinnacle — "five other Mountains").
fn filter_contains_other_than_trigger_object(filter: &crate::types::ability::TargetFilter) -> bool {
    use crate::types::ability::{FilterProp, TargetFilter};
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::OtherThanTriggerObject)),
        TargetFilter::Not { filter: inner } => filter_contains_other_than_trigger_object(inner),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(filter_contains_other_than_trigger_object),
        _ => false,
    }
}

/// Substitute an event-scoped `QuantityRef` (currently only
/// `ManaSpentOnTriggeringSpell`) using an explicit event, returning `None`
/// when the expression does not reference an event-scoped quantity.
fn resolve_event_scoped_ref(
    state: &GameState,
    expr: &QuantityExpr,
    event: &crate::types::events::GameEvent,
) -> Option<i32> {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::ManaSpentOnTriggeringSpell,
        } => {
            let id = crate::game::targeting::extract_source_from_event(event)?;
            let obj = state.objects.get(&id)?;
            Some(u32_to_i32_saturating(obj.mana_spent_to_cast_amount))
        }
        _ => None,
    }
}

/// Resolve a QuantityExpr with access to the ability's targets.
///
/// Required for TargetPower which needs to look up the targeted permanent.
pub fn resolve_quantity_with_targets(
    state: &GameState,
    expr: &QuantityExpr,
    ability: &ResolvedAbility,
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(
            state,
            qty,
            ability.controller,
            QuantityContext {
                entering: None,
                source: ability.source_id,
            },
            &ability.targets,
            ability.chosen_x,
            Some(ability),
        ),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let base = resolve_quantity_with_targets(state, inner, ability);
            half_rounded(base, *rounding)
        }
        QuantityExpr::Offset { inner, offset } => {
            resolve_quantity_with_targets(state, inner, ability) + offset
        }
        QuantityExpr::Multiply { factor, inner } => {
            factor * resolve_quantity_with_targets(state, inner, ability)
        }
    }
}

/// Resolve a QuantityExpr with an explicit target slice but no full
/// `ResolvedAbility`. Used by the combat-tax pipeline (CR 118.12a +
/// CR 202.3e) to resolve per-attacker `CountersOnTarget`-style scaling
/// (Nils, Discipline Enforcer) where each declared attacker is supplied
/// as the `TargetRef::Object` for a single resolution.
pub fn resolve_quantity_with_targets_slice(
    state: &GameState,
    expr: &QuantityExpr,
    controller: PlayerId,
    source_id: ObjectId,
    targets: &[TargetRef],
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(
            state,
            qty,
            controller,
            QuantityContext {
                entering: None,
                source: source_id,
            },
            targets,
            None,
            None,
        ),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let base =
                resolve_quantity_with_targets_slice(state, inner, controller, source_id, targets);
            half_rounded(base, *rounding)
        }
        QuantityExpr::Offset { inner, offset } => {
            resolve_quantity_with_targets_slice(state, inner, controller, source_id, targets)
                + offset
        }
        QuantityExpr::Multiply { factor, inner } => {
            factor
                * resolve_quantity_with_targets_slice(state, inner, controller, source_id, targets)
        }
    }
}

/// Resolve a QuantityExpr scoped to a specific player.
///
/// Used by `DamageEachPlayer` to evaluate per-player quantities like
/// "the number of nonbasic lands that player controls".
/// `scope_player` overrides `controller` for `ObjectCount` (ControllerRef::You)
/// and `SpellsCastThisTurn` resolution.
pub(crate) fn resolve_quantity_scoped(
    state: &GameState,
    expr: &QuantityExpr,
    source_id: ObjectId,
    scope_player: PlayerId,
) -> i32 {
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(
            state,
            qty,
            scope_player,
            QuantityContext {
                entering: None,
                source: source_id,
            },
            &[],
            None,
            None,
        ),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let base = resolve_quantity_scoped(state, inner, source_id, scope_player);
            half_rounded(base, *rounding)
        }
        QuantityExpr::Offset { inner, offset } => {
            resolve_quantity_scoped(state, inner, source_id, scope_player) + offset
        }
        QuantityExpr::Multiply { factor, inner } => {
            factor * resolve_quantity_scoped(state, inner, source_id, scope_player)
        }
    }
}

/// CR 107.1a: "If a spell or ability could generate a fractional number, the
/// spell or ability will tell you whether to round up or down." Integer-divides
/// by 2 in the direction specified by the parsed `RoundingMode`. Negative
/// inputs are resolver-safe: `(−1 + 1) / 2 = 0` rounds up, `−1 / 2 = 0` rounds
/// down (Rust truncates toward zero), matching CR 107.1b which permits
/// negative intermediate values but forbids negative damage/life results.
fn half_rounded(value: i32, rounding: RoundingMode) -> i32 {
    match rounding {
        RoundingMode::Up => (value + 1) / 2,
        RoundingMode::Down => value / 2,
    }
}

fn resolve_ref(
    state: &GameState,
    qty: &QuantityRef,
    controller: PlayerId,
    ctx: QuantityContext,
    targets: &[TargetRef],
    chosen_x: Option<u32>,
    ability: Option<&ResolvedAbility>,
) -> i32 {
    let source_id = ctx.source;
    // Build a FilterContext that preserves ability scope (for `chosen_x`/targets
    // in nested filter thresholds) when available, falling back to the controller
    // override used by `resolve_quantity_scoped`. CR 107.2 governs the fallback
    // path when no ability is in scope (X → 0).
    let filter_ctx = match ability {
        Some(a) => FilterContext::from_ability(a),
        None => FilterContext::from_source_with_controller(source_id, controller),
    };
    let player = state.players.iter().find(|p| p.id == controller);
    match qty {
        QuantityRef::HandSize => player.map_or(0, |p| usize_to_i32_saturating(p.hand.len())),
        QuantityRef::LifeTotal => player.map_or(0, |p| p.life),
        // CR 122.1: Counter-kind lookup summed across scope players. Controller
        // scope resolves to a single player; Opponents/All may span multiple.
        // Per-player u32 is widened to u64 before summing; the i32::try_from
        // saturates on the (only theoretically reachable) overflow.
        QuantityRef::PlayerCounter { kind, scope } => {
            let total: u64 = scoped_players(state, scope, controller)
                .map(|p| u64::from(p.player_counter(kind)))
                .sum();
            i32::try_from(total).unwrap_or(i32::MAX)
        }
        QuantityRef::GraveyardSize => {
            player.map_or(0, |p| usize_to_i32_saturating(p.graveyard.len()))
        }
        QuantityRef::LifeAboveStarting => {
            player.map_or(0, |p| p.life - state.format_config.starting_life)
        }
        // CR 103.4: The format's starting life total.
        QuantityRef::StartingLifeTotal => state.format_config.starting_life,
        // CR 118.4: Total life lost this turn by the controller.
        QuantityRef::LifeLostThisTurn => {
            player.map_or(0, |p| u32_to_i32_saturating(p.life_lost_this_turn))
        }
        QuantityRef::Speed => i32::from(effective_speed(state, controller)),
        QuantityRef::ObjectCount { filter } => {
            // CR 400.1: If the filter constrains to a specific zone via InZone,
            // count objects in that zone. Otherwise default to battlefield.
            let zone = filter
                .extract_in_zone()
                .unwrap_or(crate::types::zones::Zone::Battlefield);
            let raw = crate::game::targeting::zone_object_ids(state, zone)
                .iter()
                .filter(|&&id| matches_target_filter(state, id, filter, &filter_ctx))
                .count();
            // CR 603.4 + CR 109.3: If the filter carries `OtherThanTriggerObject`,
            // exclude the triggering object from the count (e.g., Valakut's "five
            // other Mountains" — the newly-entered Mountain is counted by the
            // per-object filter as a pass-through, then subtracted here). Uses
            // the currently-resolving trigger event; at detection time the event
            // is threaded in via `resolve_quantity_for_trigger_check`, which sets
            // a scoped override read here.
            //
            // When the trigger event carries no object subject (e.g. a `PhaseChanged`
            // event for "at the beginning of your upkeep" / "end step"), the
            // "other" modifier degrades to "other than the ability source" — this
            // matches CR 109.3's general sense of "other" as "not the speaking
            // object" and preserves Platoon-Dispenser-style "two or more other
            // creatures" semantics where source == the only entity to exclude.
            let adjusted = if filter_contains_other_than_trigger_object(filter) {
                // Prefer the live `current_trigger_event` (resolution-time);
                // fall back to the detection-time TLS override populated by
                // `resolve_quantity_for_trigger_check`.
                let triggering_id = state
                    .current_trigger_event
                    .as_ref()
                    .and_then(crate::game::targeting::extract_source_from_event)
                    .or_else(|| {
                        detection_trigger_event()
                            .as_ref()
                            .and_then(crate::game::targeting::extract_source_from_event)
                    })
                    .unwrap_or(source_id);
                if matches_target_filter(state, triggering_id, filter, &filter_ctx) {
                    raw.saturating_sub(1)
                } else {
                    raw
                }
            } else {
                raw
            };
            usize_to_i32_saturating(adjusted)
        }
        // CR 201.2 + CR 603.4: Count of distinct names among matching objects.
        // Field of the Dead: "seven or more lands with different names". Two
        // objects with the same printed name count once.
        //
        // CR 201.2a: Sameness is defined by printed name, so read `base_name`
        // (not the layer-applied `name`) to match how CR defines object
        // identity. Objects with no name do not share a name with any other
        // object, including one another — they are each individually unique,
        // so they are counted but not deduped.
        QuantityRef::ObjectCountDistinctNames { filter } => {
            let zone = filter
                .extract_in_zone()
                .unwrap_or(crate::types::zones::Zone::Battlefield);
            let mut distinct_named: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut unnamed_count: usize = 0;
            for id in crate::game::targeting::zone_object_ids(state, zone) {
                if !matches_target_filter(state, id, filter, &filter_ctx) {
                    continue;
                }
                if let Some(obj) = state.objects.get(&id) {
                    if obj.base_name.is_empty() {
                        unnamed_count += 1;
                    } else {
                        distinct_named.insert(obj.base_name.clone());
                    }
                }
            }
            usize_to_i32_saturating(distinct_named.len() + unnamed_count)
        }
        QuantityRef::PlayerCount { filter } => {
            resolve_player_count(state, filter, controller, source_id)
        }
        QuantityRef::CountersOnSelf { counter_type } => state
            .objects
            .get(&source_id)
            .map(|obj| {
                let ct = parse_counter_type(counter_type);
                u32_to_i32_saturating(obj.counters.get(&ct).copied().unwrap_or(0))
            })
            .unwrap_or(0),
        // CR 107.3a + CR 601.2b + CR 107.3i: "X" resolves to the value chosen at
        // cast time, carried on the resolving ability's `chosen_x`
        // (CR 601.2b announcement; CR 107.3i makes all instances share the value).
        //
        // CR 107.3e + CR 107.3m + CR 603.7c: When the trigger source itself has
        // no `chosen_x` (SpellCast triggers and similar event triggers do not
        // have their own cost), fall back to the triggering spell's
        // `cost_x_paid`. This covers "whenever you cast your first spell with
        // {X} in its mana cost each turn, put X +1/+1 counters on ~" — the X
        // there is the triggering spell's X, not this trigger's X (which
        // doesn't exist). CR 107.3e explicitly permits an ability to refer to
        // X of another object's cost.
        //
        // Other named variables (set by `NamedChoice` handlers for things like
        // "chosen number") keep their single-responsibility path through
        // `last_named_choice`.
        QuantityRef::Variable { name } if name == "X" => chosen_x
            .map(u32_to_i32_saturating)
            .or_else(|| {
                state
                    .current_trigger_event
                    .as_ref()
                    .and_then(crate::game::targeting::extract_source_from_event)
                    .and_then(|id| state.objects.get(&id))
                    .and_then(|obj| obj.cost_x_paid)
                    .map(u32_to_i32_saturating)
            })
            .unwrap_or(0),
        QuantityRef::Variable { .. } => state
            .last_named_choice
            .as_ref()
            .and_then(|choice| match choice {
                crate::types::ability::ChoiceValue::Number(value) => Some(i32::from(*value)),
                _ => None,
            })
            .unwrap_or(0),
        // CR 208.3 + CR 113.6: A creature's power/toughness from current state,
        // falling back to Last Known Information if the source has left the battlefield.
        QuantityRef::SelfPower => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.power)
            .or_else(|| state.lki_cache.get(&source_id).and_then(|lki| lki.power))
            .unwrap_or(0),
        QuantityRef::SelfToughness => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.toughness)
            .or_else(|| {
                state
                    .lki_cache
                    .get(&source_id)
                    .and_then(|lki| lki.toughness)
            })
            .unwrap_or(0),
        // CR 107.3e: Aggregate queries over game objects.
        // Uses extract_in_zone() to support non-battlefield zones (exile, graveyard, etc.),
        // same pattern as ObjectCount above.
        QuantityRef::Aggregate {
            function,
            property,
            filter,
        } => {
            let zone = filter
                .extract_in_zone()
                .unwrap_or(crate::types::zones::Zone::Battlefield);
            let zone_ids = crate::game::targeting::zone_object_ids(state, zone);
            let values = zone_ids.iter().filter_map(|&id| {
                if matches_target_filter(state, id, filter, &filter_ctx) {
                    state.objects.get(&id).map(|obj| match property {
                        ObjectProperty::Power => obj.power.unwrap_or(0),
                        ObjectProperty::Toughness => obj.toughness.unwrap_or(0),
                        // CR 202.3e: Use mana_value() which correctly excludes X.
                        ObjectProperty::ManaValue => {
                            u32_to_i32_saturating(obj.mana_cost.mana_value())
                        }
                    })
                } else {
                    None
                }
            });
            match function {
                AggregateFunction::Max => values.max().unwrap_or(0),
                AggregateFunction::Min => values.min().unwrap_or(0),
                AggregateFunction::Sum => values.sum(),
            }
        }
        QuantityRef::CountersOnTarget { counter_type } => {
            // Find the first object target and count counters of the given type.
            let ct = parse_counter_type(counter_type);
            targets
                .iter()
                .find_map(|t| {
                    if let TargetRef::Object(id) = t {
                        state.objects.get(id)
                    } else {
                        None
                    }
                })
                .map(|obj| u32_to_i32_saturating(obj.counters.get(&ct).copied().unwrap_or(0)))
                .unwrap_or(0)
        }
        // CR 122.1: Sum counters of every type on the first targeted object.
        // Used by Nils-class attack-tax scaling — per the official ruling, ALL
        // counters on the attacker (not just +1/+1 counters) count toward X.
        QuantityRef::AnyCountersOnTarget => targets
            .iter()
            .find_map(|t| {
                if let TargetRef::Object(id) = t {
                    state.objects.get(id)
                } else {
                    None
                }
            })
            .map(|obj| u32_to_i32_saturating(obj.counters.values().copied().sum::<u32>()))
            .unwrap_or(0),
        QuantityRef::CountersOnObjects {
            counter_type,
            filter,
        } => {
            // CR 122.1: When `counter_type` is `None`, sum across every counter type
            // (e.g., "counters among artifacts and creatures you control"). When
            // `Some`, count only that specific counter type.
            let ct = counter_type.as_deref().map(parse_counter_type);
            let zone = filter
                .extract_in_zone()
                .unwrap_or(crate::types::zones::Zone::Battlefield);
            crate::game::targeting::zone_object_ids(state, zone)
                .iter()
                .filter_map(|&id| {
                    if matches_target_filter(state, id, filter, &filter_ctx) {
                        state.objects.get(&id).map(|obj| match &ct {
                            Some(ct) => {
                                u32_to_i32_saturating(obj.counters.get(ct).copied().unwrap_or(0))
                            }
                            None => {
                                u32_to_i32_saturating(obj.counters.values().copied().sum::<u32>())
                            }
                        })
                    } else {
                        None
                    }
                })
                .sum()
        }
        QuantityRef::TargetPower => {
            // Find the first object target and return its power.
            targets
                .iter()
                .find_map(|t| {
                    if let TargetRef::Object(id) = t {
                        state.objects.get(id)
                    } else {
                        None
                    }
                })
                .and_then(|obj| obj.power)
                .unwrap_or(0)
        }
        QuantityRef::Devotion { colors } => u32_to_i32_saturating(
            crate::game::devotion::count_devotion(state, controller, colors),
        ),
        QuantityRef::TargetLifeTotal => {
            // CR 119.3 + CR 107.2: Find the first player target and return their life total.
            targets
                .iter()
                .find_map(|t| {
                    if let TargetRef::Player(pid) = t {
                        state.players.iter().find(|p| p.id == *pid)
                    } else {
                        None
                    }
                })
                .map_or(0, |p| p.life)
        }
        QuantityRef::TargetZoneCardCount { zone } => {
            let target_player = targets.iter().find_map(|t| {
                if let TargetRef::Player(pid) = t {
                    Some(*pid)
                } else {
                    None
                }
            });
            if let Some(pid) = target_player {
                state
                    .players
                    .iter()
                    .find(|p| p.id == pid)
                    .map_or(0, |p| match zone {
                        ZoneRef::Library => usize_to_i32_saturating(p.library.len()),
                        ZoneRef::Graveyard => usize_to_i32_saturating(p.graveyard.len()),
                        ZoneRef::Hand => usize_to_i32_saturating(p.hand.len()),
                        ZoneRef::Exile => usize_to_i32_saturating(
                            state
                                .exile
                                .iter()
                                .filter(|&&id| {
                                    state.objects.get(&id).is_some_and(|o| o.owner == pid)
                                })
                                .count(),
                        ),
                    })
            } else {
                0
            }
        }
        // CR 604.3: Count distinct card types (CoreType) across cards in a zone.
        QuantityRef::DistinctCardTypesInZone { zone, scope } => {
            let mut seen = HashSet::new();
            match zone {
                ZoneRef::Exile => {
                    for &obj_id in &state.exile {
                        if let Some(obj) = state.objects.get(&obj_id) {
                            let owner_matches = match scope {
                                CountScope::Controller => obj.owner == controller,
                                CountScope::All => true,
                                CountScope::Opponents => obj.owner != controller,
                            };
                            if owner_matches {
                                for ct in &obj.card_types.core_types {
                                    seen.insert(*ct);
                                }
                            }
                        }
                    }
                }
                ZoneRef::Graveyard | ZoneRef::Library | ZoneRef::Hand => {
                    for player in scoped_players(state, scope, controller) {
                        let zone_ids = match zone {
                            ZoneRef::Graveyard => &player.graveyard,
                            ZoneRef::Library => &player.library,
                            ZoneRef::Hand => &player.hand,
                            ZoneRef::Exile => unreachable!(),
                        };
                        for &obj_id in zone_ids {
                            if let Some(obj) = state.objects.get(&obj_id) {
                                for ct in &obj.card_types.core_types {
                                    seen.insert(*ct);
                                }
                            }
                        }
                    }
                }
            }
            usize_to_i32_saturating(seen.len())
        }
        QuantityRef::DistinctCardTypesExiledBySource => {
            let mut seen = HashSet::new();
            for linked in crate::game::players::linked_exile_cards_for_source(state, source_id) {
                if let Some(obj) = state.objects.get(&linked.exiled_id) {
                    for ct in &obj.card_types.core_types {
                        seen.insert(*ct);
                    }
                }
            }
            usize_to_i32_saturating(seen.len())
        }
        // CR 603.10a + CR 607.2a: Count cards linked as "exiled with" the
        // source. LTB triggers read the trigger-event snapshot; other contexts
        // read the live exile-link store.
        QuantityRef::CardsExiledBySource => usize_to_i32_saturating(
            crate::game::players::linked_exile_cards_for_source(state, source_id).len(),
        ),
        // CR 604.3: Count cards in a zone matching optional type filters.
        QuantityRef::ZoneCardCount {
            zone,
            card_types,
            scope,
        } => {
            let mut count = 0;
            // Per-player zones (graveyard, library)
            match zone {
                ZoneRef::Graveyard | ZoneRef::Library | ZoneRef::Hand => {
                    for player in scoped_players(state, scope, controller) {
                        let zone_ids = match zone {
                            ZoneRef::Graveyard => &player.graveyard,
                            ZoneRef::Library => &player.library,
                            ZoneRef::Hand => &player.hand,
                            ZoneRef::Exile => unreachable!(),
                        };
                        for &obj_id in zone_ids {
                            if matches_zone_card_filter(state, obj_id, card_types) {
                                count += 1;
                            }
                        }
                    }
                }
                // Exile is global; filter by owner matching scope
                ZoneRef::Exile => {
                    for &obj_id in &state.exile {
                        if let Some(obj) = state.objects.get(&obj_id) {
                            let owner_matches = match scope {
                                CountScope::Controller => obj.owner == controller,
                                CountScope::All => true,
                                CountScope::Opponents => obj.owner != controller,
                            };
                            if owner_matches && matches_zone_card_filter(state, obj_id, card_types)
                            {
                                count += 1;
                            }
                        }
                    }
                }
            }
            count
        }
        // CR 609.3: Numeric result from the preceding effect in a sub_ability chain.
        // Used for "gain life equal to the life lost this way" patterns.
        QuantityRef::PreviousEffectAmount => state.last_effect_amount.unwrap_or(0),
        // CR 609.3: "for each [thing] this way" — read the most recent tracked set size.
        QuantityRef::TrackedSetSize => state
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .map(|(_, ids)| usize_to_i32_saturating(ids.len()))
            .unwrap_or(0),
        // CR 400.7 + CR 608.2c: Read the per-resolution counter populated by
        // ChangeZoneAll when it exiles cards from a hand. Used by "draws a card
        // for each card exiled from their hand this way" (Deadly Cover-Up).
        QuantityRef::ExiledFromHandThisResolution => {
            u32_to_i32_saturating(state.exiled_from_hand_this_resolution)
        }
        // CR 603.7c: Numeric value from the triggering event.
        // Falls back to last_effect_count for sub_ability continuations where
        // current_trigger_event has no amount (e.g., "discard up to N, then draw that many").
        QuantityRef::EventContextAmount => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_amount_from_event)
            .or(state.last_effect_count)
            .unwrap_or(0),
        // CR 603.7c: Power of the source object from the triggering event.
        // CR 400.7: Falls back to LKI cache for objects that have left their zone.
        QuantityRef::EventContextSourcePower => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| {
                state
                    .objects
                    .get(&id)
                    .and_then(|obj| obj.power)
                    .or_else(|| state.lki_cache.get(&id).and_then(|lki| lki.power))
            })
            .unwrap_or(0),
        // CR 603.7c: Toughness of the source object from the triggering event.
        QuantityRef::EventContextSourceToughness => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| {
                state
                    .objects
                    .get(&id)
                    .and_then(|obj| obj.toughness)
                    .or_else(|| state.lki_cache.get(&id).and_then(|lki| lki.toughness))
            })
            .unwrap_or(0),
        // CR 603.7c: Mana value of the source object from the triggering event.
        QuantityRef::EventContextSourceManaValue => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| {
                state
                    .objects
                    .get(&id)
                    .map(|obj| u32_to_i32_saturating(obj.mana_cost.mana_value()))
                    .or_else(|| {
                        state
                            .lki_cache
                            .get(&id)
                            .map(|lki| u32_to_i32_saturating(lki.mana_value))
                    })
            })
            .unwrap_or(0),
        // CR 107.3a + CR 601.2b + CR 603.7c: The announced value of X for the
        // triggering spell. Reads `GameObject::cost_x_paid` — populated during
        // cost determination and persisted through the stack → battlefield
        // transition. Triggers resolve on the stack, so the spell object is
        // still present in `state.objects`. Falls back to 0 when no event is
        // in scope or the event-source object is gone (LKI mana_value does
        // not store X, so no fallback is meaningful).
        QuantityRef::EventContextSourceCostX => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| state.objects.get(&id))
            .and_then(|obj| obj.cost_x_paid)
            .map(u32_to_i32_saturating)
            .unwrap_or(0),
        // CR 601.2h + CR 603.4: Total mana actually spent to cast the triggering
        // spell. Reads `GameObject::mana_spent_to_cast_amount` on the spell
        // object referenced by `current_trigger_event`. Distinct from
        // `EventContextSourceManaValue` which reads the printed mana value —
        // the two differ for X spells, alternative costs, and cost reduction.
        QuantityRef::ManaSpentOnTriggeringSpell => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| state.objects.get(&id))
            .map(|obj| u32_to_i32_saturating(obj.mana_spent_to_cast_amount))
            .unwrap_or(0),
        // CR 601.2h: Total mana actually spent to cast the ability's source
        // object. Used by spell effects that reference their own cost at
        // resolution time (Molten Note). The `mana_spent_to_cast_amount`
        // field persists through resolution (not cleared by
        // `clear_transient_cast_state`). Resolves against the entering object
        // when in an ETB-replacement context, else the static source.
        QuantityRef::ManaSpentOnSelf => state
            .objects
            .get(&ctx.self_object())
            .map(|obj| u32_to_i32_saturating(obj.mana_spent_to_cast_amount))
            .unwrap_or(0),
        // CR 601.2h + CR 202.2: Number of distinct colors of mana spent to cast
        // the "self" object. Resolves against the entering object when in an
        // ETB-replacement context (threaded by `extract_etb_counters`), else
        // the static source. Reads `GameObject::colors_spent_to_cast`, which
        // is populated by `pay_mana_cost` during casting and survives until
        // `process_triggers` clears it after trigger collection.
        QuantityRef::ColorsSpentOnSelf => state
            .objects
            .get(&ctx.self_object())
            .map(|obj| usize_to_i32_saturating(obj.colors_spent_to_cast.distinct_colors()))
            .unwrap_or(0),
        // CR 903.4 + CR 903.4f: Number of distinct colors in the controller's
        // commander(s)' combined color identity. Returns 0 when the controller
        // has no commander (per CR 903.4f: "that quality is undefined if that
        // player doesn't have a commander"). War Room's pay-life cost reads
        // this; an undefined identity pays 0 life (and per Scryfall ruling,
        // the ability is still activatable).
        QuantityRef::ColorsInCommandersColorIdentity => usize_to_i32_saturating(
            super::commander::commander_color_identity(state, controller).len(),
        ),
        // CR 106.1 + CR 109.1: Count distinct colors (W/U/B/R/G) among permanents
        // matching the filter. "Gold"/"multicolor"/"colorless" are not colors, so
        // each ManaColor contributes at most once per colored permanent.
        QuantityRef::DistinctColorsAmongPermanents { filter } => {
            let zone = filter
                .extract_in_zone()
                .unwrap_or(crate::types::zones::Zone::Battlefield);
            let mut seen: HashSet<ManaColor> = HashSet::new();
            for &id in crate::game::targeting::zone_object_ids(state, zone).iter() {
                if !matches_target_filter(state, id, filter, &filter_ctx) {
                    continue;
                }
                if let Some(obj) = state.objects.get(&id) {
                    for color in &obj.color {
                        seen.insert(*color);
                    }
                }
            }
            usize_to_i32_saturating(seen.len())
        }
        // CR 305.6: Count distinct basic land types among lands the controller controls.
        QuantityRef::BasicLandTypeCount => {
            let basic_subtypes = ["Plains", "Island", "Swamp", "Mountain", "Forest"];
            let mut found = HashSet::new();
            for &id in state.battlefield.iter() {
                if let Some(obj) = state.objects.get(&id) {
                    if obj.controller == controller
                        && obj.card_types.core_types.contains(&CoreType::Land)
                    {
                        for subtype in &basic_subtypes {
                            if obj.card_types.subtypes.iter().any(|s| s == subtype) {
                                found.insert(*subtype);
                            }
                        }
                    }
                }
            }
            usize_to_i32_saturating(found.len())
        }
        // CR 117.1: Count spells cast this turn by the controller, optionally filtered.
        QuantityRef::SpellsCastThisTurn { ref filter } => {
            let spells = state.spells_cast_this_turn_by_player.get(&controller);
            match spells {
                None => 0,
                Some(list) => match filter {
                    None => usize_to_i32_saturating(list.len()),
                    Some(filter) => usize_to_i32_saturating(
                        list.iter()
                            .filter(|record| {
                                spell_record_matches_filter(record, filter, controller)
                            })
                            .count(),
                    ),
                },
            }
        }
        // Count permanents matching filter that entered the battlefield this turn.
        // Uses `entered_battlefield_turn` field on GameObject.
        QuantityRef::EnteredThisTurn { ref filter } => usize_to_i32_saturating(
            state
                .objects
                .values()
                .filter(|o| {
                    o.zone == crate::types::zones::Zone::Battlefield
                        && o.entered_battlefield_turn == Some(state.turn_number)
                        && matches_target_filter(state, o.id, filter, &filter_ctx)
                })
                .count(),
        ),
        // CR 710.2: Crimes committed this turn — uses tracked counter on player.
        QuantityRef::CrimesCommittedThisTurn => {
            player.map_or(0, |p| u32_to_i32_saturating(p.crimes_committed_this_turn))
        }
        // Life gained this turn — uses tracked counter on player.
        QuantityRef::LifeGainedThisTurn => {
            player.map_or(0, |p| u32_to_i32_saturating(p.life_gained_this_turn))
        }
        // CR 400.7: Count of permanents controlled by player that left the battlefield this turn.
        QuantityRef::PermanentsLeftBattlefieldThisTurn => usize_to_i32_saturating(
            state
                .zone_changes_this_turn
                .iter()
                .filter(|r| r.from_zone == Some(Zone::Battlefield) && r.controller == controller)
                .count(),
        ),
        // CR 400.7: Count of nonland permanents (any controller) that left the battlefield this turn.
        QuantityRef::NonlandPermanentsLeftBattlefieldThisTurn => usize_to_i32_saturating(
            state
                .zone_changes_this_turn
                .iter()
                .filter(|r| {
                    r.from_zone == Some(Zone::Battlefield)
                        && !r.core_types.contains(&CoreType::Land)
                })
                .count(),
        ),
        // CR 500: Cumulative turns taken by this player.
        QuantityRef::TurnsTaken => player.map_or(0, |p| u32_to_i32_saturating(p.turns_taken)),
        // Chosen number stored on the source object via ChosenAttribute::Number.
        QuantityRef::ChosenNumber => state
            .objects
            .get(&source_id)
            .and_then(|obj| {
                obj.chosen_attributes.iter().find_map(|a| match a {
                    crate::types::ability::ChosenAttribute::Number(n) => Some(*n as i32),
                    _ => None,
                })
            })
            .unwrap_or(0),
        // CR 700.7: Count creatures that died (battlefield → graveyard) this turn.
        QuantityRef::CreaturesDiedThisTurn => usize_to_i32_saturating(
            state
                .zone_changes_this_turn
                .iter()
                .filter(|r| {
                    r.core_types.contains(&CoreType::Creature)
                        && r.from_zone == Some(Zone::Battlefield)
                        && r.to_zone == Zone::Graveyard
                })
                .count(),
        ),
        // CR 508.1a: Whether the controller declared attackers this turn.
        QuantityRef::AttackedThisTurn => {
            if state.players_attacked_this_turn.contains(&controller) {
                1
            } else {
                0
            }
        }
        // CR 603.4: Whether the controller descended this turn.
        QuantityRef::DescendedThisTurn => {
            if player.is_some_and(|p| p.descended_this_turn) {
                1
            } else {
                0
            }
        }
        // CR 117.1: Total spells cast last turn (by any player).
        QuantityRef::SpellsCastLastTurn => state.spells_cast_last_turn.map_or(0, i32::from),
        // CR 119.3: Total life lost by opponents this turn.
        QuantityRef::OpponentLifeLostThisTurn => state
            .players
            .iter()
            .filter(|p| p.id != controller)
            .map(|p| u32_to_i32_saturating(p.life_lost_this_turn))
            .sum(),
        // CR 119.3 + CR 603.4: Maximum life lost this turn across all players
        // (controller and opponents). Used by "if a player lost N or more life
        // this turn" intervening-if clauses (Y'shtola, Knight of the Ebon
        // Legion). Per-player max, not sum — "a player lost N+" means "some
        // single player has individually lost ≥ N".
        QuantityRef::MaxLifeLostThisTurnAcrossPlayers => state
            .players
            .iter()
            .map(|p| u32_to_i32_saturating(p.life_lost_this_turn))
            .max()
            .unwrap_or(0),
        // CR 122.1: Whether the controller added any counter to any permanent this turn.
        QuantityRef::CounterAddedThisTurn => {
            if state
                .players_who_added_counter_this_turn
                .contains(&controller)
            {
                1
            } else {
                0
            }
        }
        // CR 701.9 + CR 603.4: Whether any opponent of the controller discarded
        // a card this turn. Mirrors OpponentLifeLostThisTurn semantics — scans
        // the per-turn discard set for any player != controller.
        QuantityRef::OpponentDiscardedCardThisTurn => {
            if state
                .players_who_discarded_card_this_turn
                .iter()
                .any(|&p| p != controller)
            {
                1
            } else {
                0
            }
        }
        // CR 119.3: Maximum life total among opponents.
        QuantityRef::OpponentLifeTotal => state
            .players
            .iter()
            .filter(|p| p.id != controller)
            .map(|p| p.life)
            .max()
            .unwrap_or(0),
        // CR 402.1: Maximum hand size among opponents.
        QuantityRef::OpponentHandSize => state
            .players
            .iter()
            .filter(|p| p.id != controller)
            .map(|p| usize_to_i32_saturating(p.hand.len()))
            .max()
            .unwrap_or(0),
        // CR 309.7: Number of dungeons the controller has completed.
        QuantityRef::DungeonsCompleted => state
            .dungeon_progress
            .get(&controller)
            .map_or(0, |p| usize_to_i32_saturating(p.completed.len())),
        // CR 107.3m: The X paid when the source was cast. Stashed on the object
        // by `finalize_cast` so it survives stack → battlefield. Falls back to
        // the resolving ability's `chosen_x` (for stack-resolution contexts
        // where the object hasn't landed on the battlefield yet).
        QuantityRef::CostXPaid => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.cost_x_paid)
            .map(u32_to_i32_saturating)
            .or_else(|| chosen_x.map(u32_to_i32_saturating))
            .unwrap_or(0),
        // CR 603.10a + CR 603.6e: Count attachments present on the leaving object
        // at zone-change time (look-back). Reads the `attachments` snapshot on
        // the `ZoneChanged` event in `current_trigger_event`, filtered by kind
        // and optional controller.
        QuantityRef::AttachmentsOnLeavingObject {
            kind,
            controller: ctrl,
        } => {
            use crate::types::events::GameEvent;
            let Some(ev) = state.current_trigger_event.as_ref() else {
                return 0;
            };
            let GameEvent::ZoneChanged { record, .. } = ev else {
                return 0;
            };
            usize_to_i32_saturating(
                record
                    .attachments
                    .iter()
                    .filter(|snap| snap.kind == *kind)
                    .filter(|snap| match ctrl {
                        None => true,
                        Some(ControllerRef::You) => snap.controller == controller,
                        Some(ControllerRef::Opponent) => snap.controller != controller,
                        Some(ControllerRef::TargetPlayer) => ability
                            .and_then(|a| {
                                a.targets.iter().find_map(|t| match t {
                                    crate::types::ability::TargetRef::Player(pid) => Some(*pid),
                                    crate::types::ability::TargetRef::Object(_) => None,
                                })
                            })
                            .is_some_and(|pid| pid == snap.controller),
                    })
                    .count(),
            )
        }
    }
}

/// Check if an object matches a set of type filters for zone card counting.
/// Empty `card_types` means all cards match.
fn matches_zone_card_filter(
    state: &GameState,
    obj_id: ObjectId,
    card_types: &[TypeFilter],
) -> bool {
    if card_types.is_empty() {
        return true;
    }
    state
        .objects
        .get(&obj_id)
        .is_some_and(|obj| card_types.iter().any(|tf| type_filter_matches(tf, obj)))
}

/// Return an iterator over players matching the given `CountScope`.
fn scoped_players<'a>(
    state: &'a GameState,
    scope: &'a CountScope,
    controller: PlayerId,
) -> impl Iterator<Item = &'a crate::types::player::Player> {
    state.players.iter().filter(move |p| match scope {
        CountScope::Controller => p.id == controller,
        CountScope::All => true,
        CountScope::Opponents => p.id != controller,
    })
}

/// Count players matching a PlayerFilter relative to the controller.
pub(crate) fn resolve_player_count(
    state: &GameState,
    filter: &PlayerFilter,
    controller: PlayerId,
    source_id: ObjectId,
) -> i32 {
    usize_to_i32_saturating(
        state
            .players
            .iter()
            .filter(|p| {
                !p.is_eliminated
                    && match filter {
                        PlayerFilter::Controller => p.id == controller,
                        PlayerFilter::Opponent => p.id != controller,
                        PlayerFilter::OpponentLostLife => {
                            p.id != controller && p.life_lost_this_turn > 0
                        }
                        PlayerFilter::OpponentGainedLife => {
                            p.id != controller && p.life_gained_this_turn > 0
                        }
                        PlayerFilter::All => true,
                        PlayerFilter::HighestSpeed => {
                            let highest_speed = state
                                .players
                                .iter()
                                .map(|player| effective_speed(state, player.id))
                                .max()
                                .unwrap_or(0);
                            effective_speed(state, p.id) == highest_speed
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
                            .and_then(|e| {
                                crate::game::targeting::extract_player_from_event(e, state)
                            })
                            .is_some_and(|pid| pid == p.id),
                        // CR 120.3 + CR 603.2c: Each opponent other than the triggering opponent.
                        // Falls back to plain Opponent semantics when no trigger event is in scope.
                        PlayerFilter::OpponentOtherThanTriggering => {
                            if p.id == controller {
                                false
                            } else {
                                let triggering =
                                    state.current_trigger_event.as_ref().and_then(|e| {
                                        crate::game::targeting::extract_player_from_event(e, state)
                                    });
                                triggering.is_none_or(|pid| pid != p.id)
                            }
                        }
                    }
            })
            .count(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AggregateFunction, ControllerRef, Effect, FilterProp, ObjectProperty, TargetFilter,
        TypeFilter, TypedFilter,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::counter::CounterType;
    use crate::types::game_state::{ExileLink, ExileLinkKind, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::zones::Zone;
    use crate::types::SpellCastRecord;

    /// CR 122.1: PlayerCounter resolves controller scope from the named player.
    /// Opponents/All sums the kind across the matching scope (Toph's "you have"
    /// is Controller; cousin patterns like "each opponent has" sum opponents).
    #[test]
    fn resolve_quantity_player_counter_experience_controller_and_sums() {
        use crate::types::player::PlayerCounterKind;

        let mut state = GameState::new_two_player(42);
        state.players[0]
            .player_counters
            .insert(PlayerCounterKind::Experience, 3);
        state.players[1]
            .player_counters
            .insert(PlayerCounterKind::Experience, 5);

        let controller_expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCounter {
                kind: PlayerCounterKind::Experience,
                scope: CountScope::Controller,
            },
        };
        assert_eq!(
            resolve_quantity(&state, &controller_expr, PlayerId(0), ObjectId(0)),
            3
        );

        let opponents_expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCounter {
                kind: PlayerCounterKind::Experience,
                scope: CountScope::Opponents,
            },
        };
        assert_eq!(
            resolve_quantity(&state, &opponents_expr, PlayerId(0), ObjectId(0)),
            5
        );

        let all_expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCounter {
                kind: PlayerCounterKind::Experience,
                scope: CountScope::All,
            },
        };
        assert_eq!(
            resolve_quantity(&state, &all_expr, PlayerId(0), ObjectId(0)),
            8
        );
    }

    #[test]
    fn resolve_quantity_colors_in_commanders_color_identity() {
        // CR 903.4 + CR 903.4f: When no commander exists the quality is
        // undefined and resolves to 0. When commanders exist the resolver
        // returns the size of the combined color identity.
        use crate::types::format::FormatConfig;
        use crate::types::mana::ManaCost;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ColorsInCommandersColorIdentity,
        };
        // No commander yet → 0.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(0)), 0);

        // Build a 3-color commander (W/U/B) and verify the resolver returns 3.
        let cmd_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Kaalia".to_string(),
            Zone::Command,
        );
        {
            let obj = state.objects.get_mut(&cmd_id).unwrap();
            obj.is_commander = true;
            obj.color = vec![ManaColor::White, ManaColor::Blue, ManaColor::Black];
            obj.mana_cost = ManaCost::NoCost;
        }
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(0)), 3);

        // Other player (no commander of their own) still reports 0.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(0)), 0);
    }

    /// CR 201.2 + CR 603.4: distinct-name count for Field of the Dead.
    /// Two lands sharing a name count once; overall = # of unique names.
    #[test]
    fn resolve_quantity_object_count_distinct_names() {
        let mut state = GameState::new_two_player(42);
        for (name, count) in &[("Plains", 3), ("Island", 2), ("Field of the Dead", 1)] {
            for _ in 0..*count {
                let id = create_object(
                    &mut state,
                    CardId(100),
                    PlayerId(0),
                    (*name).to_string(),
                    Zone::Battlefield,
                );
                state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];
            }
        }
        // Plus one opponent Plains — must not count because filter is controller=You.
        let opp_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Plains".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_id)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountDistinctNames { filter },
        };
        // 3 distinct names controlled by P0: Plains, Island, Field of the Dead.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(0)), 3);
        // P1's POV: only the one opponent Plains would be theirs, so 1.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(0)), 1);
    }

    #[test]
    fn resolve_quantity_fixed_returns_value() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Fixed { value: 3 };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
    }

    /// CR 107.3m + CR 107.3: Primordial Hydra cast for {X}{G}{G} with X=3 enters
    /// with 3 counters; Primo cast for {X}{G}{U} with X=4 enters with
    /// `Multiply(2, CostXPaid)` = 8 counters. The ETB-counters resolution path
    /// reads the entering permanent's own `cost_x_paid`, so the tree walk
    /// through `Multiply` applies the factor verbatim.
    #[test]
    fn resolve_quantity_cost_x_paid_composes_with_multiply() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Primo".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().cost_x_paid = Some(4);

        let bare = QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        };
        assert_eq!(resolve_quantity(&state, &bare, PlayerId(0), obj_id), 4);

        let twice = QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            }),
        };
        assert_eq!(resolve_quantity(&state, &twice, PlayerId(0), obj_id), 8);

        let half_up = QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            }),
            rounding: crate::types::ability::RoundingMode::Up,
        };
        // half of 4 = 2 (exact).
        assert_eq!(resolve_quantity(&state, &half_up, PlayerId(0), obj_id), 2);

        // X=5 → half rounded up = 3.
        state.objects.get_mut(&obj_id).unwrap().cost_x_paid = Some(5);
        assert_eq!(resolve_quantity(&state, &half_up, PlayerId(0), obj_id), 3);
    }

    // CR 603.10a + CR 603.6e: Hateful Eidolon's "for each Aura you controlled that
    // was attached to it" resolves against the ZoneChangeRecord's attachment
    // snapshot. Three auras attached (two controlled by P0, one by P1); P0's
    // resolver sees 2, P1's sees 1.
    #[test]
    fn resolve_quantity_attachments_on_leaving_object_filters_by_controller() {
        use crate::types::ability::AttachmentKind;
        use crate::types::events::GameEvent;
        use crate::types::game_state::{AttachmentSnapshot, ZoneChangeRecord};

        let mut state = GameState::new_two_player(42);
        let dying_id = ObjectId(200);
        let record = ZoneChangeRecord {
            attachments: vec![
                AttachmentSnapshot {
                    object_id: ObjectId(301),
                    controller: PlayerId(0),
                    kind: AttachmentKind::Aura,
                },
                AttachmentSnapshot {
                    object_id: ObjectId(302),
                    controller: PlayerId(0),
                    kind: AttachmentKind::Aura,
                },
                AttachmentSnapshot {
                    object_id: ObjectId(303),
                    controller: PlayerId(1),
                    kind: AttachmentKind::Aura,
                },
            ],
            ..ZoneChangeRecord::test_minimal(dying_id, Some(Zone::Battlefield), Zone::Graveyard)
        };
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: dying_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(record),
        });

        let expr_you = QuantityExpr::Ref {
            qty: QuantityRef::AttachmentsOnLeavingObject {
                kind: AttachmentKind::Aura,
                controller: Some(ControllerRef::You),
            },
        };
        let expr_any = QuantityExpr::Ref {
            qty: QuantityRef::AttachmentsOnLeavingObject {
                kind: AttachmentKind::Aura,
                controller: None,
            },
        };
        // "You" = P0 → 2 aura snapshots.
        assert_eq!(
            resolve_quantity(&state, &expr_you, PlayerId(0), ObjectId(1)),
            2
        );
        // P1's vantage → "you" = P1 → 1 aura snapshot.
        assert_eq!(
            resolve_quantity(&state, &expr_you, PlayerId(1), ObjectId(1)),
            1
        );
        // Unfiltered → all 3.
        assert_eq!(
            resolve_quantity(&state, &expr_any, PlayerId(0), ObjectId(1)),
            3
        );
    }

    // CR 603.10a: When no zone-change event is in scope, the quantity resolves to 0
    // (graceful fallback — cannot count what we don't have a snapshot of).
    #[test]
    fn resolve_quantity_attachments_on_leaving_object_without_event_returns_zero() {
        use crate::types::ability::AttachmentKind;
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::AttachmentsOnLeavingObject {
                kind: AttachmentKind::Aura,
                controller: None,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    #[test]
    fn resolve_quantity_hand_size() {
        let mut state = GameState::new_two_player(42);
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Hand,
            );
        }
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::HandSize,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            4
        );
    }

    #[test]
    fn resolve_quantity_object_count_creatures_you_control() {
        let mut state = GameState::new_two_player(42);
        // Add 3 creatures for player 0
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        // Add 1 creature for player 1 (should not count)
        let opp = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        // Source is controlled by player 0
        let source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 3);
    }

    #[test]
    fn object_count_with_in_zone_graveyard() {
        // Eddymurk Crab pattern: count instants and sorceries in your graveyard.
        use crate::types::ability::FilterProp;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        // Add 2 instants and 1 sorcery to player 0's graveyard
        for (i, name) in ["Instant A", "Instant B", "Sorcery C"].iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(i as u64),
                PlayerId(0),
                name.to_string(),
                Zone::Graveyard,
            );
            let core_type = if name.starts_with("Instant") {
                CoreType::Instant
            } else {
                CoreType::Sorcery
            };
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(core_type);
        }

        // Add a creature to graveyard (should NOT count)
        let creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Add an instant on battlefield (should NOT count — wrong zone)
        let bf_instant = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "BF Instant".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bf_instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        // Filter: Or(Instant+InZone:Graveyard, Sorcery+InZone:Graveyard)
        let instant_filter = TypedFilter {
            type_filters: vec![TypeFilter::Instant],
            controller: Some(ControllerRef::You),
            properties: vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }],
        };
        let sorcery_filter = TypedFilter {
            type_filters: vec![TypeFilter::Sorcery],
            controller: Some(ControllerRef::You),
            properties: vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }],
        };
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(instant_filter),
                TargetFilter::Typed(sorcery_filter),
            ],
        };
        // Verify extract_in_zone returns Graveyard
        assert_eq!(filter.extract_in_zone(), Some(Zone::Graveyard));

        // Verify zone_object_ids finds graveyard objects
        let gy_ids = crate::game::targeting::zone_object_ids(&state, Zone::Graveyard);
        assert_eq!(
            gy_ids.len(),
            4,
            "expected 4 objects in graveyard (3 spells + 1 creature)"
        );

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        };

        // Should count 3 (2 instants + 1 sorcery in graveyard)
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 3);
    }

    #[test]
    fn counters_on_objects_sums_matching_counters_not_permanents() {
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);

        let land_with_two = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Animated Land".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_with_two).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.counters.insert(CounterType::Plus1Plus1, 2);
        }

        let other_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&other_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        for i in 0..10 {
            let id = create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("Permanent {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOnObjects {
                counter_type: Some("P1P1".to_string()),
                filter: TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 2);
    }

    #[test]
    fn distinct_card_types_exiled_by_source_counts_linked_types_only() {
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let linked_artifact = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Linked Artifact".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&linked_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let linked_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Linked Creature".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&linked_creature)
            .unwrap()
            .card_types
            .core_types
            .extend([CoreType::Creature, CoreType::Artifact]);

        let other_source = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Other Source".to_string(),
            Zone::Battlefield,
        );
        let unlinked = create_object(
            &mut state,
            CardId(5),
            PlayerId(1),
            "Unlinked Instant".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&unlinked)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        state.exile_links.push(ExileLink {
            source_id: source,
            exiled_id: linked_artifact,
            kind: ExileLinkKind::TrackedBySource,
        });
        state.exile_links.push(ExileLink {
            source_id: source,
            exiled_id: linked_creature,
            kind: ExileLinkKind::TrackedBySource,
        });
        state.exile_links.push(ExileLink {
            source_id: other_source,
            exiled_id: unlinked,
            kind: ExileLinkKind::TrackedBySource,
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::DistinctCardTypesExiledBySource,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 2);
    }

    // CR 406.6 + CR 607.1: CardsExiledBySource counts distinct exiled objects
    // linked to the source, ignoring links to other sources and cards that have
    // left exile.
    #[test]
    fn cards_exiled_by_source_counts_linked_cards_in_exile() {
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let other_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );

        // Three cards linked to source: two in Exile, one returned to Graveyard.
        let mut linked_ids = Vec::new();
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                format!("Exiled {i}"),
                Zone::Exile,
            );
            state.exile_links.push(ExileLink {
                source_id: source,
                exiled_id: id,
                kind: ExileLinkKind::TrackedBySource,
            });
            linked_ids.push(id);
        }
        // Simulate the third card leaving exile (e.g. returned via a linked ability).
        state.objects.get_mut(&linked_ids[2]).unwrap().zone = Zone::Graveyard;

        // Link to a different source should not count.
        let other_exiled = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Other Linked".to_string(),
            Zone::Exile,
        );
        state.exile_links.push(ExileLink {
            source_id: other_source,
            exiled_id: other_exiled,
            kind: ExileLinkKind::TrackedBySource,
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CardsExiledBySource,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 2);
    }

    #[test]
    fn resolve_quantity_player_count_opponent_lost_life() {
        let mut state = GameState::new_two_player(42);
        // Opponent (player 1) lost life this turn
        state.players[1].life_lost_this_turn = 3;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_player_count_opponent_lost_life_none_lost() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    /// CR 119.3 + CR 603.4: `MaxLifeLostThisTurnAcrossPlayers` returns the
    /// maximum life-loss across all players (controller + opponents),
    /// not the sum. Three players' losses [2, 5, 1] → max = 5.
    /// Critical: 2 + 5 + 1 = 8 would falsely satisfy a >= 8 threshold,
    /// while max = 5 correctly fails it.
    #[test]
    fn resolve_quantity_max_life_lost_this_turn_across_players() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        state.players[0].life_lost_this_turn = 2;
        state.players[1].life_lost_this_turn = 5;
        state.players[2].life_lost_this_turn = 1;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::MaxLifeLostThisTurnAcrossPlayers,
        };
        // Resolves identically regardless of which player is the controller —
        // the variant scans all players, not just opponents.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 5);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(1)), 5);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(2), ObjectId(1)), 5);
    }

    /// CR 119.3: When no player has lost life this turn, the resolver
    /// returns 0 (not panics on empty `.max()`).
    #[test]
    fn resolve_quantity_max_life_lost_this_turn_none_lost() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::MaxLifeLostThisTurnAcrossPlayers,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    #[test]
    fn resolve_quantity_player_count_opponent() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_zone_card_count_matches_subtype_cards() {
        let mut state = GameState::new_two_player(42);

        for i in 0..3u64 {
            let lesson = create_object(
                &mut state,
                CardId(700 + i),
                PlayerId(0),
                format!("Lesson {i}"),
                Zone::Graveyard,
            );
            let obj = state.objects.get_mut(&lesson).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.card_types.subtypes.push("Lesson".to_string());
        }

        let non_lesson = create_object(
            &mut state,
            CardId(710),
            PlayerId(0),
            "Not a Lesson".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&non_lesson)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![TypeFilter::Subtype("Lesson".to_string())],
                scope: CountScope::Controller,
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
    }

    #[test]
    fn resolve_quantity_counters_on_self() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .counters
            .insert(CounterType::Loyalty, 4);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOnSelf {
                counter_type: "loyalty".to_string(),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 4);
    }

    #[test]
    fn resolve_quantity_player_filter_opponent_gained_life() {
        let mut state = GameState::new_two_player(42);
        state.players[1].life_gained_this_turn = 5;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentGainedLife,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn resolve_quantity_player_filter_all() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::All,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 2);
    }

    #[test]
    fn resolve_quantity_spells_cast_this_turn_with_qualified_filter() {
        let mut state = GameState::new_two_player(42);
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![
                SpellCastRecord {
                    core_types: vec![CoreType::Creature],
                    supertypes: vec![Supertype::Legendary],
                    subtypes: vec!["Bird".to_string()],
                    keywords: vec![Keyword::Flying],
                    colors: vec![ManaColor::Blue],
                    mana_value: 3,
                    has_x_in_cost: false,
                },
                SpellCastRecord {
                    core_types: vec![CoreType::Artifact],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 1,
                    has_x_in_cost: false,
                },
            ],
        );

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                filter: Some(TargetFilter::Typed(
                    TypedFilter::creature()
                        .with_type(TypeFilter::Subtype("Bird".to_string()))
                        .properties(vec![
                            FilterProp::WithKeyword {
                                value: Keyword::Flying,
                            },
                            FilterProp::HasSupertype {
                                value: crate::types::card_type::Supertype::Legendary,
                            },
                        ]),
                )),
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    #[test]
    fn half_rounded_up_even() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 20 }),
            rounding: crate::types::ability::RoundingMode::Up,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            10
        );
    }

    #[test]
    fn half_rounded_up_odd() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 7 }),
            rounding: crate::types::ability::RoundingMode::Up,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 4);
    }

    #[test]
    fn half_rounded_down_odd() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 7 }),
            rounding: crate::types::ability::RoundingMode::Down,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
    }

    #[test]
    fn resolve_target_life_total() {
        let state = GameState::new_two_player(42);
        // Player 1 starts at 20 life
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::TargetLifeTotal,
        };
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: expr.clone(),
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(1),
            PlayerId(0),
        );
        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 20);
    }

    #[test]
    fn resolve_self_power() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.power = Some(5);
        obj.toughness = Some(3);
        obj.card_types.core_types.push(CoreType::Creature);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::SelfPower,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);

        let expr_t = QuantityExpr::Ref {
            qty: QuantityRef::SelfToughness,
        };
        assert_eq!(resolve_quantity(&state, &expr_t, PlayerId(0), source), 3);
    }

    #[test]
    fn resolve_aggregate_max_power() {
        use crate::types::ability::AggregateFunction;
        use crate::types::ability::ObjectProperty;

        let mut state = GameState::new_two_player(42);
        // Create creatures with power 2, 5, 3
        for (i, pwr) in [2, 5, 3].iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(*pwr);
            obj.toughness = Some(1);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Max,
                property: ObjectProperty::Power,
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);
    }

    #[test]
    fn resolve_aggregate_sum_power() {
        use crate::types::ability::AggregateFunction;
        use crate::types::ability::ObjectProperty;

        let mut state = GameState::new_two_player(42);
        for (i, pwr) in [2, 5, 3].iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(*pwr);
            obj.toughness = Some(1);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 10);
    }

    #[test]
    fn resolve_aggregate_max_mana_value_in_exile() {
        use crate::types::ability::AggregateFunction;
        use crate::types::ability::ObjectProperty;

        let mut state = GameState::new_two_player(42);
        // Create cards in exile with mana values 3, 7, 2
        for (i, mv) in [3u32, 7, 2].iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("Exiled Card {i}"),
                Zone::Exile,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.mana_cost = crate::types::mana::ManaCost::generic(*mv);
        }
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        // Filter: "cards in exile" — InZone(Exile) property, no controller constraint
        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![],
            controller: None,
            properties: vec![crate::types::ability::FilterProp::InZone { zone: Zone::Exile }],
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Max,
                property: ObjectProperty::ManaValue,
                filter,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 7);
    }

    #[test]
    fn resolve_aggregate_sum_mana_value_of_owned_cards_exiled_by_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        for (card_id, owner, mv) in [
            (31, PlayerId(0), 2u32),
            (32, PlayerId(0), 3),
            (33, PlayerId(1), 4),
        ] {
            let exiled = create_object(
                &mut state,
                CardId(card_id),
                owner,
                format!("Exiled {card_id}"),
                Zone::Exile,
            );
            state.objects.get_mut(&exiled).unwrap().mana_cost =
                crate::types::mana::ManaCost::generic(mv);
            state.exile_links.push(ExileLink {
                source_id: source,
                exiled_id: exiled,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaValue,
                filter: TargetFilter::And {
                    filters: vec![
                        TargetFilter::ExiledBySource,
                        TargetFilter::Typed(TypedFilter::default().properties(vec![
                            FilterProp::Owned {
                                controller: ControllerRef::You,
                            },
                        ])),
                    ],
                },
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), source), 4);
    }

    #[test]
    fn resolve_aggregate_sum_mana_value_of_owned_cards_exiled_by_source_from_ltb_snapshot() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Source".to_string(),
            Zone::Graveyard,
        );
        let exiled_a = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Exiled 31".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_a).unwrap().mana_cost =
            crate::types::mana::ManaCost::generic(2);
        let exiled_b = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Exiled 32".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_b).unwrap().mana_cost =
            crate::types::mana::ManaCost::generic(3);
        let exiled_c = create_object(
            &mut state,
            CardId(33),
            PlayerId(1),
            "Exiled 33".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_c).unwrap().mana_cost =
            crate::types::mana::ManaCost::generic(4);
        state.current_trigger_event = Some(crate::types::events::GameEvent::ZoneChanged {
            object_id: source,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                linked_exile_snapshot: vec![
                    crate::types::game_state::LinkedExileSnapshot {
                        exiled_id: exiled_a,
                        owner: PlayerId(0),
                        mana_value: 2,
                    },
                    crate::types::game_state::LinkedExileSnapshot {
                        exiled_id: exiled_b,
                        owner: PlayerId(0),
                        mana_value: 3,
                    },
                    crate::types::game_state::LinkedExileSnapshot {
                        exiled_id: exiled_c,
                        owner: PlayerId(1),
                        mana_value: 4,
                    },
                ],
                ..ZoneChangeRecord::test_minimal(source, Some(Zone::Battlefield), Zone::Graveyard)
            }),
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaValue,
                filter: TargetFilter::And {
                    filters: vec![
                        TargetFilter::ExiledBySource,
                        TargetFilter::Typed(TypedFilter::default().properties(vec![
                            FilterProp::Owned {
                                controller: ControllerRef::You,
                            },
                        ])),
                    ],
                },
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), source), 4);
    }

    #[test]
    fn resolve_multiply() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Multiply {
            factor: 3,
            inner: Box::new(QuantityExpr::Fixed { value: 4 }),
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            12
        );
    }

    #[test]
    fn resolve_event_context_amount_from_damage() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 5,
            is_combat: false,
            excess: 0,
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 5);
    }

    #[test]
    fn resolve_event_context_amount_none_returns_zero() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    #[test]
    fn resolve_event_context_source_power_live_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().power = Some(4);
        state.objects.get_mut(&source).unwrap().toughness = Some(3);
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(PlayerId(1)),
            amount: 4,
            is_combat: true,
            excess: 0,
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourcePower,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            4
        );
    }

    #[test]
    fn resolve_event_context_source_power_lki_fallback() {
        use crate::types::game_state::LKISnapshot;
        let mut state = GameState::new_two_player(42);
        let dead_id = ObjectId(42);
        // Object is gone from state.objects but has LKI entry
        state.lki_cache.insert(
            dead_id,
            LKISnapshot {
                name: String::new(),
                power: Some(6),
                toughness: Some(5),
                mana_value: 3,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                counters: HashMap::new(),
            },
        );
        state.current_trigger_event =
            Some(crate::types::events::GameEvent::CreatureDestroyed { object_id: dead_id });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextSourcePower,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            6
        );
    }

    #[test]
    fn lki_cleared_on_advance_phase() {
        use crate::types::game_state::LKISnapshot;
        let mut state = GameState::new_two_player(42);
        state.lki_cache.insert(
            ObjectId(1),
            LKISnapshot {
                name: String::new(),
                power: Some(3),
                toughness: Some(3),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                counters: HashMap::new(),
            },
        );
        assert!(!state.lki_cache.is_empty());
        let mut events = Vec::new();
        crate::game::turns::advance_phase(&mut state, &mut events);
        assert!(state.lki_cache.is_empty());
    }

    #[test]
    fn resolve_multiply_negative() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Multiply {
            factor: -1,
            inner: Box::new(QuantityExpr::Fixed { value: 5 }),
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            -5
        );
    }

    /// CR 107.3a + CR 601.2b: `ObjectCount` with an inner filter that references X
    /// must resolve X against the resolving ability's `chosen_x`. Regression for
    /// the latent bug where `resolve_ref` passed bare context to the filter matcher
    /// (X → 0) — only reachable through `resolve_quantity_with_targets`.
    #[test]
    fn object_count_filter_resolves_x_against_chosen_x() {
        use crate::types::ability::{QuantityExpr, QuantityRef, ResolvedAbility};
        use crate::types::mana::ManaCost;
        let mut state = GameState::new_two_player(42);
        // Build three on-battlefield creatures of varying CMCs.
        for (i, cmc) in [(1u64, 1u32), (2, 3), (3, 7)].into_iter() {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("CMC {}", cmc),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(cmc);
        }

        let inner_filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: inner_filter,
            },
        };
        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        ability.chosen_x = Some(3);

        // With X=3, only CMC-1 and CMC-3 match — count is 2.
        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 2);
    }
}
