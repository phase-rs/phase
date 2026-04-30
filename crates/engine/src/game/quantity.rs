//! Dynamic quantity resolution for QuantityExpr values.
//!
//! Evaluates QuantityRef variants (ObjectCount, PlayerCount, CountersOnSelf, etc.)
//! against the current game state at resolution time. Used by effect resolvers
//! to support "for each [X]" patterns on Draw, DealDamage, GainLife, LoseLife, Mill.

use std::collections::HashSet;

use crate::game::arithmetic::{u32_to_i32_saturating, usize_to_i32_saturating};
use crate::game::filter::{
    matches_target_filter, matches_target_filter_on_counter_added_record,
    matches_target_filter_on_zone_change_record, spell_record_matches_filter, type_filter_matches,
    FilterContext,
};
use crate::game::speed::effective_speed;
use crate::types::ability::{
    AggregateFunction, CardTypeSetSource, CastManaObjectScope, CastManaSpentMetric, ControllerRef,
    CountScope, FilterProp, ObjectProperty, ObjectScope, PlayerFilter, PlayerScope, QuantityExpr,
    QuantityRef, ResolvedAbility, RoundingMode, TargetFilter, TargetRef, TypeFilter, ZoneRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::{parse_counter_type, CounterMatch, CounterType};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::player::PlayerId;

/// Scope information for quantity resolution.
///
/// Some `QuantityRef` variants need to distinguish between "the static ability
/// source" and "the object entering the battlefield" — e.g., Wildgrowth
/// Archaic's self-scoped spent-mana quantity during an ETB replacement refers to the
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
    /// CR 613.4c: The per-recipient binding for "<subject> gets +N/+M for
    /// each X attached to it" Aura/Equipment statics. Set by the layer
    /// evaluator when the dynamic modification's filter contains
    /// `FilterProp::AttachedToRecipient`; `None` otherwise.
    pub recipient: Option<ObjectId>,
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
            recipient: None,
        },
    )
}

/// CR 613.4c: Resolve a `QuantityExpr` for a layer-evaluated dynamic
/// modification whose quantity references the affected object ("attached to
/// it", "its name", "its colors", etc.). The recipient is the affected object
/// in the layer evaluator's loop, not necessarily the static's source.
pub fn resolve_quantity_with_recipient(
    state: &GameState,
    expr: &QuantityExpr,
    controller: PlayerId,
    source_id: ObjectId,
    recipient_id: ObjectId,
) -> i32 {
    resolve_quantity_with_ctx(
        state,
        expr,
        controller,
        QuantityContext {
            entering: None,
            source: source_id,
            recipient: Some(recipient_id),
        },
    )
}

/// True when the QuantityExpr needs a per-object recipient binding to resolve
/// an anaphoric quantity such as "for each Aura attached to it" or "for each
/// word in its name."
pub(crate) fn quantity_expr_uses_recipient(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Ref { qty } => match qty {
            QuantityRef::HandSize {
                player: PlayerScope::RecipientController,
            }
            | QuantityRef::LifeTotal {
                player: PlayerScope::RecipientController,
            }
            | QuantityRef::LifeLostThisTurn {
                player: PlayerScope::RecipientController,
            }
            | QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::RecipientController,
            }
            | QuantityRef::PartySize {
                player: PlayerScope::RecipientController,
            } => true,
            QuantityRef::ObjectCount { filter }
            | QuantityRef::ObjectCountDistinctNames { filter }
            | QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::Objects { filter },
            }
            | QuantityRef::ManaSpentToCast {
                metric:
                    CastManaSpentMetric::FromSource {
                        source_filter: filter,
                    },
                ..
            } => filter_uses_recipient(filter),
            QuantityRef::ObjectColorCount {
                scope: ObjectScope::Recipient,
            }
            | QuantityRef::ObjectNameWordCount {
                scope: ObjectScope::Recipient,
            }
            | QuantityRef::ManaSymbolsInManaCost {
                scope: ObjectScope::Recipient,
                ..
            } => true,
            _ => false,
        },
        QuantityExpr::HalfRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_expr_uses_recipient(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(quantity_expr_uses_recipient),
        QuantityExpr::UpTo { max } => quantity_expr_uses_recipient(max),
    }
}

fn filter_uses_recipient(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(filter_prop_uses_recipient),
        TargetFilter::Not { filter: inner } => filter_uses_recipient(inner),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_uses_recipient)
        }
        _ => false,
    }
}

fn filter_prop_uses_recipient(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::AttachedToRecipient | FilterProp::Another => true,
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_uses_recipient),
        FilterProp::SharesQuality {
            reference: Some(reference),
            ..
        } => matches!(reference.as_ref(), TargetFilter::ParentTarget),
        _ => false,
    }
}

/// Resolve a QuantityExpr with an explicit `QuantityContext` so variants like
/// self-scoped spent-mana quantities can distinguish entering-object scope from static-source
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
        other => fold_compose(other, |inner| {
            resolve_quantity_with_ctx(state, inner, controller, ctx)
        }),
    }
}

/// Compose recursively-resolved inner values for the non-leaf
/// `QuantityExpr` variants (`HalfRounded`, `Offset`, `Multiply`, `Sum`).
/// All four resolver entry points share this logic; only the leaf arms
/// (`Fixed`, `Ref`) differ in context handling. `recurse` is a closure
/// the caller supplies that re-enters its own resolver with the inner
/// expression.
///
/// Panics if called with a leaf variant — callers must dispatch leaves
/// before delegating here.
fn fold_compose(expr: &QuantityExpr, recurse: impl Fn(&QuantityExpr) -> i32) -> i32 {
    match expr {
        QuantityExpr::HalfRounded { inner, rounding } => half_rounded(recurse(inner), *rounding),
        QuantityExpr::Offset { inner, offset } => recurse(inner) + offset,
        QuantityExpr::Multiply { factor, inner } => factor * recurse(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().map(&recurse).sum(),
        // CR 107.1c + CR 608.2d: Generic resolvers see UpTo transparently as
        // its upper bound — the 4 effect-specific resolvers (Draw,
        // Sacrifice, Discard, SearchLibrary) peel the wrapper via
        // `QuantityExpr::peel_up_to` to extract the "may pick fewer" flag
        // before reaching arithmetic. Treating it transparently here keeps
        // legacy serde round-trips correct and makes accidental composition
        // (e.g., `HalfRounded { inner: UpTo { max: ... } }`) collapse to a
        // sensible bound rather than panicking.
        QuantityExpr::UpTo { max } => recurse(max),
        QuantityExpr::Fixed { .. } | QuantityExpr::Ref { .. } => {
            unreachable!("fold_compose called on leaf variant — caller must dispatch leaves first")
        }
    }
}

/// CR 603.4: Resolve a `QuantityExpr` for an intervening-if check using an
/// explicit `trigger_event` override. `state.current_trigger_event` is not
/// populated at trigger-detection time (it is only set at resolution via
/// `stack::resolve_top`), so event-scoped refs like
/// triggering-spell spent-mana refs would otherwise resolve to 0
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

/// Substitute an event-scoped `QuantityRef` using an explicit event, returning `None`
/// when the expression does not reference an event-scoped quantity.
fn resolve_event_scoped_ref(
    state: &GameState,
    expr: &QuantityExpr,
    event: &crate::types::events::GameEvent,
) -> Option<i32> {
    match expr {
        QuantityExpr::Ref {
            qty:
                QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::TriggeringSpell,
                    metric,
                },
        } => {
            let id = crate::game::targeting::extract_source_from_event(event)?;
            resolve_mana_spent_to_cast_metric(
                state,
                id,
                metric,
                &FilterContext::from_source(state, id),
            )
        }
        _ => None,
    }
}

fn resolve_mana_spent_to_cast_metric(
    state: &GameState,
    cast_object: ObjectId,
    metric: &CastManaSpentMetric,
    filter_ctx: &FilterContext<'_>,
) -> Option<i32> {
    let obj = state.objects.get(&cast_object)?;
    Some(match metric {
        CastManaSpentMetric::Total => u32_to_i32_saturating(obj.mana_spent_to_cast_amount),
        CastManaSpentMetric::DistinctColors => {
            usize_to_i32_saturating(obj.colors_spent_to_cast.distinct_colors())
        }
        CastManaSpentMetric::FromSource { source_filter } => usize_to_i32_saturating(
            obj.mana_spent_source_snapshots
                .iter()
                .filter(|snapshot| {
                    crate::game::filter::matches_target_filter_on_lki_snapshot(
                        state,
                        snapshot.source_id,
                        &snapshot.lki,
                        source_filter,
                        filter_ctx,
                    )
                })
                .count(),
        ),
    })
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
                recipient: None,
            },
            &ability.targets,
            ability.chosen_x,
            Some(ability),
        ),
        other => fold_compose(other, |inner| {
            resolve_quantity_with_targets(state, inner, ability)
        }),
    }
}

/// Resolve a QuantityExpr with ability targets/chosen-X plus a per-object
/// recipient binding for `FilterProp::AttachedToRecipient`.
pub(crate) fn resolve_quantity_with_targets_and_recipient(
    state: &GameState,
    expr: &QuantityExpr,
    ability: &ResolvedAbility,
    recipient_id: ObjectId,
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
                recipient: Some(recipient_id),
            },
            &ability.targets,
            ability.chosen_x,
            Some(ability),
        ),
        other => fold_compose(other, |inner| {
            resolve_quantity_with_targets_and_recipient(state, inner, ability, recipient_id)
        }),
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
                recipient: None,
            },
            targets,
            None,
            None,
        ),
        other => fold_compose(other, |inner| {
            resolve_quantity_with_targets_slice(state, inner, controller, source_id, targets)
        }),
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
                recipient: None,
            },
            &[],
            None,
            None,
        ),
        other => fold_compose(other, |inner| {
            resolve_quantity_scoped(state, inner, source_id, scope_player)
        }),
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
    //
    // CR 613.4c: The optional `recipient` from `QuantityContext` flows into
    // `FilterContext::recipient_id` so recipient-relative filter properties
    // resolve against the per-object recipient bound by the layer evaluator.
    let mut filter_ctx = match ability {
        Some(a) => FilterContext::from_ability(a),
        None => FilterContext::from_source_with_controller(source_id, controller),
    };
    filter_ctx.recipient_id = ctx.recipient;
    let player = state.players.iter().find(|p| p.id == controller);
    match qty {
        // CR 402: hand size for the scoped player(s).
        QuantityRef::HandSize { player: scope } => {
            resolve_per_player_scalar(state, *scope, controller, ctx, targets, |p| {
                usize_to_i32_saturating(p.hand.len())
            })
        }
        // CR 119: life total for the scoped player(s).
        QuantityRef::LifeTotal { player: scope } => {
            resolve_per_player_scalar(state, *scope, controller, ctx, targets, |p| p.life)
        }
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
        // CR 118.4 + CR 119.3: Life lost this turn, scoped via PlayerScope (Π-3).
        QuantityRef::LifeLostThisTurn { player } => {
            resolve_per_player_scalar(state, *player, controller, ctx, targets, |p| {
                u32_to_i32_saturating(p.life_lost_this_turn)
            })
        }
        // CR 700.8: Number of creatures in `player`'s party. The maximum
        // assignment of creatures to the four party slots (Cleric/Rogue/
        // Warrior/Wizard) is computed per CR 700.8b for creatures with
        // multiple party-relevant types. Bounded to `0..=4`.
        QuantityRef::PartySize { player: scope } => {
            resolve_per_player_scalar(state, *scope, controller, ctx, targets, |p| {
                compute_party_size(state, p.id)
            })
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
        // CR 122.1: Counters on an object, scoped via ObjectScope (Π-5).
        // Replaces CountersOnSelf / CountersOnTarget / AnyCountersOnSelf /
        // AnyCountersOnTarget. `counter_type = None` sums every type.
        QuantityRef::CountersOn {
            scope,
            counter_type,
        } => {
            let object = object_for_scope(state, *scope, ctx, targets);
            object
                .map(|obj| match counter_type {
                    Some(ct) => {
                        let kind = parse_counter_type(ct);
                        u32_to_i32_saturating(obj.counters.get(&kind).copied().unwrap_or(0))
                    }
                    None => u32_to_i32_saturating(obj.counters.values().copied().sum::<u32>()),
                })
                .unwrap_or(0)
        }
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
        // falling back to Last Known Information if the source has left the
        // battlefield. Scoped via ObjectScope (Π-6).
        QuantityRef::Power { scope } => resolve_object_pt(
            state,
            *scope,
            ctx,
            targets,
            |obj| obj.power,
            |lki| lki.power,
        ),
        QuantityRef::Toughness { scope } => resolve_object_pt(
            state,
            *scope,
            ctx,
            targets,
            |obj| obj.toughness,
            |lki| lki.toughness,
        ),
        QuantityRef::ObjectManaValue { scope } => {
            resolve_object_mana_value(state, *scope, ctx, targets)
        }
        // CR 105.1 + CR 105.2: Count the object's current colors. The color
        // vector is maintained by layer 5, so recipient-relative static boosts
        // see color-changing effects correctly when this resolves in layer 7c.
        QuantityRef::ObjectColorCount { scope } => {
            resolve_object_color_count(state, *scope, ctx, targets)
        }
        QuantityRef::ObjectNameWordCount { scope } => {
            resolve_object_name_word_count(state, *scope, ctx, targets)
        }
        QuantityRef::ManaSymbolsInManaCost { scope, color } => {
            resolve_mana_symbols_in_mana_cost(state, *scope, *color, ctx, targets)
        }
        // CR 202.3 + CR 118.9: Mana value of the source object. Used by
        // alt-cost cast permissions ("pay life equal to its mana value rather
        // than paying its mana cost") where `source_id` is the spell being
        // cast. Falls back to LKI for objects that have left their zone
        // mid-resolution.
        QuantityRef::SelfManaValue => state
            .objects
            .get(&source_id)
            .map(|obj| u32_to_i32_saturating(obj.mana_cost.mana_value()))
            .or_else(|| {
                state
                    .lki_cache
                    .get(&source_id)
                    .map(|lki| u32_to_i32_saturating(lki.mana_value))
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
        QuantityRef::Devotion { colors } => match colors {
            crate::types::ability::DevotionColors::Fixed(colors) => u32_to_i32_saturating(
                crate::game::devotion::count_devotion(state, controller, colors),
            ),
            crate::types::ability::DevotionColors::ChosenColor => state
                .objects
                .get(&ctx.source)
                .and_then(|obj| obj.chosen_color())
                .or_else(|| {
                    state
                        .last_named_choice
                        .as_ref()
                        .and_then(|choice| match choice {
                            crate::types::ability::ChoiceValue::Color(color) => Some(*color),
                            _ => None,
                        })
                })
                .map(|color| {
                    u32_to_i32_saturating(crate::game::devotion::count_devotion(
                        state,
                        controller,
                        &[color],
                    ))
                })
                .unwrap_or(0),
        },
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
        // CR 205.2a: Count distinct card types (CoreType) across a source set.
        QuantityRef::DistinctCardTypes { source } => {
            let mut seen = HashSet::new();
            match source {
                CardTypeSetSource::Zone { zone, scope } => match zone {
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
                },
                CardTypeSetSource::ExiledBySource => {
                    for linked in
                        crate::game::players::linked_exile_cards_for_source(state, source_id)
                    {
                        if let Some(obj) = state.objects.get(&linked.exiled_id) {
                            for ct in &obj.card_types.core_types {
                                seen.insert(*ct);
                            }
                        }
                    }
                }
                CardTypeSetSource::Objects { filter } => {
                    let zone = filter
                        .extract_in_zone()
                        .unwrap_or(crate::types::zones::Zone::Battlefield);
                    for obj_id in crate::game::targeting::zone_object_ids(state, zone) {
                        if !matches_target_filter(state, obj_id, filter, &filter_ctx) {
                            continue;
                        }
                        if let Some(obj) = state.objects.get(&obj_id) {
                            for ct in &obj.card_types.core_types {
                                seen.insert(*ct);
                            }
                        }
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
        // The resolver stamps this from the parent effect's semantic event class.
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
        // CR 117.1 + CR 202.3: Mana value of the cost-paid object (sacrificed
        // creature, exiled creature, etc.), captured at cost-payment time on
        // the resolving ability. Food Chain / Burnt Offering / Metamorphosis.
        // Returns 0 when no cost-paid object snapshot is in scope.
        QuantityRef::CostPaidObjectManaValue => ability
            .and_then(|a| a.cost_paid_object_mana_value)
            .map(u32_to_i32_saturating)
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
        // CR 106.3 + CR 601.2h: Mana spent to cast a spell, parameterized by
        // scope and metric. Source-qualified metrics read one payment-time
        // source snapshot per mana unit, so Treasure/Cave/artifact-source
        // queries do not depend on the producing permanent still existing or
        // retaining the same type.
        QuantityRef::ManaSpentToCast { scope, metric } => {
            let cast_object = match scope {
                CastManaObjectScope::SelfObject => Some(ctx.self_object()),
                CastManaObjectScope::TriggeringSpell => state
                    .current_trigger_event
                    .as_ref()
                    .and_then(crate::game::targeting::extract_source_from_event),
            };
            cast_object
                .and_then(|id| resolve_mana_spent_to_cast_metric(state, id, metric, &filter_ctx))
                .unwrap_or(0)
        }
        // CR 903.4 + CR 903.4f: Number of distinct colors in the controller's
        // commander(s)' combined color identity. Returns 0 when the controller
        // has no commander (per CR 903.4f: "that quality is undefined if that
        // player doesn't have a commander"). War Room's pay-life cost reads
        // this; an undefined identity pays 0 life (and per Scryfall ruling,
        // the ability is still activatable).
        QuantityRef::ColorsInCommandersColorIdentity => usize_to_i32_saturating(
            super::commander::commander_color_identity(state, controller).len(),
        ),
        QuantityRef::CommanderCastFromCommandZoneCount => u32_to_i32_saturating(
            super::commander::commander_casts_from_command_zone(state, controller),
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
        // CR 305.6: Count distinct basic land types among lands controlled by
        // the referenced player. Domain counts distinct land subtypes, not
        // lands, so multiple Forests still contribute one.
        QuantityRef::BasicLandTypeCount {
            controller: land_controller,
        } => {
            let target_player = ability.and_then(|a| {
                a.targets.iter().find_map(|target| match target {
                    TargetRef::Player(player) => Some(*player),
                    TargetRef::Object(_) => None,
                })
            });
            let basic_subtypes = ["Plains", "Island", "Swamp", "Mountain", "Forest"];
            let mut found = HashSet::new();
            for &id in state.battlefield.iter() {
                if let Some(obj) = state.objects.get(&id) {
                    let controller_matches = match land_controller {
                        ControllerRef::You => obj.controller == controller,
                        ControllerRef::Opponent => obj.controller != controller,
                        ControllerRef::TargetPlayer => target_player == Some(obj.controller),
                        ControllerRef::DefendingPlayer => {
                            crate::game::combat::defending_player_for_attacker(state, ctx.source)
                                .is_some_and(|pid| pid == obj.controller)
                        }
                    };
                    if controller_matches && obj.card_types.core_types.contains(&CoreType::Land) {
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
                                spell_record_matches_filter(
                                    record,
                                    filter,
                                    controller,
                                    &state.all_creature_types,
                                )
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
        // CR 119.4: Life gained this turn, scoped via PlayerScope (Π-4).
        QuantityRef::LifeGainedThisTurn { player } => {
            resolve_per_player_scalar(state, *player, controller, ctx, targets, |p| {
                u32_to_i32_saturating(p.life_gained_this_turn)
            })
        }
        // CR 400.7 + CR 700.4: Count zone-change snapshots from this turn
        // using last-known characteristics for the moved object.
        QuantityRef::ZoneChangeCountThisTurn { from, to, filter } => usize_to_i32_saturating(
            state
                .zone_changes_this_turn
                .iter()
                .filter(|record| {
                    from.is_none_or(|zone| record.from_zone == Some(zone))
                        && to.is_none_or(|zone| record.to_zone == zone)
                        && matches_target_filter_on_zone_change_record(
                            state,
                            record,
                            filter,
                            &filter_ctx,
                        )
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
        // CR 122.1 + CR 122.6: Count counters put this turn by the scoped
        // actor onto objects matching event-time recipient characteristics.
        QuantityRef::CounterAddedThisTurn {
            actor,
            counters,
            target,
        } => u32_to_i32_saturating(
            state
                .counter_added_this_turn
                .iter()
                .filter(|record| {
                    counter_added_actor_matches(actor, controller, record.actor)
                        && counter_match_matches(counters, &record.counter_type)
                        && matches_target_filter_on_counter_added_record(
                            state,
                            record,
                            target,
                            &filter_ctx,
                        )
                })
                .fold(0, |total: u32, record| total.saturating_add(record.count)),
        ),
        // CR 701.9 + CR 603.4: Whether any opponent of the controller discarded
        // a card this turn. Mirrors `LifeLostThisTurn { Opponent { Sum } }`
        // semantics — scans the per-turn discard set for any player != controller.
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
        QuantityRef::KickerCount => state
            .objects
            .get(&ctx.self_object())
            .map(|obj| usize_to_i32_saturating(obj.kickers_paid.len()))
            .unwrap_or(0),
        QuantityRef::ConvokedCreatureCount => state
            .objects
            .get(&ctx.self_object())
            .map(|obj| usize_to_i32_saturating(obj.convoked_creatures.len()))
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
                        Some(ControllerRef::DefendingPlayer) => {
                            crate::game::combat::defending_player_for_attacker(state, ctx.source)
                                .is_some_and(|pid| pid == snap.controller)
                        }
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
    state.objects.get(&obj_id).is_some_and(|obj| {
        card_types
            .iter()
            .any(|tf| type_filter_matches(tf, obj, &state.all_creature_types))
    })
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

fn counter_added_actor_matches(scope: &CountScope, controller: PlayerId, actor: PlayerId) -> bool {
    match scope {
        CountScope::Controller => actor == controller,
        CountScope::All => true,
        CountScope::Opponents => actor != controller,
    }
}

fn counter_match_matches(counter_match: &CounterMatch, counter_type: &CounterType) -> bool {
    match counter_match {
        CounterMatch::Any => true,
        CounterMatch::OfType(expected) => expected == counter_type,
    }
}

/// Resolve an object scope to a live object.
///
/// `Recipient` is the per-object binding supplied by layer evaluation. Outside
/// layers, it falls back to the first object target and then the source, matching
/// the affected-object reading of "its" in targeted spell effects.
fn object_for_scope<'a>(
    state: &'a GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
) -> Option<&'a crate::game::game_object::GameObject> {
    match scope {
        ObjectScope::Source => state.objects.get(&ctx.source),
        ObjectScope::Target => targets.iter().find_map(|t| match t {
            TargetRef::Object(id) => state.objects.get(id),
            _ => None,
        }),
        ObjectScope::Recipient => ctx
            .recipient
            .and_then(|id| state.objects.get(&id))
            .or_else(|| {
                targets.iter().find_map(|t| match t {
                    TargetRef::Object(id) => state.objects.get(id),
                    _ => None,
                })
            })
            .or_else(|| state.objects.get(&ctx.source)),
        ObjectScope::EventSource => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
            .and_then(|id| state.objects.get(&id)),
    }
}

fn object_id_for_scope(
    state: &GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
) -> Option<ObjectId> {
    match scope {
        ObjectScope::Source => Some(ctx.source),
        ObjectScope::Target => targets.iter().find_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        }),
        ObjectScope::Recipient => ctx
            .recipient
            .or_else(|| {
                targets.iter().find_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
            })
            .or(Some(ctx.source)),
        ObjectScope::EventSource => state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event),
    }
}

fn resolve_object_color_count(
    state: &GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
) -> i32 {
    let Some(object_id) = object_id_for_scope(state, scope, ctx, targets) else {
        return 0;
    };
    let colors = state
        .objects
        .get(&object_id)
        .map(|obj| obj.color.as_slice())
        .or_else(|| {
            state
                .lki_cache
                .get(&object_id)
                .map(|lki| lki.colors.as_slice())
        });
    colors
        .map(|colors| usize_to_i32_saturating(colors.iter().copied().collect::<HashSet<_>>().len()))
        .unwrap_or(0)
}

fn resolve_object_name_word_count(
    state: &GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
) -> i32 {
    let Some(object_id) = object_id_for_scope(state, scope, ctx, targets) else {
        return 0;
    };
    let name = state
        .objects
        .get(&object_id)
        .map(|obj| obj.name.as_str())
        .or_else(|| state.lki_cache.get(&object_id).map(|lki| lki.name.as_str()));
    name.map(|name| usize_to_i32_saturating(name.split_whitespace().count()))
        .unwrap_or(0)
}

fn resolve_mana_symbols_in_mana_cost(
    state: &GameState,
    scope: ObjectScope,
    color: ManaColor,
    ctx: QuantityContext,
    targets: &[TargetRef],
) -> i32 {
    object_for_scope(state, scope, ctx, targets)
        .map(|obj| match &obj.mana_cost {
            ManaCost::Cost { shards, .. } => usize_to_i32_saturating(
                shards
                    .iter()
                    .filter(|shard| shard.contributes_to(color))
                    .count(),
            ),
            ManaCost::NoCost | ManaCost::SelfManaCost => 0,
        })
        .unwrap_or(0)
}

/// CR 208.3 + CR 113.6 + CR 400.7: Resolve a per-object scalar (power, toughness)
/// through an `ObjectScope`, with LKI fallback for the source.
///
/// Single authority for `Power { scope }` / `Toughness { scope }` resolution
/// (Π-6). `obj_extract` returns the property for a current object; `lki_extract`
/// returns the same property from a Last Known Information snapshot. LKI fallback
/// applies only to the source object — Target reads only the current state per
/// CR 113.6 (a target's identity is captured on cast/announce).
fn resolve_object_pt<F, G>(
    state: &GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
    obj_extract: F,
    lki_extract: G,
) -> i32
where
    F: Fn(&crate::game::game_object::GameObject) -> Option<i32>,
    G: Fn(&crate::types::game_state::LKISnapshot) -> Option<i32>,
{
    match scope {
        ObjectScope::Source => state
            .objects
            .get(&ctx.source)
            .and_then(&obj_extract)
            .or_else(|| state.lki_cache.get(&ctx.source).and_then(&lki_extract))
            .unwrap_or(0),
        ObjectScope::Target => targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => state.objects.get(id),
                _ => None,
            })
            .and_then(&obj_extract)
            .unwrap_or(0),
        ObjectScope::Recipient => object_for_scope(state, ObjectScope::Recipient, ctx, targets)
            .and_then(&obj_extract)
            .unwrap_or(0),
        ObjectScope::EventSource => {
            let Some(object_id) =
                object_id_for_scope(state, ObjectScope::EventSource, ctx, targets)
            else {
                return 0;
            };
            state
                .objects
                .get(&object_id)
                .and_then(&obj_extract)
                .or_else(|| state.lki_cache.get(&object_id).and_then(&lki_extract))
                .unwrap_or(0)
        }
    }
}

/// CR 202.3: Resolve an object's mana value through the same ObjectScope axis
/// used for power/toughness. Source scope falls back to LKI for objects that
/// moved during resolution; target scope reads the selected object target.
fn resolve_object_mana_value(
    state: &GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
) -> i32 {
    match scope {
        ObjectScope::Source => state
            .objects
            .get(&ctx.source)
            .map(|obj| u32_to_i32_saturating(obj.mana_cost.mana_value()))
            .or_else(|| {
                state
                    .lki_cache
                    .get(&ctx.source)
                    .map(|lki| u32_to_i32_saturating(lki.mana_value))
            })
            .unwrap_or(0),
        ObjectScope::Target => targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => state.objects.get(id),
                _ => None,
            })
            .map(|obj| u32_to_i32_saturating(obj.mana_cost.mana_value()))
            .unwrap_or(0),
        ObjectScope::Recipient => object_for_scope(state, ObjectScope::Recipient, ctx, targets)
            .map(|obj| u32_to_i32_saturating(obj.mana_cost.mana_value()))
            .unwrap_or(0),
        ObjectScope::EventSource => {
            let Some(object_id) =
                object_id_for_scope(state, ObjectScope::EventSource, ctx, targets)
            else {
                return 0;
            };
            state
                .objects
                .get(&object_id)
                .map(|obj| u32_to_i32_saturating(obj.mana_cost.mana_value()))
                .or_else(|| {
                    state
                        .lki_cache
                        .get(&object_id)
                        .map(|lki| u32_to_i32_saturating(lki.mana_value))
                })
                .unwrap_or(0)
        }
    }
}

/// CR 102 + CR 119 + CR 402: Resolve a per-player scalar through a `PlayerScope`.
///
/// Single authority for all `LifeTotal { player }` / `HandSize { player }`-style
/// player-scoped quantity references. `extract` returns the scalar for a single
/// player (e.g., `p.life`, `p.hand.len()`); the scope decides which players
/// contribute and how to combine them.
///
/// - `Controller`: returns the controller's value, or 0 if not found.
/// - `Target`: returns the first player target's value (CR 115.1), or 0.
/// - `RecipientController`: returns the controller of the per-recipient object
///   supplied by layer evaluation; outside layer scope it falls back to the
///   first object target, then the source object.
/// - `Opponent { aggregate }`: aggregates over `p.id != controller` (CR 102.2).
/// - `AllPlayers { aggregate }`: aggregates over every player (CR 102.1).
fn resolve_per_player_scalar<F>(
    state: &GameState,
    scope: PlayerScope,
    controller: PlayerId,
    ctx: QuantityContext,
    targets: &[TargetRef],
    mut extract: F,
) -> i32
where
    F: FnMut(&crate::types::player::Player) -> i32,
{
    match scope {
        PlayerScope::Controller => state
            .players
            .iter()
            .find(|p| p.id == controller)
            .map_or(0, &mut extract),
        PlayerScope::Target => targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Player(pid) => state.players.iter().find(|p| p.id == *pid),
                _ => None,
            })
            .map_or(0, &mut extract),
        PlayerScope::RecipientController => {
            let object_id = ctx.recipient.or_else(|| {
                targets.iter().find_map(|target| match target {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
            });
            let recipient_controller = object_id
                .or(Some(ctx.source))
                .and_then(|id| state.objects.get(&id))
                .map(|obj| obj.controller);
            recipient_controller
                .and_then(|pid| state.players.iter().find(|p| p.id == pid))
                .map_or(0, &mut extract)
        }
        PlayerScope::Opponent { aggregate } => aggregate_over_players(
            state.players.iter().filter(|p| p.id != controller),
            aggregate,
            &mut extract,
        ),
        PlayerScope::AllPlayers { aggregate } => {
            aggregate_over_players(state.players.iter(), aggregate, &mut extract)
        }
    }
}

/// CR 107.3e: Reduce a player iterator to a single i32 by aggregate function.
/// Returns 0 for an empty iterator (mirrors the prior `OpponentLifeTotal`
/// `.unwrap_or(0)` semantics — there is always at least one opponent in a
/// real game, but a 1-player test harness should not panic).
fn aggregate_over_players<'a, I, F>(players: I, aggregate: AggregateFunction, mut extract: F) -> i32
where
    I: IntoIterator<Item = &'a crate::types::player::Player>,
    F: FnMut(&crate::types::player::Player) -> i32,
{
    let values = players.into_iter().map(&mut extract);
    match aggregate {
        AggregateFunction::Max => values.max().unwrap_or(0),
        AggregateFunction::Min => values.min().unwrap_or(0),
        AggregateFunction::Sum => values.sum(),
    }
}

/// CR 700.8 + CR 700.8b: Compute the size of `player`'s party.
///
/// A player's party consists of up to one Cleric creature, one Rogue, one
/// Warrior, and one Wizard the player controls (CR 700.8). When a creature
/// has multiple party-relevant types, it counts toward only one slot, and
/// the assignment maximizes the resulting party size (CR 700.8b). The
/// result is bounded `0..=4`.
///
/// Reads each battlefield creature's post-layer `card_types.subtypes` so
/// type-changing effects (Arcane Adaptation, Conspiracy, etc.) compose
/// correctly. The four party slots are encoded as a 4-bit mask; the maximum
/// matching is computed by exact bipartite enumeration over the 24 slot
/// permutations — trivially small (4 slots, ≤24 permutations) and strictly
/// correct.
pub(crate) fn compute_party_size(state: &GameState, player: PlayerId) -> i32 {
    /// Bitmask: bit 0=Cleric, 1=Rogue, 2=Warrior, 3=Wizard.
    fn party_mask(subtypes: &[String]) -> u8 {
        let mut mask = 0u8;
        for s in subtypes {
            match s.as_str() {
                "Cleric" => mask |= 0b0001,
                "Rogue" => mask |= 0b0010,
                "Warrior" => mask |= 0b0100,
                "Wizard" => mask |= 0b1000,
                _ => {}
            }
        }
        mask
    }

    // Collect non-zero party masks for each creature `player` controls on the
    // battlefield. Creatures with no party-relevant types are skipped.
    let masks: Vec<u8> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|obj| party_mask(&obj.card_types.subtypes))
        .filter(|m| *m != 0)
        .collect();

    if masks.is_empty() {
        return 0;
    }

    // CR 700.8b: try every permutation of the 4 slot indices and assign each
    // creature to the first slot in the permutation it satisfies. Take the
    // maximum across all permutations.
    let permutations: [[u8; 4]; 24] = [
        [0, 1, 2, 3],
        [0, 1, 3, 2],
        [0, 2, 1, 3],
        [0, 2, 3, 1],
        [0, 3, 1, 2],
        [0, 3, 2, 1],
        [1, 0, 2, 3],
        [1, 0, 3, 2],
        [1, 2, 0, 3],
        [1, 2, 3, 0],
        [1, 3, 0, 2],
        [1, 3, 2, 0],
        [2, 0, 1, 3],
        [2, 0, 3, 1],
        [2, 1, 0, 3],
        [2, 1, 3, 0],
        [2, 3, 0, 1],
        [2, 3, 1, 0],
        [3, 0, 1, 2],
        [3, 0, 2, 1],
        [3, 1, 0, 2],
        [3, 1, 2, 0],
        [3, 2, 0, 1],
        [3, 2, 1, 0],
    ];
    let mut best: u32 = 0;
    for perm in &permutations {
        let mut filled: u8 = 0;
        let mut count: u32 = 0;
        for &m in &masks {
            for &slot in perm {
                let bit = 1u8 << slot;
                if filled & bit == 0 && m & bit != 0 {
                    filled |= bit;
                    count += 1;
                    break;
                }
            }
            if filled == 0b1111 {
                break;
            }
        }
        if count > best {
            best = count;
            if best == 4 {
                break;
            }
        }
    }
    best as i32
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
                        PlayerFilter::PerformedActionThisWay { relation, action } => {
                            crate::game::players::matches_relation(p.id, controller, *relation)
                                && crate::game::players::performed_action_this_way(
                                    state, p.id, *action,
                                )
                        }
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
        AggregateFunction, ChoiceValue, ControllerRef, DevotionColors, Effect, FilterProp,
        KickerVariant, ObjectProperty, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::counter::CounterType;
    use crate::types::game_state::{
        ExileLink, ExileLinkKind, ManaSpentSourceSnapshot, ZoneChangeRecord,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::zones::Zone;
    use crate::types::SpellCastRecord;

    fn add_spent_mana_source_snapshot(
        state: &mut GameState,
        cast_object: ObjectId,
        source_id: ObjectId,
    ) {
        let lki = state.objects[&source_id].snapshot_for_mana_spent();
        state
            .objects
            .get_mut(&cast_object)
            .unwrap()
            .mana_spent_source_snapshots
            .push(ManaSpentSourceSnapshot { source_id, lki });
    }

    #[test]
    fn resolve_source_qualified_mana_spent_counts_matching_snapshots() {
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Bat Colony".to_string(),
            Zone::Stack,
        );
        let cave = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Hidden Grotto".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cave)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state
            .objects
            .get_mut(&cave)
            .unwrap()
            .card_types
            .subtypes
            .push("Cave".to_string());
        let forest = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        add_spent_mana_source_snapshot(&mut state, spell, cave);
        add_spent_mana_source_snapshot(&mut state, spell, cave);
        add_spent_mana_source_snapshot(&mut state, spell, forest);

        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::SelfObject,
                metric: CastManaSpentMetric::FromSource {
                    source_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                        "Cave".into(),
                    ))),
                },
            },
        };

        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), spell), 2);
    }

    #[test]
    fn resolve_source_qualified_mana_spent_uses_entering_context() {
        let mut state = GameState::new_two_player(42);
        let static_source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Coin of Mastery".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Creature Spell".to_string(),
            Zone::Battlefield,
        );
        let treasure = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&treasure)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        add_spent_mana_source_snapshot(&mut state, entering, treasure);

        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::SelfObject,
                metric: CastManaSpentMetric::FromSource {
                    source_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                },
            },
        };

        assert_eq!(
            resolve_quantity_with_ctx(
                &state,
                &qty,
                PlayerId(0),
                QuantityContext {
                    entering: Some(entering),
                    source: static_source,
                    recipient: None,
                },
            ),
            1
        );
    }

    /// CR 700.8 + CR 700.8b: party size — building-block test exercising
    /// `compute_party_size` directly across the full assignment surface.
    /// Verifies that the bipartite-matching maximizes the count for creatures
    /// with multi-class subtype lines, that the cap is 4, and that opponent's
    /// creatures don't contribute.
    #[test]
    fn compute_party_size_covers_700_8b_assignment() {
        let mut state = GameState::new_two_player(42);

        // Helper: spawn a creature on `controller`'s battlefield with given subtypes.
        let spawn = |state: &mut GameState, controller: PlayerId, subtypes: &[&str]| {
            let id = create_object(
                state,
                CardId(100),
                controller,
                "Test Creature".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.card_types.subtypes = subtypes.iter().map(|s| (*s).to_string()).collect();
        };

        // No creatures → party size 0.
        assert_eq!(compute_party_size(&state, PlayerId(0)), 0);

        // One Cleric Wizard alone: assignment is forced (one slot), party = 1
        // per CR 700.8b. The set-of-types shortcut would wrongly return 2.
        spawn(&mut state, PlayerId(0), &["Cleric", "Wizard"]);
        assert_eq!(compute_party_size(&state, PlayerId(0)), 1);

        // Add a plain Wizard. Optimal: Cleric Wizard → Cleric, Wizard → Wizard.
        // Party = 2.
        spawn(&mut state, PlayerId(0), &["Wizard"]);
        assert_eq!(compute_party_size(&state, PlayerId(0)), 2);

        // Add a Rogue Warrior. Optimal: assign to Rogue OR Warrior (not both).
        // Party = 3.
        spawn(&mut state, PlayerId(0), &["Rogue", "Warrior"]);
        assert_eq!(compute_party_size(&state, PlayerId(0)), 3);

        // Add a plain Warrior. Optimal: Rogue Warrior → Rogue, Warrior →
        // Warrior, plus the existing Cleric/Wizard pair. Party = 4 (cap).
        spawn(&mut state, PlayerId(0), &["Warrior"]);
        assert_eq!(compute_party_size(&state, PlayerId(0)), 4);

        // Adding a fifth party-typed creature does not exceed the cap.
        spawn(&mut state, PlayerId(0), &["Rogue"]);
        assert_eq!(compute_party_size(&state, PlayerId(0)), 4);

        // Non-party creature subtypes contribute nothing.
        spawn(&mut state, PlayerId(1), &["Goblin", "Soldier"]);
        assert_eq!(compute_party_size(&state, PlayerId(1)), 0);

        // Opponent-controlled party-typed creature does not count for P0.
        spawn(&mut state, PlayerId(1), &["Cleric"]);
        assert_eq!(compute_party_size(&state, PlayerId(1)), 1);
        assert_eq!(compute_party_size(&state, PlayerId(0)), 4);
    }

    /// CR 700.8: end-to-end resolution through `QuantityRef::PartySize` with
    /// `PlayerScope::Controller` reads the controller's party size.
    #[test]
    fn resolve_party_size_controller_scope() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Wizard".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.card_types.subtypes = vec!["Wizard".to_string()];

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PartySize {
                player: PlayerScope::Controller,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(0)), 1);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(0)), 0);
    }

    #[test]
    fn counter_added_this_turn_quantity_counts_by_actor_counter_and_recipient_filter() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Source".to_string(),
            Zone::Battlefield,
        );
        let friendly = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Friendly Creature".to_string(),
            Zone::Battlefield,
        );
        let opposing = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opposing Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&friendly)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state
            .objects
            .get_mut(&opposing)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let mut events = Vec::new();
        crate::game::effects::counters::apply_counter_addition(
            &mut state,
            PlayerId(0),
            friendly,
            CounterType::Plus1Plus1,
            2,
            &mut events,
        );
        crate::game::effects::counters::apply_counter_addition(
            &mut state,
            PlayerId(0),
            opposing,
            CounterType::Plus1Plus1,
            3,
            &mut events,
        );
        crate::game::effects::counters::apply_counter_addition(
            &mut state,
            PlayerId(1),
            friendly,
            CounterType::Plus1Plus1,
            5,
            &mut events,
        );
        crate::game::effects::counters::apply_counter_addition(
            &mut state,
            PlayerId(0),
            friendly,
            CounterType::Loyalty,
            7,
            &mut events,
        );

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CounterAddedThisTurn {
                actor: CountScope::Controller,
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source_id), 2);
    }

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

    #[test]
    fn resolve_quantity_commander_cast_from_command_zone_count() {
        use crate::game::commander::record_commander_cast;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        let commander_a = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Partner A".to_string(),
            Zone::Command,
        );
        let commander_b = create_object(
            &mut state,
            CardId(202),
            PlayerId(0),
            "Partner B".to_string(),
            Zone::Command,
        );
        let opponent_commander = create_object(
            &mut state,
            CardId(203),
            PlayerId(1),
            "Opponent Commander".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&commander_a).unwrap().is_commander = true;
        state.objects.get_mut(&commander_b).unwrap().is_commander = true;
        state
            .objects
            .get_mut(&opponent_commander)
            .unwrap()
            .is_commander = true;

        record_commander_cast(&mut state, commander_a);
        record_commander_cast(&mut state, commander_a);
        record_commander_cast(&mut state, commander_b);
        record_commander_cast(&mut state, opponent_commander);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CommanderCastFromCommandZoneCount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(0)), 3);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(0)), 1);
    }

    #[test]
    fn devotion_to_chosen_color_uses_current_named_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nyx Lotus".to_string(),
            Zone::Battlefield,
        );
        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Green Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&permanent).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 1,
        };
        state.last_named_choice = Some(ChoiceValue::Color(ManaColor::Green));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Devotion {
                colors: DevotionColors::ChosenColor,
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 2);
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
    fn distinct_card_types_among_other_nonland_permanents_counts_matching_objects() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Loot, the Key to Everything".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let artifact_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Artifact Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact_creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact, CoreType::Creature];

        let enchantment = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&enchantment)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Enchantment];

        let land_artifact = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Land Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land_artifact)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land, CoreType::Artifact];

        let opponent_planeswalker = create_object(
            &mut state,
            CardId(5),
            PlayerId(1),
            "Opponent Planeswalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_planeswalker)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Planeswalker];

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::Objects {
                    filter: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Permanent)
                            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Another]),
                    ),
                },
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 3);
    }

    #[test]
    fn object_count_controller_ref_defending_player_uses_combat_attacker() {
        let mut state = GameState::new_two_player(42);
        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        let your_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&your_land)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];
        for i in 0..2 {
            let land = create_object(
                &mut state,
                CardId(3 + i),
                PlayerId(1),
                format!("Island {i}"),
                Zone::Battlefield,
            );
            state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];
        }
        state.combat = Some(crate::game::combat::CombatState {
            attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
                attacker,
                PlayerId(1),
            )],
            ..Default::default()
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::land().controller(ControllerRef::DefendingPlayer),
                ),
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), attacker), 2);
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

    #[test]
    fn resolve_quantity_kicker_count_reads_source_object_payments() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Multikicked".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&obj_id).unwrap().kickers_paid = vec![
            KickerVariant::First,
            KickerVariant::First,
            KickerVariant::First,
        ];

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::KickerCount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), obj_id), 3);
    }

    #[test]
    fn resolve_quantity_convoked_creature_count_reads_source_object() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1001),
            PlayerId(0),
            "Convoked Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().convoked_creatures =
            vec![ObjectId(10), ObjectId(11), ObjectId(12)];

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ConvokedCreatureCount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), obj_id), 3);
    }

    #[test]
    fn resolve_zone_change_count_this_turn_filters_dies_subtype() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Ashen-Skin Zubera".to_string(),
            Zone::Graveyard,
        );
        let zubera = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Zubera".to_string(), "Spirit".to_string()],
            ..ZoneChangeRecord::test_minimal(ObjectId(10), Some(Zone::Battlefield), Zone::Graveyard)
        };
        let non_zubera = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string()],
            ..ZoneChangeRecord::test_minimal(ObjectId(11), Some(Zone::Battlefield), Zone::Graveyard)
        };
        let zubera_bounced = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Zubera".to_string()],
            ..ZoneChangeRecord::test_minimal(ObjectId(12), Some(Zone::Battlefield), Zone::Hand)
        };
        state
            .zone_changes_this_turn
            .extend([zubera, non_zubera, zubera_bounced]);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Graveyard),
                filter: TargetFilter::Typed(TypedFilter::creature().subtype("Zubera".to_string())),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 1);
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
            qty: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
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
    fn resolve_quantity_object_count_creatures_blocking_source() {
        use crate::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let other_attacker = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Other Attacker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&other_attacker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let blockers: Vec<_> = (0..3)
            .map(|i| {
                let id = create_object(
                    &mut state,
                    CardId(30 + i),
                    PlayerId(1),
                    format!("Blocker {i}"),
                    Zone::Battlefield,
                );
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Creature);
                id
            })
            .collect();
        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(source, PlayerId(1)),
                AttackerInfo::attacking_player(other_attacker, PlayerId(1)),
            ],
            blocker_assignments: [
                (source, vec![blockers[0], blockers[1]]),
                (other_attacker, vec![blockers[2]]),
            ]
            .into(),
            blocker_to_attacker: [
                (blockers[0], vec![source]),
                (blockers[1], vec![source]),
                (blockers[2], vec![other_attacker]),
            ]
            .into(),
            ..CombatState::default()
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::BlockingSource]),
                ),
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 2);
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
            qty: QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::ExiledBySource,
            },
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

    /// CR 119.3: `LifeLostThisTurn { Opponent { Sum } }` sums life lost across
    /// opponents, excluding the controller. Three players' losses [2, 5, 1]
    /// with controller = 0 → sum of opponents 1+2 = 5+1 = 6. Locks in the
    /// pre-Π-3 `OpponentLifeLostThisTurn` semantic.
    #[test]
    fn resolve_quantity_opponent_life_lost_this_turn_sum() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        state.players[0].life_lost_this_turn = 2;
        state.players[1].life_lost_this_turn = 5;
        state.players[2].life_lost_this_turn = 1;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::LifeLostThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
        };
        // Controller = player 0: opponents are 1 and 2 → 5 + 1 = 6.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 6);
        // Controller = player 1: opponents are 0 and 2 → 2 + 1 = 3.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(1)), 3);
    }

    /// CR 119.3 + CR 603.4: `LifeLostThisTurn { AllPlayers { Max } }` returns
    /// the maximum life-loss across all players (controller + opponents),
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
            qty: QuantityRef::LifeLostThisTurn {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                },
            },
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
            qty: QuantityRef::LifeLostThisTurn {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Max,
                },
            },
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
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some("loyalty".to_string()),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 4);
    }

    /// CR 122.1: `AnyCountersOnSelf` sums every counter type on the source
    /// object — used by Gemstone Mine's "no counters on it" sacrifice trigger
    /// and the depletion-land cycle. Mirrors the `AnyCountersOnTarget` resolver
    /// but reads from `source_id` instead of the target list.
    #[test]
    fn resolve_quantity_any_counters_on_self_sums_all_types() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        // Sum across distinct counter types — Gemstone Mine prints "mining",
        // depletion-land cycle prints "depletion", etc. The any-type resolver
        // must aggregate every present type, not just one canonical kind.
        obj.counters
            .insert(CounterType::Generic("mining".to_string()), 2);
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 3);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: None,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);
    }

    /// Bare source with no counters → 0 (the Gemstone Mine sacrifice gate
    /// composed against `EQ 0` then fires).
    #[test]
    fn resolve_quantity_any_counters_on_self_empty_is_zero() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: None,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 0);
    }

    /// CR 119.4 (Π-4): `LifeGainedThisTurn { Opponent { Sum } }` sums life
    /// gained across opponents, excluding the controller. Locks in the
    /// opponent-axis semantic introduced by Π-4 (no pre-Π-4 equivalent —
    /// `LifeGainedThisTurn` was unit-variant controller-only before).
    #[test]
    fn resolve_quantity_opponent_life_gained_this_turn_sum() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        state.players[0].life_gained_this_turn = 4;
        state.players[1].life_gained_this_turn = 7;
        state.players[2].life_gained_this_turn = 2;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
        };
        // Controller = player 0: opponents are 1 and 2 → 7 + 2 = 9.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 9);
        // Controller = player 2: opponents are 0 and 1 → 4 + 7 = 11.
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(2), ObjectId(1)),
            11
        );
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
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::Target,
            },
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
            qty: QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);

        let expr_t = QuantityExpr::Ref {
            qty: QuantityRef::Toughness {
                scope: crate::types::ability::ObjectScope::Source,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr_t, PlayerId(0), source), 3);
    }

    #[test]
    fn resolve_object_color_count_source_target_and_recipient() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let recipient = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Recipient".to_string(),
            Zone::Battlefield,
        );

        state.objects.get_mut(&source).unwrap().color = vec![ManaColor::White];
        state.objects.get_mut(&target).unwrap().color =
            vec![ManaColor::Blue, ManaColor::Black, ManaColor::Blue];
        state.objects.get_mut(&recipient).unwrap().color =
            vec![ManaColor::Red, ManaColor::Green, ManaColor::White];

        let source_expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectColorCount {
                scope: ObjectScope::Source,
            },
        };
        assert_eq!(
            resolve_quantity(&state, &source_expr, PlayerId(0), source),
            1
        );

        let target_expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectColorCount {
                scope: ObjectScope::Target,
            },
        };
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: target_expr.clone(),
                player: crate::types::ability::GainLifePlayer::Controller,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        assert_eq!(
            resolve_quantity_with_targets(&state, &target_expr, &ability),
            2
        );

        let recipient_expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectColorCount {
                scope: ObjectScope::Recipient,
            },
        };
        assert_eq!(
            resolve_quantity_with_recipient(
                &state,
                &recipient_expr,
                PlayerId(0),
                source,
                recipient
            ),
            3
        );
    }

    #[test]
    fn resolve_object_name_word_count_uses_recipient_name_not_source_name() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Wordmail".to_string(),
            Zone::Battlefield,
        );
        let recipient = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Swords to Plowshares".to_string(),
            Zone::Battlefield,
        );

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectNameWordCount {
                scope: ObjectScope::Recipient,
            },
        };

        assert_eq!(
            resolve_quantity_with_recipient(&state, &expr, PlayerId(0), source, recipient),
            3
        );

        let source_expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectNameWordCount {
                scope: ObjectScope::Source,
            },
        };
        assert_eq!(
            resolve_quantity_with_recipient(&state, &source_expr, PlayerId(0), source, recipient),
            1
        );
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
    fn resolve_sum_of_independent_refs_against_state() {
        // A-Alrund pattern: hand size + count of foretold cards in exile.
        // Validates that Sum recurses through fold_compose and that each
        // child resolves independently against game state (not a tautology
        // over Fixed values).
        let mut state = GameState::new_two_player(42);
        let player_id = state.players[0].id;

        // Put 3 cards in hand. `create_object(..., Zone::Hand)` already
        // pushes onto the player's hand vector — no second push needed.
        for _ in 0..3 {
            let _ = create_object(
                &mut state,
                CardId(0),
                player_id,
                "Card".to_string(),
                Zone::Hand,
            );
        }

        let expr = QuantityExpr::Sum {
            exprs: vec![
                QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                QuantityExpr::Fixed { value: 7 },
            ],
        };
        assert_eq!(
            resolve_quantity(&state, &expr, player_id, ObjectId(1)),
            10,
            "expected 3 (hand) + 7 (fixed) = 10"
        );
    }

    #[test]
    fn object_count_matches_owned_foretold_cards_in_exile() {
        let mut state = GameState::new_two_player(42);
        let player_id = state.players[0].id;
        let opponent_id = state.players[1].id;

        let owned_foretold_a = create_object(
            &mut state,
            CardId(10),
            player_id,
            "Foretold A".to_string(),
            Zone::Exile,
        );
        let owned_foretold_b = create_object(
            &mut state,
            CardId(11),
            player_id,
            "Foretold B".to_string(),
            Zone::Exile,
        );
        let owned_not_foretold = create_object(
            &mut state,
            CardId(12),
            player_id,
            "Not Foretold".to_string(),
            Zone::Exile,
        );
        let opponent_foretold = create_object(
            &mut state,
            CardId(13),
            opponent_id,
            "Opponent Foretold".to_string(),
            Zone::Exile,
        );

        for id in [owned_foretold_a, owned_foretold_b, opponent_foretold] {
            state.objects.get_mut(&id).unwrap().foretold = true;
        }
        assert!(!state.objects.get(&owned_not_foretold).unwrap().foretold);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::Foretold,
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::InZone { zone: Zone::Exile },
                ])),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, player_id, ObjectId(1)), 2);
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
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
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
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
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
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: crate::types::ability::Comparator::LE,
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

    /// CR 202.3 + CR 118.9: `SelfManaValue` reads the source object's printed
    /// mana value at resolve-time. Used by alt-cost cast permissions
    /// (`ExileWithAltAbilityCost`) where "its mana value" must resolve
    /// against the spell-being-cast (passed as `source_id`).
    #[test]
    fn self_mana_value_reads_source_mana_cost() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Exile,
        );
        // Set mana cost = {3}{B}{B} → mana value 5.
        let cost = crate::types::mana::ManaCost::Cost {
            shards: vec![
                crate::types::mana::ManaCostShard::Black,
                crate::types::mana::ManaCostShard::Black,
            ],
            generic: 3,
        };
        state.objects.get_mut(&obj_id).unwrap().mana_cost = cost;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::SelfManaValue,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), obj_id), 5);
    }
}
