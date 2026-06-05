//! Dynamic quantity resolution for QuantityExpr values.
//!
//! Evaluates QuantityRef variants (ObjectCount, PlayerCount, CountersOnSelf, etc.)
//! against the current game state at resolution time. Used by effect resolvers
//! to support "for each [X]" patterns on Draw, DealDamage, GainLife, LoseLife, Mill.

use std::collections::{HashMap, HashSet};

use crate::game::arithmetic::{u32_to_i32_saturating, usize_to_i32_saturating};
use crate::game::filter::{
    matches_target_filter, matches_target_filter_on_counter_added_record,
    matches_target_filter_on_damage_record_source, matches_target_filter_on_zone_change_record,
    player_matches_target_filter_in_state, spell_record_matches_filter, type_filter_matches,
    FilterContext,
};
use crate::game::speed::effective_speed;
use crate::types::ability::{
    AggregateFunction, AttackScope, CardTypeSetSource, CastManaObjectScope, CastManaSpentMetric,
    ControllerRef, CountScope, FilterProp, ObjectProperty, ObjectScope, PlayerFilter, PlayerScope,
    QuantityExpr, QuantityRef, ResolvedAbility, RoundingMode, TargetFilter, TargetRef, TypeFilter,
    ZoneRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::{positive_counter_types, CounterType};
use crate::types::game_state::{DamageRecord, GameState};
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
    /// Current player for an "each player/opponent" resolution pass. Distinct
    /// from `controller`, which remains the printed ability's controller.
    pub scoped_player: Option<PlayerId>,
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
            scoped_player: None,
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
            scoped_player: None,
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
            | QuantityRef::CardsDrawnThisTurn {
                player: PlayerScope::RecipientController,
            }
            | QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::RecipientController,
            }
            | QuantityRef::TokensCreatedThisTurn {
                player: PlayerScope::RecipientController,
                ..
            }
            | QuantityRef::PlayerActionsThisTurn {
                player: PlayerScope::RecipientController,
                ..
            }
            | QuantityRef::PartySize {
                player: PlayerScope::RecipientController,
            } => true,
            QuantityRef::ObjectCount { filter }
            | QuantityRef::ObjectCountDistinct { filter, .. }
            | QuantityRef::ObjectCountBySharedQuality { filter, .. }
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
            | QuantityRef::ObjectTypelineComponentCount {
                scope: ObjectScope::Recipient,
            }
            | QuantityRef::Power {
                scope: ObjectScope::Recipient,
            }
            | QuantityRef::Toughness {
                scope: ObjectScope::Recipient,
            }
            | QuantityRef::ObjectManaValue {
                scope: ObjectScope::Recipient,
            }
            | QuantityRef::ManaSymbolsInManaCost {
                scope: ObjectScope::Recipient,
                ..
            } => true,
            QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            }
            | QuantityRef::Toughness {
                scope: ObjectScope::CostPaidObject,
            }
            | QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject,
            } => false,
            _ => false,
        },
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_expr_uses_recipient(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(quantity_expr_uses_recipient),
        QuantityExpr::UpTo { max } => quantity_expr_uses_recipient(max),
        QuantityExpr::Power { exponent, .. } => quantity_expr_uses_recipient(exponent),
        QuantityExpr::Difference { left, right } => {
            quantity_expr_uses_recipient(left) || quantity_expr_uses_recipient(right)
        }
    }
}

/// True when the QuantityExpr's magnitude depends on the population of objects
/// on the battlefield (a count/aggregate over a board-wide object set).
///
/// CR 611.3a: a static-ability continuous effect isn't locked in — it applies at
/// any moment to whatever its text indicates. A magnitude that reads board
/// population ("+1/+1 for each creature you control", Devotion, distinct colors
/// among permanents, etc.) re-evaluates when ANY object enters or leaves, which
/// changes the value applied to PRE-EXISTING recipients. The incremental
/// layer-flush fast path must escalate to a full re-evaluation when an active
/// effect carries such a magnitude. CR 613.7d / CR 613.8a: timestamp/dependency
/// ordering operates on the live set, so a population change can also reorder.
///
/// Mirrors the structural recursion of `quantity_expr_uses_recipient`: composite
/// arms recurse, the `QuantityRef` leaf classifies each variant exhaustively
/// (NO wildcard tail) so a future population-reading variant forces a decision
/// here at compile time.
pub(crate) fn quantity_expr_uses_object_count(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Ref { qty } => quantity_ref_uses_object_count(qty),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_expr_uses_object_count(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(quantity_expr_uses_object_count),
        QuantityExpr::UpTo { max } => quantity_expr_uses_object_count(max),
        QuantityExpr::Power { exponent, .. } => quantity_expr_uses_object_count(exponent),
        QuantityExpr::Difference { left, right } => {
            quantity_expr_uses_object_count(left) || quantity_expr_uses_object_count(right)
        }
    }
}

/// CR 611.3a + CR 613.7d: Leaf classification for `quantity_expr_uses_object_count`.
/// EXHAUSTIVE and wildcard-free — adding a `QuantityRef` variant forces a
/// decision here. `true` for any reference that reads battlefield object
/// population; `false` for single-object, player-level, history-record, and
/// payment/choice references whose value is unaffected by another object
/// entering or leaving the battlefield.
fn quantity_ref_uses_object_count(qty: &QuantityRef) -> bool {
    match qty {
        // Read battlefield object population directly.
        QuantityRef::ObjectCount { .. }
        | QuantityRef::ObjectCountDistinct { .. }
        | QuantityRef::ObjectCountBySharedQuality { .. }
        | QuantityRef::CountersOnObjects { .. }
        | QuantityRef::Aggregate { .. }
        | QuantityRef::ControlledByEachPlayer { .. }
        | QuantityRef::Devotion { .. }
        | QuantityRef::BasicLandTypeCount { .. }
        | QuantityRef::PartySize { .. }
        | QuantityRef::DistinctColorsAmongPermanents { .. }
        | QuantityRef::DistinctCounterKindsAmong { .. }
        | QuantityRef::EnteredThisTurn { .. } => true,
        // Distinct card types reads battlefield population ONLY when its source
        // is the object-filter variant; zone / linked-exile sources do not.
        QuantityRef::DistinctCardTypes { source } => match source {
            CardTypeSetSource::Objects { .. } => true,
            CardTypeSetSource::Zone { .. } | CardTypeSetSource::ExiledBySource => false,
        },
        // Player-level, single-object, history-record, payment, and choice
        // references: unaffected by another object's battlefield entry/exit.
        QuantityRef::HandSize { .. }
        | QuantityRef::LifeTotal { .. }
        | QuantityRef::GraveyardSize { .. }
        | QuantityRef::LifeAboveStarting
        | QuantityRef::StartingLifeTotal
        | QuantityRef::PlayerCount { .. }
        | QuantityRef::CountersOn { .. }
        | QuantityRef::PlayerCounter { .. }
        | QuantityRef::Variable { .. }
        | QuantityRef::Power { .. }
        | QuantityRef::Toughness { .. }
        | QuantityRef::ObjectManaValue { .. }
        | QuantityRef::ObjectColorCount { .. }
        | QuantityRef::ObjectNameWordCount { .. }
        | QuantityRef::ObjectTypelineComponentCount { .. }
        | QuantityRef::ManaSymbolsInManaCost { .. }
        | QuantityRef::SelfManaValue
        | QuantityRef::TargetZoneCardCount { .. }
        | QuantityRef::CardsExiledBySource
        | QuantityRef::ZoneCardCount { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::FilteredTrackedSetSize { .. }
        | QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount
        | QuantityRef::LifeLostThisTurn { .. }
        | QuantityRef::Speed { .. }
        | QuantityRef::EventContextAmount
        | QuantityRef::AttachmentsOnLeavingObject { .. }
        | QuantityRef::EventContextSourceCostX
        | QuantityRef::SpellsCastThisTurn { .. }
        | QuantityRef::SacrificedThisTurn { .. }
        | QuantityRef::CrimesCommittedThisTurn
        | QuantityRef::LifeGainedThisTurn { .. }
        | QuantityRef::CardsDrawnThisTurn { .. }
        | QuantityRef::LandsPlayedThisTurn { .. }
        | QuantityRef::TurnsTaken
        | QuantityRef::ZoneChangeCountThisTurn { .. }
        | QuantityRef::DamageDealtThisTurn { .. }
        | QuantityRef::ChosenNumber
        | QuantityRef::AttackedThisTurn
        | QuantityRef::DescendedThisTurn
        | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
        | QuantityRef::SpellsCastLastTurn
        | QuantityRef::SpellsCastThisGame { .. }
        | QuantityRef::CounterAddedThisTurn { .. }
        | QuantityRef::CardsDiscardedThisTurn { .. }
        | QuantityRef::TokensCreatedThisTurn { .. }
        | QuantityRef::PlayerActionsThisTurn { .. }
        | QuantityRef::DungeonsCompleted
        | QuantityRef::CostXPaid
        | QuantityRef::KickerCount
        | QuantityRef::AdditionalCostPaymentCount
        | QuantityRef::ConvokedCreatureCount
        | QuantityRef::ManaSpentToCast { .. }
        | QuantityRef::ColorsInCommandersColorIdentity
        | QuantityRef::CommanderCastFromCommandZoneCount => false,
    }
}

/// CR 611.3a + CR 700.5: ENTRY-AWARE narrowing for a population-sensitive
/// magnitude. `quantity_expr_uses_object_count` proves an effect's magnitude
/// *can* read board population; this proves a SPECIFIC entering object can
/// actually perturb that population input.
///
/// Monotonicity (the correctness foundation): the `EnteredObjects` fast path is
/// reached only for battlefield ENTRIES (leaves always escalate to `Full`).
/// Entry only ADDS objects, so counts only increase, devotion only increases,
/// and presence only flips false→true — the ONLY way an entry changes a
/// population-dependent magnitude is if the entered object is a MEMBER of /
/// CONTRIBUTES to that population. So an entry-membership test is sufficient.
///
/// Mirrors the structural recursion of `quantity_expr_uses_object_count`:
/// composite arms recurse, the `QuantityRef` leaf classifies each variant.
/// `ctx` is built from the EFFECT SOURCE (CR 109.5 controller rebinding), so
/// "you control" filters resolve against the effect's controller — NOT the
/// entered object's controller.
pub(crate) fn entered_object_perturbs_quantity_expr(
    state: &GameState,
    entered: &crate::game::game_object::GameObject,
    ctx: &FilterContext<'_>,
    expr: &QuantityExpr,
) -> bool {
    match expr {
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Ref { qty } => entered_object_perturbs_quantity_ref(state, entered, ctx, qty),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => {
            entered_object_perturbs_quantity_expr(state, entered, ctx, inner)
        }
        QuantityExpr::Sum { exprs } => exprs
            .iter()
            .any(|e| entered_object_perturbs_quantity_expr(state, entered, ctx, e)),
        QuantityExpr::UpTo { max } => {
            entered_object_perturbs_quantity_expr(state, entered, ctx, max)
        }
        QuantityExpr::Power { exponent, .. } => {
            entered_object_perturbs_quantity_expr(state, entered, ctx, exponent)
        }
        QuantityExpr::Difference { left, right } => {
            entered_object_perturbs_quantity_expr(state, entered, ctx, left)
                || entered_object_perturbs_quantity_expr(state, entered, ctx, right)
        }
    }
}

/// CR 611.3a + CR 700.5: entry-membership leaf for
/// `entered_object_perturbs_quantity_expr`. EXHAUSTIVE and wildcard-free — the
/// classification mirrors `quantity_ref_uses_object_count`: every `false` arm
/// THERE is `false` HERE (an object entering can never perturb a value the
/// classifier already proved population-independent), and every `true` arm
/// there is narrowed HERE to "does THIS entered object join the population?".
fn entered_object_perturbs_quantity_ref(
    state: &GameState,
    entered: &crate::game::game_object::GameObject,
    ctx: &FilterContext<'_>,
    qty: &QuantityRef,
) -> bool {
    match qty {
        // Filter-bearing battlefield-population refs: perturbed iff the entered
        // object matches the population filter (CR 109.5 controller via `ctx`).
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. }
        | QuantityRef::CountersOnObjects { filter, .. }
        | QuantityRef::Aggregate { filter, .. }
        | QuantityRef::ControlledByEachPlayer { filter, .. }
        | QuantityRef::DistinctColorsAmongPermanents { filter }
        | QuantityRef::DistinctCounterKindsAmong { filter }
        | QuantityRef::EnteredThisTurn { filter } => {
            matches_target_filter(state, entered.id, filter, ctx)
        }
        QuantityRef::DistinctCardTypes { source } => match source {
            CardTypeSetSource::Objects { filter } => {
                matches_target_filter(state, entered.id, filter, ctx)
            }
            // Zone / linked-exile sources are not battlefield population — the
            // classifier returns false for them, so they cannot be perturbed.
            CardTypeSetSource::Zone { .. } | CardTypeSetSource::ExiledBySource => false,
        },
        // CR 700.5: devotion is perturbed iff the entered object's mana cost
        // contributes a symbol for one of the fixed colors. `ChosenColor`'s
        // color isn't statically known, so conservatively perturb (over-
        // escalation is safe). LOW-1: controller-blind — escalate if ANY
        // entered object contributes a shard regardless of controller.
        QuantityRef::Devotion { colors } => match colors {
            crate::types::ability::DevotionColors::Fixed(cols) => {
                entered_object_contributes_devotion(entered, cols)
            }
            crate::types::ability::DevotionColors::ChosenColor => true,
        },
        // CR 305.6: a basic land entering can change the distinct-basic-land-type
        // count. Synthesizing a precise "land controlled by `controller`" filter
        // is awkward (the count is per-controller-rebound), so conservatively
        // perturb whenever the entered object is a land at all.
        QuantityRef::BasicLandTypeCount { .. } => {
            entered.card_types.core_types.contains(&CoreType::Land)
        }
        // CR 700.8: party is composed of creatures the controller controls — any
        // creature entry can change party size. Conservatively perturb on any
        // creature entry rather than re-deriving the party-type maximization.
        QuantityRef::PartySize { .. } => {
            entered.card_types.core_types.contains(&CoreType::Creature)
        }
        // Player-level, single-object, history-record, payment, and choice refs:
        // an object's battlefield entry/exit cannot change their value. Identical
        // enumeration to the `false` arm of `quantity_ref_uses_object_count`.
        QuantityRef::HandSize { .. }
        | QuantityRef::LifeTotal { .. }
        | QuantityRef::GraveyardSize { .. }
        | QuantityRef::LifeAboveStarting
        | QuantityRef::StartingLifeTotal
        | QuantityRef::PlayerCount { .. }
        | QuantityRef::CountersOn { .. }
        | QuantityRef::PlayerCounter { .. }
        | QuantityRef::Variable { .. }
        | QuantityRef::Power { .. }
        | QuantityRef::Toughness { .. }
        | QuantityRef::ObjectManaValue { .. }
        | QuantityRef::ObjectColorCount { .. }
        | QuantityRef::ObjectNameWordCount { .. }
        | QuantityRef::ObjectTypelineComponentCount { .. }
        | QuantityRef::ManaSymbolsInManaCost { .. }
        | QuantityRef::SelfManaValue
        | QuantityRef::TargetZoneCardCount { .. }
        | QuantityRef::CardsExiledBySource
        | QuantityRef::ZoneCardCount { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::FilteredTrackedSetSize { .. }
        | QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount
        | QuantityRef::LifeLostThisTurn { .. }
        | QuantityRef::Speed { .. }
        | QuantityRef::EventContextAmount
        | QuantityRef::AttachmentsOnLeavingObject { .. }
        | QuantityRef::EventContextSourceCostX
        | QuantityRef::SpellsCastThisTurn { .. }
        | QuantityRef::SacrificedThisTurn { .. }
        | QuantityRef::CrimesCommittedThisTurn
        | QuantityRef::LifeGainedThisTurn { .. }
        | QuantityRef::CardsDrawnThisTurn { .. }
        | QuantityRef::LandsPlayedThisTurn { .. }
        | QuantityRef::TurnsTaken
        | QuantityRef::ZoneChangeCountThisTurn { .. }
        | QuantityRef::DamageDealtThisTurn { .. }
        | QuantityRef::ChosenNumber
        | QuantityRef::AttackedThisTurn
        | QuantityRef::DescendedThisTurn
        | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
        | QuantityRef::SpellsCastLastTurn
        | QuantityRef::SpellsCastThisGame { .. }
        | QuantityRef::CounterAddedThisTurn { .. }
        | QuantityRef::CardsDiscardedThisTurn { .. }
        | QuantityRef::TokensCreatedThisTurn { .. }
        | QuantityRef::PlayerActionsThisTurn { .. }
        | QuantityRef::DungeonsCompleted
        | QuantityRef::CostXPaid
        | QuantityRef::KickerCount
        | QuantityRef::AdditionalCostPaymentCount
        | QuantityRef::ConvokedCreatureCount
        | QuantityRef::ManaSpentToCast { .. }
        | QuantityRef::ColorsInCommandersColorIdentity
        | QuantityRef::CommanderCastFromCommandZoneCount => false,
    }
}

/// CR 700.5: True when the entered object's mana cost carries at least one
/// shard that contributes to devotion for any of `colors`. Mirrors the per-
/// object inner loop of `count_devotion` so an entry that adds a matching
/// symbol perturbs the running devotion total.
fn entered_object_contributes_devotion(
    entered: &crate::game::game_object::GameObject,
    colors: &[ManaColor],
) -> bool {
    if let ManaCost::Cost { ref shards, .. } = entered.mana_cost {
        shards
            .iter()
            .any(|shard| colors.iter().any(|c| shard.contributes_to(*c)))
    } else {
        false
    }
}

pub(crate) fn filter_uses_recipient(filter: &TargetFilter) -> bool {
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
/// `QuantityExpr` variants (`DivideRounded`, `Offset`, `Multiply`, `Sum`,
/// `Power`, `UpTo`, `Difference`). All resolver entry points share this
/// logic; only the leaf arms (`Fixed`, `Ref`) differ in context handling.
/// `recurse` is a closure the caller supplies that re-enters its own
/// resolver with the inner expression.
///
/// Panics if called with a leaf variant — callers must dispatch leaves
/// before delegating here.
fn fold_compose(expr: &QuantityExpr, recurse: impl Fn(&QuantityExpr) -> i32) -> i32 {
    match expr {
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => divide_rounded(recurse(inner), *divisor, *rounding),
        QuantityExpr::Offset { inner, offset } => recurse(inner) + offset,
        // CR 107.1b: effect-result calculations that would be negative use zero
        // instead. The lower bound is parameterized for non-zero floors, but
        // current Oracle users are the zero-floor "beyond the first" class.
        QuantityExpr::ClampMin { inner, minimum } => recurse(inner).max(*minimum),
        QuantityExpr::Multiply { factor, inner } => factor.saturating_mul(recurse(inner)),
        QuantityExpr::Sum { exprs } => exprs.iter().map(&recurse).sum(),
        // CR 107.3: `base ^ exponent` with the exponent resolved from a
        // QuantityExpr (typically the X variable on a cost). The exponent is
        // clamped to a non-negative u32 because `i32::saturating_pow` requires
        // u32 — a raw `as u32` cast of a negative i32 would compute against a
        // ~4B exponent and saturate to `i32::MAX`. No real card emits a
        // negative exponent (X is non-negative per CR 107.3a), so the clamp
        // is purely defensive. `saturating_pow` handles overflow by returning
        // `i32::MAX` rather than panicking.
        QuantityExpr::Power { base, exponent } => {
            let exp = u32::try_from(recurse(exponent)).unwrap_or(0);
            base.saturating_pow(exp)
        }
        // CR 107.1c + CR 608.2d: Generic resolvers see UpTo transparently as
        // its upper bound — the 4 effect-specific resolvers (Draw,
        // Sacrifice, Discard, SearchLibrary) peel the wrapper via
        // `QuantityExpr::peel_up_to` to extract the "may pick fewer" flag
        // before reaching arithmetic. Treating it transparently here keeps
        // legacy serde round-trips correct and makes accidental composition
        // (e.g., `DivideRounded { inner: UpTo { max: ... } }`) collapse to a
        // sensible bound rather than panicking.
        QuantityExpr::UpTo { max } => recurse(max),
        // "The difference between A and B" is an unsigned-magnitude Oracle
        // templating convention — it has no dedicated Comprehensive Rules
        // number. `.abs()` implements that convention: the gap between the two
        // operands regardless of operand order. (CR 107.1b is related but
        // distinct — it clamps a negative *result* to zero, which is not the
        // same operation as an absolute value; it confirms only that the
        // resulting amount is non-negative.)
        QuantityExpr::Difference { left, right } => (recurse(left) - recurse(right)).abs(),
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
    // CR 603.4 + CR 102.1: Derive the "scoped player" from the
    // triggering event so `PlayerScope::ScopedPlayer` (e.g. "that player has
    // no cards in hand" on Ghirapur Orrery) resolves to the event's player
    // — the active player for `PhaseChanged`, the drawing/discarding player
    // for hand-event triggers, etc. Without this binding, intervening-if
    // checks for `that player has X` silently resolve against the source's
    // controller and produce wrong results.
    let resolution_event = state.current_trigger_event.as_ref().or(event);
    let scoped_player =
        resolution_event.and_then(|e| crate::game::targeting::extract_player_from_event(e, state));
    let ctx = QuantityContext {
        entering: None,
        source: source_id,
        recipient: None,
        scoped_player,
    };

    // Fast path: when current_trigger_event is already set (resolution-time
    // re-check in stack::resolve_top), the default resolver reads it directly.
    if state.current_trigger_event.is_some() {
        return resolve_quantity_with_ctx(state, expr, controller, ctx);
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
            resolve_quantity_with_ctx(state, expr, controller, ctx)
        });
    }
    resolve_quantity_with_ctx(state, expr, controller, ctx)
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
///
/// CR 603.4: At trigger *detection* time `state.current_trigger_event` is
/// `None`; the candidate event is held in this thread-local instead. Filter
/// matching for `ControllerRef::TriggeringPlayer` falls back to this accessor
/// so intervening-if quantity checks resolve the triggering player during
/// detection (mirrors the `OtherThanTriggerObject` detection/resolution
/// dual-path).
pub fn detection_trigger_event() -> Option<crate::types::events::GameEvent> {
    DETECTION_TRIGGER_EVENT.with(|slot| slot.borrow().clone())
}

/// CR 603.2 + CR 109.4: Resolve the player identified by the current
/// triggering event, preferring the resolution-time `current_trigger_event`
/// and falling back to the detection-time thread-local override.
///
/// Single authority for `ControllerRef::TriggeringPlayer` resolution — `filter.rs`
/// calls this rather than duplicating the detection/resolution dual-path. Lives
/// here because this module owns the `DETECTION_TRIGGER_EVENT` thread-local.
pub(crate) fn triggering_event_player(state: &GameState) -> Option<PlayerId> {
    state
        .current_trigger_event
        .as_ref()
        .cloned()
        .or_else(detection_trigger_event)
        .and_then(|e| crate::game::targeting::extract_player_from_event(&e, state))
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

pub(crate) fn resolve_mana_spent_to_cast_metric(
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
    let controller = ability.original_controller.unwrap_or(ability.controller);
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(
            state,
            qty,
            controller,
            QuantityContext {
                entering: None,
                source: ability.source_id,
                recipient: None,
                scoped_player: ability.scoped_player,
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

/// CR 608.2c: Resolve a condition quantity from the printed ability controller's
/// perspective while preserving target/chosen-X and scoped-player bindings.
///
/// Player-scoped effect resolution temporarily rebinds `ability.controller` to
/// the player being affected. Conditional clauses such as "if creatures you
/// control have total toughness 40 or greater" still refer to the source's
/// controller, not that temporary recipient.
pub(crate) fn resolve_quantity_for_ability_condition(
    state: &GameState,
    expr: &QuantityExpr,
    ability: &ResolvedAbility,
) -> i32 {
    let Some(original_controller) = ability.original_controller else {
        return resolve_quantity_with_targets(state, expr, ability);
    };
    let mut condition_ability = ability.clone();
    condition_ability.controller = original_controller;
    resolve_quantity_with_targets(state, expr, &condition_ability)
}

/// Resolve a QuantityExpr with ability targets/chosen-X plus a per-object
/// recipient binding for `FilterProp::AttachedToRecipient`.
pub(crate) fn resolve_quantity_with_targets_and_recipient(
    state: &GameState,
    expr: &QuantityExpr,
    ability: &ResolvedAbility,
    recipient_id: ObjectId,
) -> i32 {
    let controller = ability.original_controller.unwrap_or(ability.controller);
    match expr {
        QuantityExpr::Fixed { value } => *value,
        QuantityExpr::Ref { qty } => resolve_ref(
            state,
            qty,
            controller,
            QuantityContext {
                entering: None,
                source: ability.source_id,
                recipient: Some(recipient_id),
                scoped_player: ability.scoped_player,
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
                scoped_player: None,
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
                scoped_player: Some(scope_player),
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
/// spell or ability will tell you whether to round up or down.
fn divide_rounded(value: i32, divisor: u32, rounding: RoundingMode) -> i32 {
    debug_assert!(divisor > 0, "fractional quantity divisor must be nonzero");
    let divisor = i64::from(divisor.max(1));
    let value = i64::from(value);
    let rounded = match rounding {
        RoundingMode::Up => {
            let quotient = value.div_euclid(divisor);
            if value.rem_euclid(divisor) == 0 {
                quotient
            } else {
                quotient + 1
            }
        }
        RoundingMode::Down => value.div_euclid(divisor),
    };
    rounded as i32
}

/// CR 400.1: The object IDs an `ObjectCount { filter }` matches — resolved in
/// the filter's zone (default battlefield) with the `OtherThanTriggerObject`
/// exclusion applied. Single source of truth: the `ObjectCount` count is `.len()`
/// of this, and a "for each [object]" loop (`effects::resolve_chain_body`)
/// iterates these exact ids for per-iteration `ParentTarget` rebinding — so the
/// count and the iteration members can never diverge.
///
/// The `OtherThanTriggerObject` marker excludes the triggering/source object.
/// This derives from the Oracle word "other"/"other than" — a parser-level
/// semantic, not a numbered CR.
///
/// The `OtherThanTriggerObject` exclusion drops the trigger id by SET MEMBERSHIP
/// (`retain`): it only excludes the trigger when it actually appears in the
/// matched set, so a trigger object that matches the filter predicate but lies
/// outside the filter's zone is not wrongly decremented. The `Aggregate` resolver
/// delegates here for its population, so count and aggregate share this exclusion.
pub(crate) fn object_count_matching_ids(
    state: &GameState,
    filter: &TargetFilter,
    filter_ctx: &FilterContext<'_>,
    source_id: ObjectId,
) -> Vec<ObjectId> {
    let zone = filter
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let mut ids: Vec<ObjectId> = crate::game::targeting::zone_object_ids(state, zone)
        .into_iter()
        .filter(|&id| matches_target_filter(state, id, filter, filter_ctx))
        .collect();
    // Drop the triggering object for an "other than" filter (Valakut's "five
    // other Mountains" — the newly-entered Mountain matches the per-object filter
    // as a pass-through and is removed here). The exclusion is the Oracle-text
    // semantic of "other"/"other than", not a numbered CR. Falls back to the
    // ability source when the trigger event carries no object subject.
    // Resolution-time prefers the live `current_trigger_event`; detection-time
    // uses the TLS override set by `resolve_quantity_for_trigger_check`.
    if filter_contains_other_than_trigger_object(filter) {
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
        ids.retain(|&id| id != triggering_id);
    }
    ids
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
    // path when no ability is in scope (X -> 0).
    //
    // CR 613.4c: The optional `recipient` from `QuantityContext` flows into
    // `FilterContext::recipient_id` so recipient-relative filter properties
    // resolve against the per-object recipient bound by the layer evaluator.
    let mut filter_ctx = match ability {
        // CR 109.5: "you"/"your" on a triggered ability refer to the ability's
        // (printed) controller. A quantity is a value sub-expression, not an effect
        // target: while a `player_scope` iteration rebinds `ability.controller` to
        // each affected player, "Zombies you control" in the quantity still means the
        // printed/original controller (The Scarab God upkeep X). CR 608.2c: the
        // ability's controller follows its instructions. Effect-target resolution
        // keeps the scoped controller via the bare `from_ability` constructor.
        Some(a) => FilterContext::from_ability_with_controller(
            a,
            a.original_controller.unwrap_or(a.controller),
        ),
        None => FilterContext::from_source_with_controller(source_id, controller),
    };
    filter_ctx.recipient_id = ctx.recipient;
    let player = state.players.iter().find(|p| p.id == controller);
    match qty {
        // CR 402: hand size for the scoped player(s).
        QuantityRef::HandSize { player: scope } => {
            // CR 608.2e (§8): a cross-player `AllPlayers` hand extremum is the
            // discard-clause equalization minimum — freeze it against the
            // clause's pre-clause board if a snapshot was captured. Single-
            // player scopes (Controller/ScopedPlayer/Target/...) re-resolve
            // live; only the aggregated form is snapshot.
            if matches!(scope, PlayerScope::AllPlayers { .. }) {
                if let Some(v) = state
                    .clause_minimum_snapshot
                    .as_ref()
                    .and_then(|s| s.get(qty))
                {
                    return v;
                }
            }
            resolve_per_player_scalar(state, scope, controller, ctx, targets, ability, |p| {
                usize_to_i32_saturating(p.hand.len())
            })
        }
        // CR 119: life total for the scoped player(s).
        QuantityRef::LifeTotal { player: scope } => {
            resolve_per_player_scalar(state, scope, controller, ctx, targets, ability, |p| p.life)
        }
        // CR 122.1: Counter-kind lookup summed across scope players. Controller
        // scope resolves to a single player; Opponents/All may span multiple.
        // Per-player u32 is widened to u64 before summing; the i32::try_from
        // saturates on the (only theoretically reachable) overflow.
        QuantityRef::PlayerCounter { kind, scope } => {
            let total: u64 = scoped_players(state, scope, ctx, controller)
                .map(|p| u64::from(p.player_counter(kind)))
                .sum();
            i32::try_from(total).unwrap_or(i32::MAX)
        }
        // CR 404: cards in the scoped player(s)' graveyard.
        QuantityRef::GraveyardSize { player: scope } => {
            resolve_per_player_scalar(state, scope, controller, ctx, targets, ability, |p| {
                usize_to_i32_saturating(p.graveyard.len())
            })
        }
        QuantityRef::LifeAboveStarting => {
            player.map_or(0, |p| p.life - state.format_config.starting_life)
        }
        // CR 103.4: The format's starting life total.
        QuantityRef::StartingLifeTotal => state.format_config.starting_life,
        // CR 118.4 + CR 119.3: Life lost this turn, scoped via PlayerScope (Π-3).
        QuantityRef::LifeLostThisTurn { player } => {
            resolve_per_player_scalar(state, player, controller, ctx, targets, ability, |p| {
                u32_to_i32_saturating(p.life_lost_this_turn)
            })
        }
        // CR 700.8: Number of creatures in `player`'s party. The maximum
        // assignment of creatures to the four party slots (Cleric/Rogue/
        // Warrior/Wizard) is computed per CR 700.8b for creatures with
        // multiple party-relevant types. Bounded to `0..=4`.
        QuantityRef::PartySize { player: scope } => {
            resolve_per_player_scalar(state, scope, controller, ctx, targets, ability, |p| {
                compute_party_size(state, p.id)
            })
        }
        // CR 702.179f: `player`'s current speed, treating no speed as 0.
        QuantityRef::Speed { player: scope } => {
            resolve_per_player_scalar(state, scope, controller, ctx, targets, ability, |p| {
                i32::from(effective_speed(state, p.id))
            })
        }
        QuantityRef::ObjectCount { filter } => usize_to_i32_saturating(
            // CR 400.1 + CR 603.4 + CR 109.3: count of the matching objects in
            // the filter's zone, with `OtherThanTriggerObject` exclusion applied
            // — shared with the "for each [object]" iteration so count and
            // members stay in lockstep.
            object_count_matching_ids(state, filter, &filter_ctx, source_id).len(),
        ),
        // CR 201.2 + CR 603.4: Count of objects matching `filter`,
        // deduplicated by the listed `qualities`. Each object contributes a
        // tuple-key formed from its values per quality; objects whose tuples
        // coincide count once. Objects with no value for a quality (empty
        // name, missing power, etc.) get a per-object sentinel for that
        // axis, so they are counted but not deduped against one another —
        // matching the legacy `ObjectCountDistinctNames` invariant for
        // unnamed objects.
        //
        // Lifts the legacy `ObjectCountDistinctNames` resolver onto the same
        // `Vec<SharedQuality>` axis used by
        // `SearchSelectionConstraint::DistinctQualities`. Both sides share
        // the `crate::game::filter::object_shared_quality_values` extractor,
        // keeping the count-expression and constraint vocabularies aligned.
        QuantityRef::ObjectCountDistinct { filter, qualities } => {
            let zone = filter
                .extract_in_zone()
                .unwrap_or(crate::types::zones::Zone::Battlefield);
            // Per-object signature: for each quality, a sorted Vec<String> of
            // the object's values for that quality. Empty values get a
            // per-object sentinel so unnamed/unstatted objects each count as
            // distinct on that axis (preserving the legacy invariant).
            let mut signatures: std::collections::HashSet<Vec<Vec<String>>> =
                std::collections::HashSet::new();
            for id in crate::game::targeting::zone_object_ids(state, zone) {
                if !matches_target_filter(state, id, filter, &filter_ctx) {
                    continue;
                }
                let Some(obj) = state.objects.get(&id) else {
                    continue;
                };
                let signature: Vec<Vec<String>> = qualities
                    .iter()
                    .map(|quality| {
                        let values = crate::game::filter::object_shared_quality_values_public(
                            obj,
                            quality,
                            &state.all_creature_types,
                        );
                        if values.is_empty() {
                            // Per-object sentinel: empty-value objects are
                            // each individually unique on this axis.
                            vec![format!("__unique_{}__", id.0)]
                        } else {
                            let mut sorted: Vec<String> = values.into_iter().collect();
                            sorted.sort();
                            sorted
                        }
                    })
                    .collect();
                signatures.insert(signature);
            }
            usize_to_i32_saturating(signatures.len())
        }
        // CR 109.3 + CR 205.3m: Group the matching object population by a
        // shared characteristic, such as creature type, and aggregate the
        // resulting bucket sizes. Each object contributes once to each distinct
        // value it has for `quality`; objects with no value for that quality
        // contribute to no bucket.
        QuantityRef::ObjectCountBySharedQuality {
            filter,
            quality,
            aggregate,
        } => {
            let mut buckets: HashMap<String, HashSet<ObjectId>> = HashMap::new();
            for id in object_count_matching_ids(state, filter, &filter_ctx, source_id) {
                let Some(obj) = state.objects.get(&id) else {
                    continue;
                };
                for value in crate::game::filter::object_shared_quality_values_public(
                    obj,
                    quality,
                    &state.all_creature_types,
                ) {
                    buckets.entry(value).or_default().insert(id);
                }
            }

            match aggregate {
                AggregateFunction::Max => buckets
                    .values()
                    .map(HashSet::len)
                    .max()
                    .map_or(0, usize_to_i32_saturating),
                AggregateFunction::Min => buckets
                    .values()
                    .map(HashSet::len)
                    .min()
                    .map_or(0, usize_to_i32_saturating),
                AggregateFunction::Sum => {
                    let unique_objects: HashSet<ObjectId> = buckets
                        .values()
                        .filter(|bucket| bucket.len() >= 2)
                        .flatten()
                        .copied()
                        .collect();
                    usize_to_i32_saturating(unique_objects.len())
                }
            }
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
        } => resolve_counters_on_scope(state, *scope, ctx, targets, ability, counter_type.as_ref()),
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
            ability,
            |obj| obj.power,
            |lki| lki.power,
        ),
        QuantityRef::Toughness { scope } => resolve_object_pt(
            state,
            *scope,
            ctx,
            targets,
            ability,
            |obj| obj.toughness,
            |lki| lki.toughness,
        ),
        QuantityRef::ObjectManaValue { scope } => {
            resolve_object_mana_value(state, *scope, ctx, targets, ability)
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
        QuantityRef::ObjectTypelineComponentCount { scope } => {
            resolve_object_typeline_component_count(state, *scope, ctx, targets)
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
        // Aggregate over objects matching `filter` in its zone; id population
        // delegated to `object_count_matching_ids` (single source of truth for
        // zone selection + the `OtherThanTriggerObject` exclusion). Selvala-class
        // "each other creature's power" excludes the triggering object via the
        // shared helper, so count and aggregate population are identical.
        //
        // CR 608.2h + CR 400.7: For look-back aggregates over non-battlefield
        // objects (Oversimplify: "the total power of creatures they controlled
        // that were exiled this way"), the live object's `power`/`toughness`
        // are `None` because the at-exile layer values are not maintained off
        // battlefield. Fall back to the LKI snapshot captured by
        // `change_zone` on leaving the battlefield (`game/zones.rs:65-92`),
        // which holds the post-layer-7 values from the moment of departure —
        // exactly what the "as they last existed on the battlefield" ruling
        // requires. `ManaValue` doesn't need LKI: the printed mana cost is
        // stable across zones.
        QuantityRef::Aggregate {
            function,
            property,
            filter,
        } => {
            let ids = object_count_matching_ids(state, filter, &filter_ctx, source_id);
            let extract = |id: ObjectId| -> Option<i32> {
                let live = state.objects.get(&id).and_then(|obj| match property {
                    ObjectProperty::Power => obj.power,
                    ObjectProperty::Toughness => obj.toughness,
                    // CR 202.3e: mana_value() excludes X. Always live — printed
                    // value is stable across zones, no LKI fallback needed.
                    ObjectProperty::ManaValue => {
                        Some(u32_to_i32_saturating(obj.mana_cost.mana_value()))
                    }
                });
                live.or_else(|| {
                    state.lki_cache.get(&id).and_then(|lki| match property {
                        ObjectProperty::Power => lki.power,
                        ObjectProperty::Toughness => lki.toughness,
                        ObjectProperty::ManaValue => Some(u32_to_i32_saturating(lki.mana_value)),
                    })
                })
            };
            let values = ids.iter().filter_map(|&id| extract(id));
            match function {
                AggregateFunction::Max => values.max().unwrap_or(0),
                AggregateFunction::Min => values.min().unwrap_or(0),
                AggregateFunction::Sum => values.sum(),
            }
        }
        // CR 107.1 + CR 700.1: min/max across players of the count of
        // battlefield objects matching `filter` each player controls.
        QuantityRef::ControlledByEachPlayer { filter, aggregate } => {
            // CR 608.2e (§8): prefer the clause-local snapshot if this clause
            // captured one — that freezes the extremum against the board as it
            // stood when the clause began, so an earlier APNAP player's
            // sacrifices do not shrink a later player's minimum.
            if let Some(v) = state
                .clause_minimum_snapshot
                .as_ref()
                .and_then(|s| s.get(qty))
            {
                return v;
            }
            // Battlefield-scoped: this variant carries no zone axis.
            let zone_ids = crate::game::targeting::zone_object_ids(
                state,
                crate::types::zones::Zone::Battlefield,
            );
            aggregate_over_players(state.players.iter(), *aggregate, |p| {
                // CR 109.5: evaluate `filter` as if `p` were "you" — count
                // battlefield objects `p` controls matching the filter. The
                // explicit `obj.controller == p.id` gate enforces the "they
                // control" semantics even when `filter` itself carries no
                // controller clause (Balance's Arm A parses a bare "lands"
                // type phrase); the rebound `FilterContext` additionally makes
                // any `controller: You` clause inside `filter` read `p`.
                let pctx = match ability {
                    Some(a) => FilterContext::from_ability_with_controller(a, p.id),
                    None => FilterContext::from_source_with_controller(source_id, p.id),
                };
                usize_to_i32_saturating(
                    zone_ids
                        .iter()
                        .filter(|&&id| {
                            state
                                .objects
                                .get(&id)
                                .is_some_and(|obj| obj.controller == p.id)
                                && matches_target_filter(state, id, filter, &pctx)
                        })
                        .count(),
                )
            })
        }
        QuantityRef::CountersOnObjects {
            counter_type,
            filter,
        } => {
            // CR 122.1: When `counter_type` is `None`, sum across every counter type
            // (e.g., "counters among artifacts and creatures you control"). When
            // `Some`, count only that specific counter type.
            let zone = filter
                .extract_in_zone()
                .unwrap_or(crate::types::zones::Zone::Battlefield);
            crate::game::targeting::zone_object_ids(state, zone)
                .iter()
                .filter_map(|&id| {
                    if matches_target_filter(state, id, filter, &filter_ctx) {
                        state.objects.get(&id).map(|obj| match counter_type {
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
                                let owner_matches = count_scope_owner_matches(
                                    state, scope, ctx, controller, obj.owner,
                                );
                                if owner_matches {
                                    for ct in &obj.card_types.core_types {
                                        seen.insert(*ct);
                                    }
                                }
                            }
                        }
                    }
                    ZoneRef::Graveyard | ZoneRef::Library | ZoneRef::Hand => {
                        for player in scoped_players(state, scope, ctx, controller) {
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
                    for player in scoped_players(state, scope, ctx, controller) {
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
                            let owner_matches =
                                count_scope_owner_matches(state, scope, ctx, controller, obj.owner);
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
        // CR 608.2c + CR 400.7: Count only the members of the most recent tracked
        // set that also satisfy the inner filter. Used for "for each nontoken
        // creature you controlled that was destroyed this way" — the tracked set
        // holds all destroyed creatures; the filter narrows to controlled nontokens.
        QuantityRef::FilteredTrackedSetSize { filter } => {
            let Some((_, ids)) = state.tracked_object_sets.iter().max_by_key(|(id, _)| id.0) else {
                return 0;
            };
            let count = ids
                .iter()
                .filter(|&&oid| {
                    crate::game::filter::matches_target_filter(state, oid, filter, &filter_ctx)
                })
                .count();
            usize_to_i32_saturating(count)
        }
        // CR 400.7 + CR 608.2c: Read the per-resolution counter populated by
        // ChangeZoneAll when it exiles cards from a hand. Used by "draws a card
        // for each card exiled from their hand this way" (Deadly Cover-Up).
        QuantityRef::ExiledFromHandThisResolution => {
            u32_to_i32_saturating(state.exiled_from_hand_this_resolution)
        }
        // CR 603.2c: Numeric value carried by the triggering event,
        // resolution-precedence ordered:
        //
        //   1. `current_trigger_match_count` — the filtered subject count of a
        //      batched trigger ("one or more <FILTER> <verb>"), set by
        //      `stack::resolve_top` for the resolution. This is the canonical
        //      "that many" for Ur-Dragon-style batched triggers; without it
        //      the `extract_amount_from_event` cascade below falls through to
        //      0 on `AttackersDeclared` and similar batched events.
        //   2. CR 706.4: `die_result_this_resolution` — die results recorded
        //      earlier in THIS resolution (no results table) outrank the
        //      triggering event's own amount, so "roll one or more dice.
        //      <effect> equal to the result(s)" consumes the roll total, not
        //      the combat damage / life change that triggered it.
        //   3. `extract_amount_from_event(current_trigger_event)` — scalar
        //      events with an inherent amount (damage dealt, life changed,
        //      cards drawn, counters added/removed, die rolls).
        //   4. `last_effect_counts_by_player` — APNAP per-player counts from
        //      the preceding effect in the same resolution.
        //   5. `last_effect_count` / `last_effect_amount` — sub_ability
        //      continuation fallbacks (e.g. "discard up to N, then draw that
        //      many"; "dealt excess damage this way, add that much {R}").
        //   6. `0` — undefined.
        QuantityRef::EventContextAmount => state
            .current_trigger_match_count
            .map(u32_to_i32_saturating)
            // CR 706.4: Die results recorded earlier in THIS resolution
            // outrank the triggering event's own amount, so "roll one or more
            // dice. <effect> equal to the result(s)" consumes the roll total,
            // not the combat damage / life change that triggered it.
            .or(state.die_result_this_resolution)
            .or_else(|| {
                state
                    .current_trigger_event
                    .as_ref()
                    .and_then(crate::game::targeting::extract_amount_from_event)
            })
            .or_else(|| {
                ctx.scoped_player.and_then(|player| {
                    (!state.last_effect_counts_by_player.is_empty()).then(|| {
                        state
                            .last_effect_counts_by_player
                            .get(&player)
                            .copied()
                            .unwrap_or(0)
                    })
                })
            })
            .or(state.last_effect_count)
            .or(state.last_effect_amount)
            .unwrap_or(0),
        // CR 608.2c: If an earlier effect in this same resolution captured an
        // explicit object context, use that object before the original trigger
        // event. This covers "sacrifice another creature. ... that creature's
        // power" where the ETB trigger event source is the entering permanent,
        // not the sacrificed object.
        //
        // CR 400.7: Falls back to LKI cache for objects that have left their zone.
        // CR 608.2k cost-paid / trigger-condition referents are now resolved
        // via `QuantityRef::Power/Toughness/ObjectManaValue { scope:
        // ObjectScope::CostPaidObject }` (see `resolve_object_pt` /
        // `resolve_object_mana_value`).
        //
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
        // CR 122.1: Count distinct counter kinds among permanents matching the
        // filter (controller-relative, CR 109.4). Counter-side dual of
        // `DistinctColorsAmongPermanents`. Each `CounterType` present on at
        // least one matching permanent contributes once.
        QuantityRef::DistinctCounterKindsAmong { filter } => {
            usize_to_i32_saturating(distinct_counter_kinds_among(state, filter, &filter_ctx).len())
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
                        ControllerRef::ScopedPlayer => {
                            obj.controller == scoped_player_or_controller(ability, controller)
                        }
                        ControllerRef::TargetPlayer => target_player == Some(obj.controller),
                        ControllerRef::ParentTargetController => ability
                            .and_then(|ability| {
                                crate::game::ability_utils::parent_target_controller(ability, state)
                            })
                            .is_some_and(|player| player == obj.controller),
                        ControllerRef::DefendingPlayer => {
                            crate::game::combat::defending_player_for_attacker(state, ctx.source)
                                .is_some_and(|pid| pid == obj.controller)
                        }
                        // CR 613.1: Land controlled by the source's chosen player.
                        ControllerRef::SourceChosenPlayer => {
                            crate::game::game_object::source_chosen_player(state, ctx.source)
                                .is_some_and(|pid| pid == obj.controller)
                        }
                        // CR 608.2c + CR 109.4: Land controlled by a chosen player.
                        ControllerRef::ChosenPlayer { index } => ability
                            .and_then(|a| a.chosen_players.get(*index as usize).copied())
                            .is_some_and(|pid| pid == obj.controller),
                        // CR 603.2 + CR 109.4: Land controlled by the triggering player.
                        ControllerRef::TriggeringPlayer => {
                            triggering_event_player(state).is_some_and(|pid| pid == obj.controller)
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
        // CR 117.1: Count spells cast this turn by the scoped players, optionally filtered.
        QuantityRef::SpellsCastThisTurn { scope, ref filter } => usize_to_i32_saturating(
            scoped_players(state, scope, ctx, controller)
                .filter_map(|player| state.spells_cast_this_turn_by_player.get(&player.id))
                .map(|list| match filter {
                    None => list.len(),
                    Some(filter) => list
                        .iter()
                        .filter(|record| {
                            spell_record_matches_filter(
                                record,
                                filter,
                                controller,
                                &state.all_creature_types,
                            )
                        })
                        .count(),
                })
                .sum(),
        ),
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
        QuantityRef::SacrificedThisTurn { player, ref filter } => resolve_per_player_scalar(
            state,
            player,
            controller,
            ctx,
            targets,
            ability,
            |scoped_player| {
                usize_to_i32_saturating(
                    state
                        .sacrificed_permanents_this_turn
                        .iter()
                        .filter(|record| {
                            record.controller == scoped_player.id
                                && matches_target_filter_on_zone_change_record(
                                    state,
                                    record,
                                    filter,
                                    &filter_ctx,
                                )
                        })
                        .count(),
                )
            },
        ),
        // CR 710.2: Crimes committed this turn — uses tracked counter on player.
        QuantityRef::CrimesCommittedThisTurn => {
            player.map_or(0, |p| u32_to_i32_saturating(p.crimes_committed_this_turn))
        }
        // CR 119.4: Life gained this turn, scoped via PlayerScope (Π-4).
        QuantityRef::LifeGainedThisTurn { player } => {
            resolve_per_player_scalar(state, player, controller, ctx, targets, ability, |p| {
                u32_to_i32_saturating(p.life_gained_this_turn)
            })
        }
        // CR 121.1: Cards drawn this turn, scoped via PlayerScope.
        QuantityRef::CardsDrawnThisTurn { player } => {
            resolve_per_player_scalar(state, player, controller, ctx, targets, ability, |p| {
                u32_to_i32_saturating(p.cards_drawn_this_turn)
            })
        }
        // CR 305.2a + CR 603.4: Lands played this turn by the scoped player.
        QuantityRef::LandsPlayedThisTurn { player, from_zones } => {
            resolve_per_player_scalar(state, player, controller, ctx, targets, ability, |p| {
                from_zones.as_ref().map_or_else(
                    || i32::from(p.lands_played_this_turn),
                    |zones| {
                        usize_to_i32_saturating(
                            state.lands_played_this_turn_by_player.get(&p.id).map_or(
                                0,
                                |records| {
                                    records
                                        .iter()
                                        .filter(|record| zones.contains(&record.from_zone))
                                        .count()
                                },
                            ),
                        )
                    },
                )
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
        // CR 120.1 + CR 120.9 + CR 603.4: Damage dealt this turn matching the
        // supplied source/target filters. `group_by` selects whether records are
        // partitioned (per CR 120.9 "by a specific source") before `aggregate`
        // collapses each group's sum into a single value.
        QuantityRef::DamageDealtThisTurn {
            source,
            target,
            aggregate,
            group_by,
            damage_kind,
        } => resolve_damage_dealt_this_turn(
            state,
            controller,
            ctx,
            ability,
            &filter_ctx,
            source,
            target,
            *aggregate,
            *group_by,
            *damage_kind,
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
        // CR 508.1a: Count creatures the controller attacked with this turn.
        QuantityRef::AttackedThisTurn => state
            .attacking_creatures_this_turn
            .get(&controller)
            .copied()
            .map(u32_to_i32_saturating)
            .unwrap_or(0),
        // CR 603.4: Whether the controller descended this turn.
        QuantityRef::DescendedThisTurn => {
            if player.is_some_and(|p| p.descended_this_turn) {
                1
            } else {
                0
            }
        }
        // CR 606.1 + CR 603.4: Per-player loyalty-ability activation count for
        // the current turn. Reads from `GameState::loyalty_abilities_activated_this_turn`,
        // a player-id-keyed counter populated in
        // `planeswalker::record_loyalty_activation`.
        QuantityRef::LoyaltyAbilitiesActivatedThisTurn { player: scope } => {
            resolve_per_player_scalar(state, scope, controller, ctx, targets, ability, |p| {
                state
                    .loyalty_abilities_activated_this_turn
                    .get(&p.id)
                    .copied()
                    .map(u32_to_i32_saturating)
                    .unwrap_or(0)
            })
        }
        // CR 117.1: Total spells cast last turn (by any player).
        QuantityRef::SpellsCastLastTurn => state.spells_cast_last_turn.map_or(0, i32::from),
        // CR 117.1 + CR 601.2: Number of spells cast this game by the scoped
        // players, optionally filtered by spell characteristics.
        //
        // `filter: None` reads the fast O(1) `state.spells_cast_this_game`
        // count — preserves the pre-lift Establishing Shot semantics.
        // `filter: Some(_)` scans `state.spells_cast_this_game_by_player`
        // records, mirroring `SpellsCastThisTurn`'s filtered scan.
        QuantityRef::SpellsCastThisGame { scope, ref filter } => match filter {
            None => usize_to_i32_saturating(
                scoped_players(state, scope, ctx, controller)
                    .filter_map(|p| state.spells_cast_this_game.get(&p.id))
                    .map(|n| *n as usize)
                    .sum(),
            ),
            Some(filter) => usize_to_i32_saturating(
                scoped_players(state, scope, ctx, controller)
                    .filter_map(|p| state.spells_cast_this_game_by_player.get(&p.id))
                    .map(|list| {
                        list.iter()
                            .filter(|record| {
                                spell_record_matches_filter(
                                    record,
                                    filter,
                                    controller,
                                    &state.all_creature_types,
                                )
                            })
                            .count()
                    })
                    .sum(),
            ),
        },
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
                    counter_added_actor_matches(state, actor, ctx, controller, record.actor)
                        && counters.matches(&record.counter_type)
                        && matches_target_filter_on_counter_added_record(
                            state,
                            record,
                            target,
                            &filter_ctx,
                        )
                })
                .fold(0, |total: u32, record| total.saturating_add(record.count)),
        ),
        // CR 701.9 + CR 603.4: Cards discarded this turn, scoped via PlayerScope.
        QuantityRef::CardsDiscardedThisTurn { player } => {
            resolve_per_player_scalar(state, player, controller, ctx, targets, ability, |p| {
                u32_to_i32_saturating(
                    state
                        .cards_discarded_this_turn_by_player
                        .get(&p.id)
                        .copied()
                        .unwrap_or_default(),
                )
            })
        }
        QuantityRef::TokensCreatedThisTurn { player, ref filter } => resolve_per_player_scalar(
            state,
            player,
            controller,
            ctx,
            targets,
            ability,
            |scoped_player| {
                usize_to_i32_saturating(
                    state
                        .created_tokens_this_turn
                        .iter()
                        .filter(|record| {
                            record.controller == scoped_player.id
                                && matches_target_filter_on_zone_change_record(
                                    state,
                                    record,
                                    filter,
                                    &filter_ctx,
                                )
                        })
                        .count(),
                )
            },
        ),
        QuantityRef::PlayerActionsThisTurn { player, action } => resolve_per_player_scalar(
            state,
            player,
            controller,
            ctx,
            targets,
            ability,
            |scoped_player| {
                usize_to_i32_saturating(
                    state
                        .player_actions_this_turn
                        .iter()
                        .filter(|(player_id, recorded_action)| {
                            *player_id == scoped_player.id && recorded_action == action
                        })
                        .count(),
                )
            },
        ),
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
        QuantityRef::AdditionalCostPaymentCount => state
            .objects
            .get(&ctx.self_object())
            .map(|obj| u32_to_i32_saturating(obj.additional_cost_payment_count))
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
                        Some(ControllerRef::ScopedPlayer) => {
                            snap.controller == scoped_player_or_controller(ability, controller)
                        }
                        Some(ControllerRef::TargetPlayer) => ability
                            .and_then(|a| {
                                a.targets.iter().find_map(|t| match t {
                                    crate::types::ability::TargetRef::Player(pid) => Some(*pid),
                                    crate::types::ability::TargetRef::Object(_) => None,
                                })
                            })
                            .is_some_and(|pid| pid == snap.controller),
                        Some(ControllerRef::ParentTargetController) => ability
                            .and_then(|a| {
                                crate::game::ability_utils::parent_target_controller(a, state)
                            })
                            .is_some_and(|pid| pid == snap.controller),
                        Some(ControllerRef::DefendingPlayer) => {
                            crate::game::combat::defending_player_for_attacker(state, ctx.source)
                                .is_some_and(|pid| pid == snap.controller)
                        }
                        // CR 613.1: Attachment controlled by the source's chosen player.
                        Some(ControllerRef::SourceChosenPlayer) => {
                            crate::game::game_object::source_chosen_player(state, ctx.source)
                                .is_some_and(|pid| pid == snap.controller)
                        }
                        // CR 608.2c + CR 109.4: Attachment controlled by a chosen player.
                        Some(ControllerRef::ChosenPlayer { index }) => ability
                            .and_then(|a| a.chosen_players.get(*index as usize).copied())
                            .is_some_and(|pid| pid == snap.controller),
                        // CR 603.2 + CR 109.4: Attachment controlled by the triggering player.
                        Some(ControllerRef::TriggeringPlayer) => {
                            triggering_event_player(state).is_some_and(|pid| pid == snap.controller)
                        }
                    })
                    .count(),
            )
        }
    }
}

fn scoped_player_or_controller(
    ability: Option<&ResolvedAbility>,
    controller: PlayerId,
) -> PlayerId {
    ability
        .and_then(|ability| ability.scoped_player)
        .unwrap_or(controller)
}

fn damage_source_controller_matches(
    state: &GameState,
    actual: PlayerId,
    controller: PlayerId,
    ctx: QuantityContext,
    ability: Option<&ResolvedAbility>,
    expected: &ControllerRef,
) -> bool {
    match expected {
        ControllerRef::You => actual == controller,
        ControllerRef::Opponent => actual != controller,
        ControllerRef::ScopedPlayer => actual == scoped_player_or_controller(ability, controller),
        ControllerRef::TargetPlayer => ability
            .and_then(|ability| {
                ability.targets.iter().find_map(|target| match target {
                    TargetRef::Player(player) => Some(*player),
                    TargetRef::Object(_) => None,
                })
            })
            .is_some_and(|player| actual == player),
        ControllerRef::ParentTargetController => ability
            .and_then(|ability| {
                crate::game::ability_utils::parent_target_controller(ability, state)
            })
            .is_some_and(|player| actual == player),
        ControllerRef::DefendingPlayer => {
            crate::game::combat::defending_player_for_attacker(state, ctx.source)
                .is_some_and(|player| actual == player)
        }
        // CR 613.1: Damage source controlled by the source's chosen player.
        ControllerRef::SourceChosenPlayer => {
            crate::game::game_object::source_chosen_player(state, ctx.source)
                .is_some_and(|player| actual == player)
        }
        // CR 608.2c + CR 109.4: Damage source controlled by a chosen player.
        ControllerRef::ChosenPlayer { index } => ability
            .and_then(|ability| ability.chosen_players.get(*index as usize).copied())
            .is_some_and(|player| actual == player),
        // CR 603.2 + CR 109.4: Damage source controlled by the triggering player.
        ControllerRef::TriggeringPlayer => {
            triggering_event_player(state).is_some_and(|player| actual == player)
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

/// CR 608.2 + CR 109.5: Resolve which player a `CountScope` variant binds to,
/// then return an iterator over matching players.
///
/// `Controller` always means the printed ability's controller (caster) — this
/// is the "you" axis. `ScopedPlayer` (Issue #310) means the currently iterated
/// player when a surrounding `player_scope` is active, falling back to
/// `Controller` outside iteration. `Opponents` and `All` are computed
/// relative to `Controller` (the caster), preserving CR 109.5's "you" semantics
/// for unscoped opponent/all counts.
fn scoped_players<'a>(
    state: &'a GameState,
    scope: &'a CountScope,
    ctx: QuantityContext,
    controller: PlayerId,
) -> impl Iterator<Item = &'a crate::types::player::Player> {
    let scoped_player = ctx
        .scoped_player
        .or_else(|| triggering_event_player(state))
        .unwrap_or(controller);
    state.players.iter().filter(move |p| match scope {
        CountScope::Controller | CountScope::Owner => p.id == controller,
        CountScope::ScopedPlayer => p.id == scoped_player,
        CountScope::SourceChosenPlayer => {
            crate::game::game_object::source_chosen_player(state, ctx.source)
                .is_some_and(|player| p.id == player)
        }
        CountScope::All => true,
        CountScope::Opponents => p.id != controller,
    })
}

/// CR 608.2 + CR 109.5: Owner-axis owner-match for `CountScope` against a
/// known object owner. Mirrors `scoped_players` for global zones (exile)
/// where iteration over players is replaced by per-object owner predication.
fn count_scope_owner_matches(
    state: &GameState,
    scope: &CountScope,
    ctx: QuantityContext,
    controller: PlayerId,
    owner: PlayerId,
) -> bool {
    match scope {
        CountScope::Controller | CountScope::Owner => owner == controller,
        CountScope::ScopedPlayer => owner == ctx.scoped_player.unwrap_or(controller),
        CountScope::SourceChosenPlayer => {
            crate::game::game_object::source_chosen_player(state, ctx.source)
                .is_some_and(|player| owner == player)
        }
        CountScope::All => true,
        CountScope::Opponents => owner != controller,
    }
}

fn counter_added_actor_matches(
    state: &GameState,
    scope: &CountScope,
    ctx: QuantityContext,
    controller: PlayerId,
    actor: PlayerId,
) -> bool {
    match scope {
        CountScope::Controller | CountScope::Owner => actor == controller,
        CountScope::ScopedPlayer => actor == ctx.scoped_player.unwrap_or(controller),
        CountScope::SourceChosenPlayer => {
            crate::game::game_object::source_chosen_player(state, ctx.source)
                .is_some_and(|player| actor == player)
        }
        CountScope::All => true,
        CountScope::Opponents => actor != controller,
    }
}

/// CR 120.9 + CR 608.2i + CR 608.2h: Match a damage record's source against
/// `filter` using the source's snapshot captured at damage time (look-back —
/// criteria need not still hold). `TargetFilter::Any` short-circuits.
fn damage_record_source_matches(
    state: &GameState,
    record: &DamageRecord,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    if matches!(filter, TargetFilter::Any) {
        return true;
    }
    matches_target_filter_on_damage_record_source(state, record, filter, ctx)
}

fn damage_record_matches_kind(
    record: &crate::types::game_state::DamageRecord,
    damage_kind: crate::types::ability::DamageKindFilter,
) -> bool {
    use crate::types::ability::DamageKindFilter;
    match damage_kind {
        DamageKindFilter::Any => true,
        DamageKindFilter::CombatOnly => record.is_combat,
        DamageKindFilter::NoncombatOnly => !record.is_combat,
    }
}

/// CR 120.1 + CR 120.9 + CR 603.4: Resolver for `QuantityRef::DamageDealtThisTurn`.
///
/// Walks `state.damage_dealt_this_turn`, filters records whose source/target
/// match the supplied filters, then either sums every match (no `group_by`) or
/// partitions by the group key, sums each partition, and applies `aggregate`
/// across the per-group sums (CR 120.9 "by a specific source").
#[allow(clippy::too_many_arguments)]
fn resolve_damage_dealt_this_turn(
    state: &GameState,
    controller: PlayerId,
    ctx: QuantityContext,
    ability: Option<&ResolvedAbility>,
    filter_ctx: &FilterContext<'_>,
    source: &TargetFilter,
    target: &TargetFilter,
    aggregate: AggregateFunction,
    group_by: Option<crate::types::ability::DamageGroupKey>,
    damage_kind: crate::types::ability::DamageKindFilter,
) -> i32 {
    use crate::types::ability::DamageGroupKey;

    // CR 120.9 + CR 608.2i + CR 608.2h: Match the source filter (including any
    // `controller` predicate) against each record's source snapshot captured at
    // damage time, not the live object. The snapshot carries the source's
    // event-time controller, type, and characteristics, so a control change,
    // type change, or the source leaving the battlefield after damage still
    // answers the rules-correct look-back question. The controller predicate is
    // served by the snapshot's controller via the matcher's controller arm — no
    // separate `split_controller_filter` lift is needed on the source side.
    let source_matches =
        |record: &DamageRecord| damage_record_source_matches(state, record, source, filter_ctx);

    let matching = state.damage_dealt_this_turn.iter().filter(|record| {
        damage_record_matches_kind(record, damage_kind)
            && source_matches(record)
            && damage_record_target_matches(
                state, record, controller, ctx, ability, target, filter_ctx,
            )
    });

    match group_by {
        // No grouping: every matching record is a single bucket, so `aggregate`
        // collapses to a sum (Max/Min/Sum over a one-element set all coincide
        // with the total sum).
        None => u32_to_i32_saturating(matching.map(|record| record.amount).sum()),
        Some(DamageGroupKey::SourceId) => {
            let mut totals: HashMap<ObjectId, u32> = HashMap::new();
            for record in matching {
                totals
                    .entry(record.source_id)
                    .and_modify(|total| *total = total.saturating_add(record.amount))
                    .or_insert(record.amount);
            }
            let aggregated: Option<u32> = match aggregate {
                AggregateFunction::Max => totals.values().copied().max(),
                AggregateFunction::Min => totals.values().copied().min(),
                AggregateFunction::Sum => Some(totals.values().copied().sum()),
            };
            aggregated.map(u32_to_i32_saturating).unwrap_or(0)
        }
    }
}

/// Split a target/source filter into (controller-stripped clone, lifted controller).
///
/// CR 120.9: The controller predicate on a damage-history participant filter
/// must be evaluated against the `DamageRecord` participant controller snapshot,
/// not against the live object's controller — control of an object can change
/// between damage and check. Returns `(None, None)` when the filter has no
/// controller predicate to lift, so callers can use the original filter
/// reference without a heap allocation.
fn split_controller_filter(filter: &TargetFilter) -> (Option<TargetFilter>, Option<ControllerRef>) {
    match filter {
        TargetFilter::Typed(typed) if typed.controller.is_some() => {
            let mut stripped = typed.clone();
            let controller = stripped.controller.take();
            (Some(TargetFilter::Typed(stripped)), controller)
        }
        _ => (None, None),
    }
}

fn damage_record_target_matches(
    state: &GameState,
    record: &crate::types::game_state::DamageRecord,
    controller: PlayerId,
    quantity_ctx: QuantityContext,
    ability: Option<&ResolvedAbility>,
    filter: &TargetFilter,
    filter_ctx: &FilterContext<'_>,
) -> bool {
    match record.target {
        TargetRef::Object(object_id) => {
            let (live_target_filter, lki_controller) = split_controller_filter(filter);
            if let Some(expected) = lki_controller.as_ref() {
                if !damage_source_controller_matches(
                    state,
                    record.target_controller,
                    controller,
                    quantity_ctx,
                    ability,
                    expected,
                ) {
                    return false;
                }
            }
            let live_target_filter_ref: &TargetFilter =
                live_target_filter.as_ref().unwrap_or(filter);
            matches_target_filter(state, object_id, live_target_filter_ref, filter_ctx)
        }
        TargetRef::Player(player_id) => player_matches_target_filter_in_state(
            state,
            filter,
            player_id,
            filter_ctx.source_controller,
        ),
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
        // CR 603.4: an intervening-if condition is checked at trigger detection
        // (current_trigger_event is None then) and re-checked on resolution.
        // EventSource-scoped quantities must resolve at BOTH times — fall back to
        // the detection-time event TLS, mirroring triggering_event_player.
        ObjectScope::EventSource => state
            .current_trigger_event
            .as_ref()
            .cloned()
            .or_else(detection_trigger_event)
            .and_then(|e| crate::game::targeting::extract_source_from_event(&e))
            .and_then(|id| state.objects.get(&id)),
        // CR 608.2k / CR 608.2c: object-*identity* lookup. Neither
        // `CostPaidObject` (cost referent) nor `Anaphoric` (instruction-order
        // referent) resolves to a live `GameObject` here — both are snapshot
        // referents read through `ability` slots, not `state.objects`.
        ObjectScope::CostPaidObject | ObjectScope::Anaphoric | ObjectScope::Demonstrative => None,
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
        // CR 603.4: an intervening-if condition is checked at trigger detection
        // (current_trigger_event is None then) and re-checked on resolution.
        // EventSource-scoped quantities must resolve at BOTH times — fall back to
        // the detection-time event TLS, mirroring triggering_event_player.
        ObjectScope::EventSource => state
            .current_trigger_event
            .as_ref()
            .cloned()
            .or_else(detection_trigger_event)
            .and_then(|e| crate::game::targeting::extract_source_from_event(&e)),
        // CR 608.2k / CR 608.2c: object-*identity* lookup. Neither
        // `CostPaidObject` (cost referent) nor `Anaphoric` (instruction-order
        // referent) resolves to an `ObjectId` here — both are snapshot
        // referents read through `ability` slots, not `state.objects`.
        ObjectScope::CostPaidObject | ObjectScope::Anaphoric | ObjectScope::Demonstrative => None,
    }
}

/// CR 122.1: Distinct counter kinds present on permanents matching `filter`
/// (controller-relative, CR 109.4). Mirrors `DistinctColorsAmongPermanents`'s
/// resolver (zone from `filter.extract_in_zone()`, `zone_object_ids`,
/// `matches_target_filter`), enumerating only positive-count counter kinds the
/// same way proliferate does. Returns a `Vec<CounterType>` SORTED by
/// `CounterType::as_str`
/// for determinism — `CounterType` has no `Ord` (and must not gain one), so the
/// underlying `HashSet` iteration order is nondeterministic and would cause
/// replay desync if returned directly. Used both as the `len()` source for
/// `QuantityRef::DistinctCounterKindsAmong` resolution and as the per-iteration
/// kind sequence for `repeat_for` counter-kind loops.
pub(crate) fn distinct_counter_kinds_among(
    state: &GameState,
    filter: &TargetFilter,
    filter_ctx: &FilterContext<'_>,
) -> Vec<CounterType> {
    let zone = filter
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let mut seen: HashSet<CounterType> = HashSet::new();
    for &id in crate::game::targeting::zone_object_ids(state, zone).iter() {
        if !matches_target_filter(state, id, filter, filter_ctx) {
            continue;
        }
        if let Some(obj) = state.objects.get(&id) {
            for counter_type in positive_counter_types(&obj.counters) {
                seen.insert(counter_type);
            }
        }
    }
    let mut kinds: Vec<CounterType> = seen.into_iter().collect();
    kinds.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    kinds
}

pub(crate) fn counter_count_from_map(
    counters: &HashMap<CounterType, u32>,
    counter_type: Option<&CounterType>,
) -> i32 {
    match counter_type {
        Some(ct) => u32_to_i32_saturating(counters.get(ct).copied().unwrap_or(0)),
        None => u32_to_i32_saturating(counters.values().copied().sum::<u32>()),
    }
}

fn resolve_counters_on_scope(
    state: &GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
    ability: Option<&ResolvedAbility>,
    counter_type: Option<&CounterType>,
) -> i32 {
    match scope {
        // CR 122.2 + CR 400.7 + CR 603.10a: When the source or triggering
        // event source has changed zones (e.g., a dies-trigger reading
        // "counters on ~"), `obj.counters` has been cleared by
        // `apply_zone_exit_cleanup`. The LKI snapshot captured there
        // preserves the pre-exit counter map per CR 400.7's "new object"
        // semantics.
        //
        // The fallback must be zone-keyed, not presence-keyed: an object that
        // died and was returned earlier this turn keeps both a stale LKI entry
        // (from the death) and a live counter map (post-return). A live
        // battlefield object's `obj.counters` is authoritative; only when the
        // source has changed zones (so it isn't on the battlefield as the
        // "same" object) does CR 603.10a's look-back apply.
        //
        // Mirrors `resolve_object_pt`'s LKI fallback for power/toughness
        // (where the cleared field becomes `None`); the counter analogue must
        // be zone-keyed because an empty `HashMap<CounterType, u32>` is
        // `Some({})`, not `None`.
        ObjectScope::Source | ObjectScope::EventSource => {
            let Some(object_id) = object_id_for_scope(state, scope, ctx, targets) else {
                return 0;
            };
            let live = state.objects.get(&object_id);
            let on_battlefield =
                live.is_some_and(|obj| obj.zone == crate::types::zones::Zone::Battlefield);
            if !on_battlefield {
                if let Some(lki) = state.lki_cache.get(&object_id) {
                    return counter_count_from_map(&lki.counters, counter_type);
                }
            }
            live.map(|obj| counter_count_from_map(&obj.counters, counter_type))
                .unwrap_or(0)
        }
        ObjectScope::CostPaidObject => ability
            .and_then(|ability| ability.cost_paid_object.as_ref())
            .map(|snapshot| counter_count_from_map(&snapshot.lki.counters, counter_type))
            .unwrap_or(0),
        _ => object_for_scope(state, scope, ctx, targets)
            .map(|obj| counter_count_from_map(&obj.counters, counter_type))
            .unwrap_or(0),
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

fn resolve_object_typeline_component_count(
    state: &GameState,
    scope: ObjectScope,
    ctx: QuantityContext,
    targets: &[TargetRef],
) -> i32 {
    let Some(object_id) = object_id_for_scope(state, scope, ctx, targets) else {
        return 0;
    };
    if let Some(obj) = state.objects.get(&object_id) {
        let ct = &obj.card_types;
        return usize_to_i32_saturating(
            ct.supertypes.len() + ct.core_types.len() + ct.subtypes.len(),
        );
    }
    state
        .lki_cache
        .get(&object_id)
        .map(|lki| {
            usize_to_i32_saturating(
                lki.supertypes.len() + lki.card_types.len() + lki.subtypes.len(),
            )
        })
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
    ability: Option<&ResolvedAbility>,
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
        // CR 608.2k: An ability's effect referring to a specific untargeted
        // object previously referred to by that ability's cost OR trigger
        // condition still affects it. Resolved (first match wins) via:
        //   1. `cost_paid_object` — canonical activated/cast sacrifice-cost
        //      referent (Greater Good).
        //   2. trigger-event source — the object named by this ability's
        //      trigger condition (Hamletback Goliath, Conclave Mentor), live
        //      object then LKI for dies/leaves-battlefield triggers.
        //   3. `effect_context_object` — effect-driven sacrifices captured
        //      mid-resolution (Fire Lord Ozai, The Meep, Venom, Broadside
        //      Bombardiers).
        // Slots 1 and 3 are PINNED in this order by the
        // `resolve_object_mana_value` regression guard; slot 2 is inserted
        // strictly between them (insert-only — never reorders 1 vs 3). CR
        // 608.2k names cost and trigger referents but does not adjudicate
        // priority between them; the engine's pinned `cost_paid_object`-first
        // choice stands. Exact parity with the `resolve_object_mana_value`
        // `CostPaidObject` arm.
        ObjectScope::CostPaidObject => ability
            .and_then(|a| a.cost_paid_object.as_ref())
            .and_then(|snapshot| lki_extract(&snapshot.lki))
            .or_else(|| {
                object_id_for_scope(state, ObjectScope::EventSource, ctx, targets).and_then(|id| {
                    state
                        .objects
                        .get(&id)
                        .and_then(&obj_extract)
                        .or_else(|| state.lki_cache.get(&id).and_then(&lki_extract))
                })
            })
            .or_else(|| {
                ability
                    .and_then(|a| a.effect_context_object.as_ref())
                    .and_then(|snapshot| lki_extract(&snapshot.lki))
            })
            .unwrap_or(0),
        // CR 608.2c: An anaphoric pronoun ("its power") in a triggered ability
        // binds to the object introduced by the most recent earlier *effect
        // instruction* in the same ability (slot 1: `effect_context_object`).
        // CR 608.2k: if no such instruction exists, fall back to the
        // trigger-condition referent (slot 2: trigger-event source) then the
        // cost referent (slot 3: `cost_paid_object`). The arm differs from
        // `CostPaidObject` only in slot priority — instruction-order (608.2c)
        // first, vs. cost referent (608.2k) first. `Demonstrative` ("that
        // creature's toughness") shares this resolution — same earlier-
        // instruction referent, named by a full noun phrase rather than "its".
        ObjectScope::Anaphoric | ObjectScope::Demonstrative => ability
            .and_then(|a| a.effect_context_object.as_ref())
            .and_then(|snapshot| lki_extract(&snapshot.lki))
            .or_else(|| {
                object_id_for_scope(state, ObjectScope::EventSource, ctx, targets).and_then(|id| {
                    state
                        .objects
                        .get(&id)
                        .and_then(&obj_extract)
                        .or_else(|| state.lki_cache.get(&id).and_then(&lki_extract))
                })
            })
            .or_else(|| {
                ability
                    .and_then(|a| a.cost_paid_object.as_ref())
                    .and_then(|snapshot| lki_extract(&snapshot.lki))
            })
            .unwrap_or(0),
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
    ability: Option<&ResolvedAbility>,
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
        // CR 608.2k + CR 400.7j + CR 701.21a: The "cost-paid object" mana
        // value resolves (first match wins) via:
        //   1. `cost_paid_object` — canonical activated/cast-cost referent
        //      (Food Chain, Burnt Offering, Dark Confidant).
        //   2. trigger-event source — the object named by this ability's
        //      trigger condition, live object then LKI.
        //   3. `effect_context_object` — when a `Sacrifice` *effect* (not a
        //      cost) appears mid-resolution (Birthing Ritual: "you may
        //      sacrifice a creature. If you do, ..., where X is 1 plus the
        //      sacrificed creature's mana value"), the sacrificed permanent is
        //      captured into `effect_context_object` by the `EffectZoneChoice`
        //      handler.
        // Slots 1 and 3 are PINNED in this order by the
        // `resolve_object_mana_value_cost_paid_object_takes_priority_over_effect_context`
        // regression guard; slot 2 is inserted strictly between them
        // (insert-only). Exact parity with the `resolve_object_pt`
        // `CostPaidObject` arm.
        ObjectScope::CostPaidObject => ability
            .and_then(|a| a.cost_paid_object.as_ref())
            .map(|snapshot| u32_to_i32_saturating(snapshot.lki.mana_value))
            .or_else(|| {
                object_id_for_scope(state, ObjectScope::EventSource, ctx, targets).and_then(|id| {
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
            })
            .or_else(|| {
                ability
                    .and_then(|a| a.effect_context_object.as_ref())
                    .map(|snapshot| u32_to_i32_saturating(snapshot.lki.mana_value))
            })
            .unwrap_or(0),
        // CR 608.2c: An anaphoric pronoun ("its mana value") in a triggered
        // ability binds to the object introduced by the most recent earlier
        // *effect instruction* in the same ability (slot 1). CR 608.2k: if no
        // such instruction exists, fall back to the trigger-condition referent
        // (slot 2) then the cost referent (slot 3). Resolution order:
        //   1. `effect_context_object` — the earlier-instruction referent
        //      (revealed card / moved card / effect-sacrificed creature). This
        //      is the CR 608.2c anaphoric binding for the reveal class (Dark
        //      Confidant, #511).
        //   2. trigger-event source — the CR 608.2k trigger-condition referent
        //      (#512: "When this creature dies, ... its mana value"), live
        //      object then LKI.
        //   3. `cost_paid_object` — the CR 608.2k cost referent, last resort.
        // The arm differs from `CostPaidObject` only in slot priority:
        // instruction-order (608.2c) first, vs. cost referent (608.2k) first.
        // `Demonstrative` ("that spell's mana value", Mana Drain) shares this
        // resolution — same earlier-instruction referent named by a noun phrase.
        ObjectScope::Anaphoric | ObjectScope::Demonstrative => ability
            .and_then(|a| a.effect_context_object.as_ref())
            .map(|s| u32_to_i32_saturating(s.lki.mana_value))
            .or_else(|| {
                object_id_for_scope(state, ObjectScope::EventSource, ctx, targets).and_then(|id| {
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
            })
            .or_else(|| {
                ability
                    .and_then(|a| a.cost_paid_object.as_ref())
                    .map(|s| u32_to_i32_saturating(s.lki.mana_value))
            })
            .unwrap_or(0),
    }
}

/// CR 109.4 + CR 113.6: Resolve a single-player `PlayerScope` to a concrete
/// `PlayerId`, when one exists. Aggregate scopes (`Opponent`, `AllPlayers`)
/// have no single-player interpretation and yield `None`. Used to resolve the
/// `exclude` anchor of `PlayerScope::AllPlayers { exclude }`.
fn resolve_single_player_scope(
    state: &GameState,
    scope: &PlayerScope,
    controller: PlayerId,
    ctx: QuantityContext,
    targets: &[TargetRef],
    ability: Option<&ResolvedAbility>,
) -> Option<PlayerId> {
    match scope {
        PlayerScope::Controller => Some(controller),
        PlayerScope::ScopedPlayer => Some(ctx.scoped_player.unwrap_or(controller)),
        PlayerScope::Target => targets.iter().find_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            _ => None,
        }),
        PlayerScope::RecipientController => {
            let object_id = ctx.recipient.or_else(|| {
                targets.iter().find_map(|target| match target {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
            });
            object_id
                .or(Some(ctx.source))
                .and_then(|id| state.objects.get(&id))
                .map(|obj| obj.controller)
        }
        PlayerScope::DefendingPlayer => defending_player_for_quantity_context(state, ctx),
        // CR 109.4: controller of the parent object target.
        PlayerScope::ParentObjectTargetController => {
            ability.and_then(|a| crate::game::ability_utils::parent_target_controller(a, state))
        }
        // CR 613.1: the player persisted on the source via an "as ~ enters,
        // choose a player" replacement (Entropic Specter, Sewer Nemesis).
        PlayerScope::SourceChosenPlayer => {
            crate::game::game_object::source_chosen_player(state, ctx.source)
        }
        // Aggregate scopes have no single-player reading.
        PlayerScope::Opponent { .. } | PlayerScope::AllPlayers { .. } => None,
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
/// - `AllPlayers { aggregate, exclude }`: aggregates over every player
///   (CR 102.1), optionally excluding the `exclude` anchor ("each other
///   player").
fn resolve_per_player_scalar<F>(
    state: &GameState,
    scope: &PlayerScope,
    controller: PlayerId,
    ctx: QuantityContext,
    targets: &[TargetRef],
    ability: Option<&ResolvedAbility>,
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
        PlayerScope::ScopedPlayer => state
            .players
            .iter()
            .find(|p| p.id == ctx.scoped_player.unwrap_or(controller))
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
        PlayerScope::DefendingPlayer => defending_player_for_quantity_context(state, ctx)
            .and_then(|pid| state.players.iter().find(|p| p.id == pid))
            .map_or(0, &mut extract),
        // CR 109.4 + CR 608.2c: controller of the parent object target.
        PlayerScope::ParentObjectTargetController => ability
            .and_then(|a| crate::game::ability_utils::parent_target_controller(a, state))
            .and_then(|pid| state.players.iter().find(|p| p.id == pid))
            .map_or(0, &mut extract),
        // CR 613.1: the player persisted on the source via an "as ~ enters,
        // choose a player" replacement (Entropic Specter, Sewer Nemesis).
        PlayerScope::SourceChosenPlayer => {
            crate::game::game_object::source_chosen_player(state, ctx.source)
                .and_then(|pid| state.players.iter().find(|p| p.id == pid))
                .map_or(0, &mut extract)
        }
        PlayerScope::Opponent { aggregate } => aggregate_over_players(
            state.players.iter().filter(|p| p.id != controller),
            *aggregate,
            &mut extract,
        ),
        // CR 102.1: aggregate over all players, optionally excluding the
        // `exclude` anchor ("each OTHER player").
        PlayerScope::AllPlayers { aggregate, exclude } => {
            let excluded_id = exclude.as_deref().and_then(|ex| {
                resolve_single_player_scope(state, ex, controller, ctx, targets, ability)
            });
            aggregate_over_players(
                state.players.iter().filter(|p| Some(p.id) != excluded_id),
                *aggregate,
                &mut extract,
            )
        }
    }
}

fn defending_player_for_quantity_context(
    state: &GameState,
    ctx: QuantityContext,
) -> Option<PlayerId> {
    crate::game::combat::defending_player_for_attacker(state, ctx.source)
        .or_else(|| defending_player_from_event(state.current_trigger_event.as_ref(), ctx.source))
        .or_else(|| defending_player_from_event(detection_trigger_event().as_ref(), ctx.source))
}

fn defending_player_from_event(
    event: Option<&crate::types::events::GameEvent>,
    source_id: ObjectId,
) -> Option<PlayerId> {
    let crate::types::events::GameEvent::AttackersDeclared {
        defending_player,
        attacks,
        ..
    } = event?
    else {
        return None;
    };
    attacks
        .iter()
        .find_map(|(attacker_id, target)| {
            if *attacker_id == source_id {
                match target {
                    crate::game::combat::AttackTarget::Player(pid) => Some(*pid),
                    crate::game::combat::AttackTarget::Planeswalker(_)
                    | crate::game::combat::AttackTarget::Battle(_) => None,
                }
            } else {
                None
            }
        })
        .or(Some(*defending_player))
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

/// CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i: Single authority for
/// `PlayerFilter::OpponentDealtCombatDamage` matching. `player` was dealt combat
/// damage this turn (relative to `controller`'s opponents) when a
/// `damage_dealt_this_turn` record targets it with `is_combat = true` AND its
/// source matches `source`. `source = None` accepts any source (Tymna the
/// Weaver); otherwise (CR 120.9) the record's CR 608.2i look-back source
/// snapshot is matched against the filter (`TargetFilter::Any` matching every
/// source via the matcher), so the source's qualities are evaluated as of
/// damage time.
pub(crate) fn opponent_dealt_combat_damage_matches(
    state: &GameState,
    player: PlayerId,
    controller: PlayerId,
    source: &Option<Box<TargetFilter>>,
    ability_source_id: ObjectId,
) -> bool {
    if player == controller {
        return false;
    }
    let ctx = FilterContext::from_source_with_controller(ability_source_id, controller);
    state.damage_dealt_this_turn.iter().any(|r| {
        r.is_combat
            && matches!(r.target, TargetRef::Player(pid) if pid == player)
            && match source {
                None => true,
                // The matcher delegates to `filter_inner_for_object`, which
                // already returns `true` for `TargetFilter::Any` (CR 120.9), so
                // no separate short-circuit is needed here.
                Some(f) => matches_target_filter_on_damage_record_source(state, r, f, &ctx),
            }
    })
}

/// Count players matching a PlayerFilter relative to the controller.
pub(crate) fn resolve_player_count(
    state: &GameState,
    filter: &PlayerFilter,
    controller: PlayerId,
    source_id: ObjectId,
) -> i32 {
    if let PlayerFilter::OpponentAttacked {
        subject,
        scope: AttackScope::ThisCombat,
    } = filter
    {
        return usize_to_i32_saturating(
            state
                .attacked_defenders_this_combat_for(*subject, controller, source_id)
                .map(|defenders| defenders.iter().filter(|pid| **pid != controller).count())
                .unwrap_or(0),
        );
    }

    usize_to_i32_saturating(
        state
            .players
            .iter()
            .filter(|p| {
                !p.is_eliminated
                    && match filter {
                        PlayerFilter::Controller => p.id == controller,
                        PlayerFilter::Opponent => p.id != controller,
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
                            p.id != controller && p.life_lost_this_turn > 0
                        }
                        PlayerFilter::OpponentGainedLife => {
                            p.id != controller && p.life_gained_this_turn > 0
                        }
                        // CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i: Each
                        // opponent who was dealt combat damage this turn,
                        // optionally restricted to a matching source.
                        PlayerFilter::OpponentDealtCombatDamage { source } => {
                            opponent_dealt_combat_damage_matches(
                                state, p.id, controller, source, source_id,
                            )
                        }
                        // CR 508.6: opponent the subject attacked within scope.
                        PlayerFilter::OpponentAttacked { subject, scope } => {
                            p.id != controller
                                && state.opponent_attacked(
                                    *subject, *scope, controller, source_id, p.id,
                                )
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
                            crate::game::players::matches_relation(
                                state, p.id, controller, *relation,
                            ) && crate::game::players::performed_action_this_way(
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
                            if !crate::game::players::is_opponent(state, controller, p.id) {
                                false
                            } else {
                                let triggering =
                                    state.current_trigger_event.as_ref().and_then(|e| {
                                        crate::game::targeting::extract_player_from_event(e, state)
                                    });
                                triggering.is_none_or(|pid| pid != p.id)
                            }
                        }
                        // CR 608.2c + CR 701.38: Match each player who cast a
                        // vote for the recorded choice index in the most
                        // recent vote within the current top-level resolution.
                        PlayerFilter::VotedFor { choice_index } => state
                            .last_vote_ballots
                            .iter()
                            .any(|(voter, idx)| *voter == p.id && *idx == *choice_index),
                        // CR 109.4: the parent-object-target anchor has no
                        // single-player-count meaning here (no parent object
                        // target is in scope for a player-count quantity).
                        PlayerFilter::ParentObjectTargetController => false,
                        // CR 109.4 + CR 109.5: "each [player class] who controls
                        // [comparator] [count] [filter]" — count candidates that
                        // satisfy both the `relation` predicate and the
                        // controlled-permanent count comparison. Mirrors the arm
                        // in `effects::mod::matches_player_scope` (the two copies
                        // must stay in sync).
                        PlayerFilter::ControlsCount {
                            relation,
                            filter,
                            comparator,
                            count,
                        } => {
                            let threshold = resolve_quantity(state, count, controller, source_id);
                            crate::game::players::matches_relation(
                                state, p.id, controller, *relation,
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
                        // whose [scalar attr] [comparator] [value]" — count
                        // candidates satisfying both the `relation` predicate and
                        // the per-candidate scalar comparison. Mirrors the arm in
                        // `effects::mod::matches_player_scope` (the two copies must
                        // stay in sync). `attr` is read directly off `p`; `value`
                        // is the controller-relative threshold, resolved once.
                        PlayerFilter::PlayerAttribute {
                            relation,
                            attr,
                            comparator,
                            value,
                        } => {
                            let threshold = resolve_quantity(state, value, controller, source_id);
                            crate::game::players::matches_relation(
                                state, p.id, controller, *relation,
                            ) && crate::game::effects::candidate_player_scalar(p, attr)
                                .is_some_and(|lhs| comparator.evaluate(lhs, threshold))
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
        AggregateFunction, ChoiceValue, ControllerRef, DamageKindFilter, DevotionColors, Effect,
        FilterProp, KickerVariant, ObjectProperty, SharedQuality, TargetFilter, TargetRef,
        TypeFilter, TypedFilter,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::events::PlayerActionKind;
    use crate::types::game_state::{
        DamageRecord, ExileLink, ExileLinkKind, ManaSpentSourceSnapshot, ZoneChangeRecord,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::zones::Zone;
    use crate::types::{LandPlayRecord, SpellCastRecord};

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
    fn object_count_triggering_player_counts_only_triggering_players_permanents() {
        // CR 603.2 + CR 109.4: An `ObjectCount` filter scoped to
        // `ControllerRef::TriggeringPlayer` resolves the count against the
        // player identified by `state.current_trigger_event`.
        let mut state = GameState::new_two_player(42);

        // P0 controls a matching artifact; P1 controls a matching artifact.
        let p0_ring = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Wedding Ring".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p0_ring)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let p1_ring = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "Wedding Ring".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p1_ring)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Artifact],
            controller: Some(ControllerRef::TriggeringPlayer),
            properties: vec![FilterProp::Named {
                name: "Wedding Ring".to_string(),
            }],
        });
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: filter.clone(),
            },
        };

        // Trigger event: P1 drew a card → triggering player is P1.
        state.current_trigger_event = Some(crate::types::events::GameEvent::CardDrawn {
            player_id: PlayerId(1),
            object_id: ObjectId(999),
            nth_in_turn: 1,
            nth_in_step: 1,
        });
        // Counts only P1's Wedding Ring, not P0's.
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), p0_ring), 1);

        // Trigger event: P0 drew a card → triggering player is P0.
        state.current_trigger_event = Some(crate::types::events::GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(998),
            nth_in_turn: 1,
            nth_in_step: 1,
        });
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), p0_ring), 1);
    }

    #[test]
    fn resolve_attacked_this_turn_counts_creatures_attacked_with_by_controller() {
        let mut state = GameState::new_two_player(42);
        state.attacking_creatures_this_turn.insert(PlayerId(0), 3);

        let qty = QuantityExpr::Ref {
            qty: QuantityRef::AttackedThisTurn,
        };

        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), ObjectId(1)), 3);
    }

    #[test]
    fn resolve_sacrificed_this_turn_counts_matching_controller_records() {
        let mut state = GameState::new_two_player(42);
        let artifact = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Clue".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let creature = create_object(
            &mut state,
            CardId(101),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        state.sacrificed_permanents_this_turn.push(
            state.objects[&artifact].snapshot_for_zone_change(
                artifact,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            ),
        );
        state.sacrificed_permanents_this_turn.push(
            state.objects[&creature].snapshot_for_zone_change(
                creature,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            ),
        );

        let qty = QuantityExpr::Ref {
            qty: QuantityRef::SacrificedThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            },
        };

        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), artifact), 1);
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(1), creature), 0);
    }

    /// CR 202.2 + CR 601.2h: GitHub #307 — Painful Truths bug regression.
    /// `ManaSpentToCast { metric: DistinctColors }` reads
    /// `GameObject::colors_spent_to_cast.distinct_colors()`, which is the
    /// per-color tally populated during cost payment. This test verifies the
    /// resolver returns the count of distinct colors with non-zero tallies —
    /// the runtime contract Painful Truths' "draw X / lose X life" depends on.
    /// Three distinct colors (W+U+B, e.g. Painful Truths cast for {1}{W}{U}{B})
    /// must resolve to 3, while monocolored repeats (B+B+B) must resolve to 1.
    #[test]
    fn resolve_distinct_colors_of_mana_spent_to_cast_self() {
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Painful Truths".to_string(),
            Zone::Stack,
        );

        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::SelfObject,
                metric: CastManaSpentMetric::DistinctColors,
            },
        };

        // Three distinct colors → 3 (Painful Truths cast for {1}{W}{U}{B})
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.colors_spent_to_cast = crate::types::mana::ColoredManaCount::default();
            obj.colors_spent_to_cast.add(ManaColor::White, 1);
            obj.colors_spent_to_cast.add(ManaColor::Blue, 1);
            obj.colors_spent_to_cast.add(ManaColor::Black, 1);
        }
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), spell), 3);

        // Monocolored repeats (B+B+B) → 1 distinct color
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.colors_spent_to_cast = crate::types::mana::ColoredManaCount::default();
            obj.colors_spent_to_cast.add(ManaColor::Black, 3);
        }
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), spell), 1);

        // No colored mana spent (would be 0 — alternate cast or all-colorless
        // payment) → 0 distinct colors.
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.colors_spent_to_cast = crate::types::mana::ColoredManaCount::default();
        }
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), spell), 0);
    }

    /// CR 122.1 + CR 109.4: count distinct counter kinds among permanents the
    /// resolving player controls. An opponent's counter kind and stale
    /// zero-count map entries must be EXCLUDED, and the same kind on two
    /// controlled permanents counts once. Order is deterministic (sorted by
    /// `as_str`).
    #[test]
    fn resolve_distinct_counter_kinds_among_controlled_permanents() {
        let mut state = GameState::new_two_player(42);

        // Controller's permanent A: +1/+1 counter.
        let perm_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Perm A".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm_a).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 1);
        }
        // Controller's permanent B: Lore counter (distinct kind).
        let perm_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Perm B".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm_b).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.counters.insert(CounterType::Lore, 1);
        }
        // Controller's permanent C: another +1/+1 (same kind as A — counts once).
        let perm_c = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Perm C".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm_c).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 3);
            obj.counters
                .insert(CounterType::Generic("charge".to_string()), 0);
        }
        // OPPONENT's permanent: Stun counter — MUST be excluded (CR 109.4).
        let opp_perm = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Opp Perm".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&opp_perm).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Stun, 1);
        }

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Permanent],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });

        // Direct helper: distinct kinds among controlled permanents = {P1P1, Lore},
        // excluding Perm C's stale zero-count charge entry.
        // sorted by as_str ("P1P1" < "lore" by byte order; uppercase 'P' = 0x50,
        // lowercase 'l' = 0x6C).
        let ctx = FilterContext::from_source_with_controller(perm_a, PlayerId(0));
        let kinds = distinct_counter_kinds_among(&state, &filter, &ctx);
        assert_eq!(kinds, vec![CounterType::Plus1Plus1, CounterType::Lore]);

        // QuantityRef resolution returns the count (2).
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::DistinctCounterKindsAmong {
                filter: filter.clone(),
            },
        };
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), perm_a), 2);
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
                    scoped_player: None,
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

    /// CR 122.1 + CR 603.7c: "that player has" on a damage trigger binds to
    /// the damaged player carried by `current_trigger_event`.
    #[test]
    fn resolve_quantity_player_counter_scoped_player_from_damage_event() {
        use crate::types::events::GameEvent;
        use crate::types::player::PlayerCounterKind;

        let mut state = GameState::new_two_player(42);
        state.players[0].poison_counters = 1;
        state.players[1].poison_counters = 4;
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 2,
            is_combat: true,
            excess: 0,
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCounter {
                kind: PlayerCounterKind::Poison,
                scope: CountScope::ScopedPlayer,
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(7)), 4);
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
            qty: QuantityRef::ObjectCountDistinct {
                filter,
                qualities: vec![SharedQuality::Name],
            },
        };
        // 3 distinct names controlled by P0: Plains, Island, Field of the Dead.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(0)), 3);
        // P1's POV: only the one opponent Plains would be theirs, so 1.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(0)), 1);
    }

    /// M7 lift regression: switching the dedup axis from Name → ManaValue must
    /// produce the count of distinct mana values among the matching objects.
    /// Proves the parameterized resolver dispatches on `qualities` rather than
    /// hardcoding the legacy name-only path.
    #[test]
    fn resolve_quantity_object_count_distinct_mana_values_uses_mana_value_axis() {
        let mut state = GameState::new_two_player(42);
        // Three objects: two with mana value 2 (one shared bucket), one with
        // mana value 4. Distinct mana values = 2.
        for cost in &[
            ManaCost::Cost {
                shards: vec![],
                generic: 2,
            },
            ManaCost::Cost {
                shards: vec![],
                generic: 2,
            },
            ManaCost::Cost {
                shards: vec![],
                generic: 4,
            },
        ] {
            let id = create_object(
                &mut state,
                CardId(200),
                PlayerId(0),
                "Generic Card".to_string(),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().mana_cost = cost.clone();
        }
        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountDistinct {
                filter,
                qualities: vec![SharedQuality::ManaValue],
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(0)), 2);
    }

    fn shared_quality_count_expr(
        aggregate: AggregateFunction,
        quality: SharedQuality,
    ) -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountBySharedQuality {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                quality,
                aggregate,
            },
        }
    }

    fn add_creature_with_subtypes(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        subtypes: &[&str],
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(300),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.card_types.subtypes = subtypes
            .iter()
            .map(|subtype| (*subtype).to_string())
            .collect();
        id
    }

    #[test]
    fn resolve_object_count_by_shared_quality_max_and_sum_creature_types() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec![
            "Elf".to_string(),
            "Warrior".to_string(),
            "Druid".to_string(),
            "Human".to_string(),
        ];
        add_creature_with_subtypes(&mut state, PlayerId(0), "Elf Warrior", &["Elf", "Warrior"]);
        add_creature_with_subtypes(&mut state, PlayerId(0), "Elf Druid", &["Elf", "Druid"]);
        add_creature_with_subtypes(
            &mut state,
            PlayerId(0),
            "Human Warrior",
            &["Human", "Warrior"],
        );
        let changeling =
            add_creature_with_subtypes(&mut state, PlayerId(0), "Masked Vandal", &["Shapeshifter"]);
        state
            .objects
            .get_mut(&changeling)
            .unwrap()
            .keywords
            .push(Keyword::Changeling);
        add_creature_with_subtypes(&mut state, PlayerId(1), "Opponent Elf", &["Elf"]);

        let source = ObjectId(0);
        assert_eq!(
            resolve_quantity(
                &state,
                &shared_quality_count_expr(AggregateFunction::Max, SharedQuality::CreatureType),
                PlayerId(0),
                source,
            ),
            3
        );
        assert_eq!(
            resolve_quantity(
                &state,
                &shared_quality_count_expr(AggregateFunction::Sum, SharedQuality::CreatureType),
                PlayerId(0),
                source,
            ),
            4
        );
    }

    #[test]
    fn resolve_object_count_by_shared_quality_sum_deduplicates_multityped_objects() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec![
            "Elf".to_string(),
            "Warrior".to_string(),
            "Goblin".to_string(),
        ];
        add_creature_with_subtypes(&mut state, PlayerId(0), "Elf Warrior", &["Elf", "Warrior"]);
        add_creature_with_subtypes(&mut state, PlayerId(0), "Elf", &["Elf"]);
        add_creature_with_subtypes(&mut state, PlayerId(0), "Goblin", &["Goblin"]);

        assert_eq!(
            resolve_quantity(
                &state,
                &shared_quality_count_expr(AggregateFunction::Sum, SharedQuality::CreatureType),
                PlayerId(0),
                ObjectId(0),
            ),
            2
        );
    }

    #[test]
    fn resolve_object_count_by_shared_quality_min_and_empty_buckets() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec!["Elf".to_string(), "Warrior".to_string()];
        add_creature_with_subtypes(&mut state, PlayerId(0), "Elf Warrior", &["Elf", "Warrior"]);
        add_creature_with_subtypes(&mut state, PlayerId(0), "Elf", &["Elf"]);

        assert_eq!(
            resolve_quantity(
                &state,
                &shared_quality_count_expr(AggregateFunction::Min, SharedQuality::CreatureType),
                PlayerId(0),
                ObjectId(0),
            ),
            1
        );

        let mut empty_state = GameState::new_two_player(43);
        add_creature_with_subtypes(&mut empty_state, PlayerId(0), "Typeless", &[]);
        assert_eq!(
            resolve_quantity(
                &empty_state,
                &shared_quality_count_expr(AggregateFunction::Max, SharedQuality::CreatureType),
                PlayerId(0),
                ObjectId(0),
            ),
            0
        );
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

        let half_up = QuantityExpr::DivideRounded {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            }),
            divisor: 2,
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

    #[test]
    fn resolve_max_damage_dealt_this_turn_groups_by_source_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Dragon Cultist".to_string(),
            Zone::Battlefield,
        );
        let p0_source = create_object(
            &mut state,
            CardId(1001),
            PlayerId(0),
            "Lightning Rig".to_string(),
            Zone::Battlefield,
        );
        let p0_other_source = create_object(
            &mut state,
            CardId(1002),
            PlayerId(0),
            "Spark Rig".to_string(),
            Zone::Battlefield,
        );
        let p1_source = create_object(
            &mut state,
            CardId(1003),
            PlayerId(1),
            "Opposing Rig".to_string(),
            Zone::Battlefield,
        );
        state.damage_dealt_this_turn.extend([
            DamageRecord {
                source_id: p0_source,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 3,
                is_combat: false,
                ..Default::default()
            },
            DamageRecord {
                source_id: p0_source,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 2,
                is_combat: false,
                ..Default::default()
            },
            DamageRecord {
                source_id: p0_other_source,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 4,
                is_combat: false,
                ..Default::default()
            },
            DamageRecord {
                source_id: p1_source,
                source_controller: PlayerId(1),
                target: TargetRef::Player(PlayerId(0)),
                target_controller: PlayerId(0),
                amount: 9,
                is_combat: false,
                source_controller_snapshot: PlayerId(1),
                ..Default::default()
            },
        ]);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                target: Box::new(TargetFilter::Any),
                aggregate: AggregateFunction::Max,
                group_by: Some(crate::types::ability::DamageGroupKey::SourceId),
                damage_kind: DamageKindFilter::Any,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 5);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), source), 9);
    }

    #[test]
    fn resolve_damage_dealt_this_turn_filters_source_and_player_target() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Dunerider Outlaw".to_string(),
            Zone::Battlefield,
        );
        let other_source = create_object(
            &mut state,
            CardId(1001),
            PlayerId(0),
            "Other Source".to_string(),
            Zone::Battlefield,
        );
        state.damage_dealt_this_turn.extend([
            DamageRecord {
                source_id: source,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 1,
                is_combat: true,
                ..Default::default()
            },
            DamageRecord {
                source_id: other_source,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 1,
                is_combat: true,
                ..Default::default()
            },
        ]);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::SelfRef),
                target: Box::new(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 1);
    }

    #[test]
    fn resolve_damage_dealt_this_turn_filters_self_target() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Wall of Resistance".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(1001),
            PlayerId(1),
            "Opposing Source".to_string(),
            Zone::Battlefield,
        );
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: damage_source,
            source_controller: PlayerId(1),
            target: TargetRef::Object(source),
            target_controller: PlayerId(0),
            amount: 1,
            is_combat: false,
            source_controller_snapshot: PlayerId(1),
            ..Default::default()
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::SelfRef),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 1);
    }

    /// CR 120.9 (audit M2): the parameterized `DamageDealtThisTurn` with
    /// `aggregate: Max, group_by: Some(SourceId)` must yield the same answer
    /// as the removed `MaxDamageDealtThisTurnBySourceControlledBy` did. Two
    /// p0-controlled sources contribute 5 (Lightning Rig: 3+2) and 4 (Spark
    /// Rig); Max picks 5. P1's lone source contributes 9.
    #[test]
    fn parameterized_damage_dealt_this_turn_max_matches_legacy_max_semantics() {
        use crate::types::ability::DamageGroupKey;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Dragon Cultist".to_string(),
            Zone::Battlefield,
        );
        let p0_source = create_object(
            &mut state,
            CardId(1001),
            PlayerId(0),
            "Lightning Rig".to_string(),
            Zone::Battlefield,
        );
        let p0_other = create_object(
            &mut state,
            CardId(1002),
            PlayerId(0),
            "Spark Rig".to_string(),
            Zone::Battlefield,
        );
        let p1_source = create_object(
            &mut state,
            CardId(1003),
            PlayerId(1),
            "Opposing Rig".to_string(),
            Zone::Battlefield,
        );
        state.damage_dealt_this_turn.extend([
            DamageRecord {
                source_id: p0_source,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 3,
                is_combat: false,
                ..Default::default()
            },
            DamageRecord {
                source_id: p0_source,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 2,
                is_combat: false,
                ..Default::default()
            },
            DamageRecord {
                source_id: p0_other,
                source_controller: PlayerId(0),
                target: TargetRef::Player(PlayerId(1)),
                target_controller: PlayerId(1),
                amount: 4,
                is_combat: false,
                ..Default::default()
            },
            DamageRecord {
                source_id: p1_source,
                source_controller: PlayerId(1),
                target: TargetRef::Player(PlayerId(0)),
                target_controller: PlayerId(0),
                amount: 9,
                is_combat: false,
                source_controller_snapshot: PlayerId(1),
                ..Default::default()
            },
        ]);

        let your_max = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                target: Box::new(TargetFilter::Any),
                aggregate: AggregateFunction::Max,
                group_by: Some(DamageGroupKey::SourceId),
                damage_kind: DamageKindFilter::Any,
            },
        };
        // P0's single largest source contribution is 5 (Lightning Rig: 3+2),
        // not 9 (P1's source) — controller predicate evaluated against
        // record.source_controller (LKI per CR 120.9).
        assert_eq!(resolve_quantity(&state, &your_max, PlayerId(0), source), 5);
        assert_eq!(resolve_quantity(&state, &your_max, PlayerId(1), source), 9);
    }

    /// CR 120.9 (audit M2): the source filter's `controller` predicate must be
    /// evaluated against `DamageRecord::source_controller` (LKI captured at the
    /// time of damage), not against the live source object's current controller.
    /// If the live object's controller has changed (e.g., Threaten effect) since
    /// the damage was dealt, "a source you controlled dealt damage this turn"
    /// must still credit the original controller.
    #[test]
    fn parameterized_damage_dealt_this_turn_uses_lki_controller_after_control_change() {
        use crate::types::ability::DamageGroupKey;

        let mut state = GameState::new_two_player(42);
        let scoping = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Scope".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(1001),
            PlayerId(0),
            "Goblin Piker".to_string(),
            Zone::Battlefield,
        );
        // Damage was dealt while P0 controlled the source.
        // CR 608.2i: the source-controller snapshot captures P0 at damage time;
        // the source-side filter now matches against this snapshot, not the live
        // object's controller (which changes below).
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: damage_source,
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 4,
            is_combat: false,
            source_controller_snapshot: PlayerId(0),
            ..Default::default()
        });
        // Then control changed (e.g., Threaten); the live object now belongs to P1.
        state.objects.get_mut(&damage_source).unwrap().controller = PlayerId(1);

        let your_max = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                target: Box::new(TargetFilter::Any),
                aggregate: AggregateFunction::Max,
                group_by: Some(DamageGroupKey::SourceId),
                damage_kind: DamageKindFilter::Any,
            },
        };
        // P0 still sees their 4 damage even though the live source is now P1's.
        assert_eq!(resolve_quantity(&state, &your_max, PlayerId(0), scoping), 4);
        // P1 sees nothing — they didn't control the source when the damage occurred.
        assert_eq!(resolve_quantity(&state, &your_max, PlayerId(1), scoping), 0);
    }

    #[test]
    fn parameterized_damage_dealt_this_turn_uses_target_lki_controller_after_control_change() {
        let mut state = GameState::new_two_player(42);
        let scoping = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Scope".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(1001),
            PlayerId(0),
            "Prodigal Pyromancer".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(1002),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: damage_source,
            source_controller: PlayerId(0),
            target: TargetRef::Object(target),
            target_controller: PlayerId(1),
            amount: 3,
            is_combat: false,
            ..Default::default()
        });
        state.objects.get_mut(&target).unwrap().controller = PlayerId(0);

        let damage_to_opponent_controlled_target = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
        };

        assert_eq!(
            resolve_quantity(
                &state,
                &damage_to_opponent_controlled_target,
                PlayerId(0),
                scoping,
            ),
            3
        );
        assert_eq!(
            resolve_quantity(
                &state,
                &damage_to_opponent_controlled_target,
                PlayerId(1),
                scoping,
            ),
            0
        );
    }

    /// CR 120.2b + CR 601.2f: Noncombat-only damage to opponents (Chandra's
    /// Incinerator cost reduction) ignores combat damage in the ledger.
    #[test]
    fn resolve_noncombat_damage_dealt_to_opponents_this_turn_excludes_combat() {
        use crate::types::ability::DamageKindFilter;
        use crate::types::game_state::DamageRecord;

        let mut state = GameState::new_two_player(42);
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(1)),
            amount: 5,
            is_combat: false,
            ..Default::default()
        });
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(2),
            target: TargetRef::Player(PlayerId(1)),
            amount: 7,
            is_combat: true,
            ..Default::default()
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::And {
                    filters: vec![
                        TargetFilter::Player,
                        TargetFilter::Typed(
                            TypedFilter::default().controller(ControllerRef::Opponent),
                        ),
                    ],
                }),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::NoncombatOnly,
            },
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            5
        );
    }

    /// Audit M2 backward-compat: a JSON snapshot of the pre-parameterization
    /// `DamageDealtThisTurn { source, target }` form must deserialize via the
    /// `#[serde(default)]` defaults (`aggregate: Sum`, `group_by: None`) so
    /// existing serialized state continues to work.
    #[test]
    fn parameterized_damage_dealt_this_turn_legacy_json_deserializes_with_defaults() {
        use crate::types::ability::DamageGroupKey;

        let legacy_json = r#"{
            "type": "DamageDealtThisTurn",
            "source": { "type": "Any" },
            "target": { "type": "SelfRef" }
        }"#;
        let parsed: QuantityRef =
            serde_json::from_str(legacy_json).expect("legacy JSON must deserialize");
        match parsed {
            QuantityRef::DamageDealtThisTurn {
                source,
                target,
                aggregate,
                group_by,
                damage_kind,
            } => {
                assert_eq!(*source, TargetFilter::Any);
                assert_eq!(*target, TargetFilter::SelfRef);
                assert_eq!(aggregate, AggregateFunction::Sum);
                assert_eq!(group_by, None);
                assert_eq!(damage_kind, crate::types::ability::DamageKindFilter::Any);
                // Sanity: an explicit Max+SourceId still round-trips.
                let new_form = QuantityRef::DamageDealtThisTurn {
                    source: Box::new(TargetFilter::Any),
                    target: Box::new(TargetFilter::Any),
                    aggregate: AggregateFunction::Max,
                    group_by: Some(DamageGroupKey::SourceId),
                    damage_kind: DamageKindFilter::Any,
                };
                let round_trip: QuantityRef =
                    serde_json::from_str(&serde_json::to_string(&new_form).unwrap()).unwrap();
                assert_eq!(round_trip, new_form);
            }
            other => panic!("expected DamageDealtThisTurn, got {other:?}"),
        }
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

    /// CR 613.1: `CountScope::SourceChosenPlayer` resolves to the player
    /// persisted on the source via `ChosenAttribute::Player`, so CDA P/T can
    /// track any chosen-player zone.
    #[test]
    fn resolve_quantity_source_chosen_player_zone_counts() {
        let mut state = GameState::new_two_player(42);
        // The CDA source, controlled by player 0, with player 1 chosen.
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sewer Nemesis".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&src)
            .unwrap()
            .chosen_attributes
            .push(crate::types::ability::ChosenAttribute::Player(PlayerId(1)));
        // Player 1: 2 cards in hand, 3 in graveyard, 4 in library, 1 in exile.
        for i in 0..2 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                format!("H{i}"),
                Zone::Hand,
            );
        }
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(20 + i),
                PlayerId(1),
                format!("G{i}"),
                Zone::Graveyard,
            );
        }
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(30 + i),
                PlayerId(1),
                format!("L{i}"),
                Zone::Library,
            );
        }
        create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "E0".to_string(),
            Zone::Exile,
        );

        for (zone, expected, label) in [
            (ZoneRef::Hand, 2, "chosen player's hand"),
            (ZoneRef::Graveyard, 3, "chosen player's graveyard"),
            (ZoneRef::Library, 4, "chosen player's library"),
            (ZoneRef::Exile, 1, "chosen player's exile"),
        ] {
            let qty = QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone,
                    card_types: Vec::new(),
                    scope: CountScope::SourceChosenPlayer,
                },
            };
            assert_eq!(
                resolve_quantity(&state, &qty, PlayerId(0), src),
                expected,
                "{label}"
            );
        }
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

    /// CR 109.5: "you"/"your" on a triggered ability refer to the printed
    /// controller of the source object, not the player a `player_scope`
    /// iteration is temporarily affecting. During each-opponent resolution the
    /// engine rebinds `ResolvedAbility::controller` to the scoped player while
    /// preserving the printed controller in `original_controller`. A quantity
    /// sub-expression ("Zombies you control" on The Scarab God) must read the
    /// printed controller, so this asserts the count comes from P0 (printed
    /// caster) even though `controller` has been rebound to P1 (scoped
    /// opponent). Reverting the `quantity.rs` seam to a bare `from_ability`
    /// would count P1's creatures (0) and fail this test.
    #[test]
    fn resolve_quantity_you_control_uses_original_controller_under_player_scope() {
        let mut state = GameState::new_two_player(42);
        // P0 (printed caster) controls 2 creatures.
        for i in 0..2 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("P0 Creature {i}"),
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
        // P1 (scoped opponent) controls 0 creatures — asymmetric so the seam is
        // discriminating: a correct fix returns 2, the buggy path returns 0.

        // Source object is controlled by P0.
        let source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        // Simulate mid-`player_scope` iteration: `controller` has been rebound to
        // the scoped opponent (P1), but `original_controller` retains the printed
        // caster (P0).
        let mut ability = crate::types::ability::ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(1),
        );
        ability.original_controller = Some(PlayerId(0));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };

        // "creatures you control" resolves against the printed controller (P0 → 2),
        // not the scoped opponent (P1 → 0).
        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 2);
    }

    /// Build an asymmetric board (P0 = `p0_creatures`, P1 = `p1_creatures`) plus a
    /// P0-controlled source object. Mirrors the harness shape of
    /// `resolve_quantity_you_control_uses_original_controller_under_player_scope`
    /// so the "they control" relative-scope tests are discriminating.
    fn asymmetric_creature_board(p0_creatures: u32, p1_creatures: u32) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let mut next_card = 1u64;
        let mut add_creatures = |state: &mut GameState, owner: PlayerId, n: u32| {
            for i in 0..n {
                let id = create_object(
                    state,
                    CardId(next_card),
                    owner,
                    format!("{owner:?} Creature {i}"),
                    Zone::Battlefield,
                );
                next_card += 1;
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Creature);
            }
        };
        add_creatures(&mut state, PlayerId(0), p0_creatures);
        add_creatures(&mut state, PlayerId(1), p1_creatures);
        let source = create_object(
            &mut state,
            CardId(next_card),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        (state, source)
    }

    /// CR 115.10 + CR 608.2c: runtime-seam companion to the parser fix
    /// (`oracle_quantity::tests::for_each_they_control_threads_scoped_player`).
    /// Once the parser threads "they control" into a
    /// `ControllerRef::ScopedPlayer` filter, this verifies the resolver consumes
    /// it correctly: with `controller` rebound to the scoped opponent (P1) and
    /// `original_controller` retaining the caster (P0), the `ScopedPlayer` count
    /// follows `scoped_player` (P1 → 5), NOT the caster's board (P0 → 3). The
    /// asymmetric board makes the assertion discriminating against any resolver
    /// regression that read the caster instead.
    #[test]
    fn for_each_they_control_scoped_player_counts_iterating_player() {
        let (state, source) = asymmetric_creature_board(3, 5);

        let mut ability = crate::types::ability::ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(1),
        );
        ability.original_controller = Some(PlayerId(0));
        ability.scoped_player = Some(PlayerId(1));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                ),
            },
        };

        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 5);
    }

    /// CR 109.4 + CR 115.1: runtime-seam companion to
    /// `oracle_quantity::tests::for_each_they_control_threads_target_player`. The
    /// resolver reads a `ControllerRef::TargetPlayer` filter against the first
    /// `TargetRef::Player` (P1 → 5), NOT the caster (P0 → 3). Covers Burden of
    /// Greed / Emissary of Despair / Hoard Hauler ("for each [type] that player
    /// controls").
    #[test]
    fn for_each_they_control_target_player_counts_targeted_player() {
        let (state, source) = asymmetric_creature_board(3, 5);

        let mut ability = crate::types::ability::ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        ability.original_controller = Some(PlayerId(0));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
            },
        };

        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 5);
    }

    /// CR 608.2c + CR 109.4: runtime-seam companion for the chosen-player scope.
    /// The resolver reads a `ControllerRef::ChosenPlayer { index: 0 }` filter
    /// against `chosen_players[0]` (P1 → 5), NOT the caster (P0 → 3). Covers
    /// Benevolent Offering's second clause ("for each creature the chosen player
    /// controls").
    #[test]
    fn for_each_they_control_chosen_player_counts_chosen_player() {
        let (state, source) = asymmetric_creature_board(3, 5);

        let mut ability = crate::types::ability::ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        ability.original_controller = Some(PlayerId(0));
        ability.chosen_players = vec![PlayerId(1)];

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::ChosenPlayer { index: 0 }),
                ),
            },
        };

        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 5);
    }

    /// CR 109.5: "creatures you control" stays bound to the printed caster (P0 → 3)
    /// even while `controller` is rebound to the scoped opponent (P1) mid-iteration —
    /// the complement of `for_each_they_control_scoped_player_counts_iterating_player`,
    /// confirming the fix doesn't disturb "you control" counts.
    #[test]
    fn for_each_you_control_stays_caster_under_player_scope() {
        let (state, source) = asymmetric_creature_board(3, 5);

        let mut ability = crate::types::ability::ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(1),
        );
        ability.original_controller = Some(PlayerId(0));
        ability.scoped_player = Some(PlayerId(1));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };

        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 3);
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
                counter_type: Some(CounterType::Plus1Plus1),
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

    /// CR 120.1 + CR 510.1: Resolving `PlayerCount { OpponentDealtCombatDamage }`
    /// against `damage_dealt_this_turn` records counts only opponents whose
    /// player target appears in the ledger with `is_combat = true`. Mirrors
    /// the partner-quality "for each opponent that was dealt combat damage
    /// this turn" class (Tymna the Weaver) and the resolver's `Opponent`
    /// guard ensures the controller's own combat-damage entries don't leak in.
    #[test]
    fn resolve_quantity_player_count_opponent_dealt_combat_damage() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        // Player 1 was dealt combat damage; player 2 was dealt non-combat
        // damage; player 0 (controller) is excluded by the Opponent guard.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(99),
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 4,
            is_combat: true,
            ..Default::default()
        });
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(99),
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(2)),
            target_controller: PlayerId(2),
            amount: 2,
            is_combat: false,
            ..Default::default()
        });
        // Self-damage record must not count as an "opponent dealt combat damage".
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(99),
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(0)),
            target_controller: PlayerId(0),
            amount: 1,
            is_combat: true,
            ..Default::default()
        });

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
            },
        };
        // Controller = player 0: only player 1 satisfies (combat + opponent).
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);
    }

    /// CR 120.1 + CR 510.1: With no combat damage dealt this turn, the
    /// quantity resolves to zero — the empty-ledger case mirrors
    /// `resolve_quantity_player_count_opponent_lost_life_none_lost`.
    #[test]
    fn resolve_quantity_player_count_opponent_dealt_combat_damage_none() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    /// CR 608.2i (the whole point): a source-restricted `OpponentDealtCombatDamage`
    /// must match against the record's damage-time source snapshot, NOT the live
    /// object. Record combat damage from a Dragon to an opponent, then remove
    /// the live Dragon from the battlefield. A resolve-now model would see no
    /// Dragon and return 0; the snapshot model still counts the opponent (1).
    #[test]
    fn opponent_dealt_combat_damage_source_uses_damage_time_snapshot() {
        let mut state = GameState::new_two_player(42);
        let dragon = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Shivan Dragon".to_string(),
            Zone::Battlefield,
        );
        // The live object's subtype at damage time IS Dragon; record the snapshot.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: dragon,
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 5,
            is_combat: true,
            source_subtypes: vec!["Dragon".to_string()],
            source_core_types: vec![CoreType::Creature],
            source_controller_snapshot: PlayerId(0),
            source_owner: PlayerId(0),
            ..Default::default()
        });
        // The live Dragon leaves the battlefield (or is transformed). A
        // resolve-now matcher against the live object would now find nothing.
        state.objects.remove(&dragon);

        let dragon_filter =
            TargetFilter::Typed(TypedFilter::default().subtype("Dragon".to_string()));
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentDealtCombatDamage {
                    source: Some(Box::new(dragon_filter)),
                },
            },
        };
        // CR 608.2i: the opponent was dealt combat damage by a Dragon this turn,
        // even though the Dragon no longer exists.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), dragon), 1);
    }

    /// CR 120.9 + CR 608.2i: source-filter discrimination — a Dragon-restricted
    /// filter counts a Dragon-snapshot record but not a non-Dragon one; a
    /// `SelfRef` filter matches only the record whose source is the ability source.
    #[test]
    fn opponent_dealt_combat_damage_source_filter_discriminates() {
        let mut state = GameState::new_two_player(42);
        let ability_source = ObjectId(50);
        // Record 1: combat damage to opponent from a Dragon that IS the ability source.
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ability_source,
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 2,
            is_combat: true,
            source_subtypes: vec!["Dragon".to_string()],
            source_core_types: vec![CoreType::Creature],
            source_controller_snapshot: PlayerId(0),
            source_owner: PlayerId(0),
            ..Default::default()
        });

        let dragon = TargetFilter::Typed(TypedFilter::default().subtype("Dragon".to_string()));
        let goblin = TargetFilter::Typed(TypedFilter::default().subtype("Goblin".to_string()));

        let count = |source: TargetFilter| {
            resolve_quantity(
                &state,
                &QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount {
                        filter: PlayerFilter::OpponentDealtCombatDamage {
                            source: Some(Box::new(source)),
                        },
                    },
                },
                PlayerId(0),
                ability_source,
            )
        };

        assert_eq!(count(dragon), 1, "Dragon snapshot matches Dragon filter");
        assert_eq!(
            count(goblin),
            0,
            "Dragon snapshot does not match Goblin filter"
        );
        assert_eq!(
            count(TargetFilter::SelfRef),
            1,
            "record source IS the ability source → SelfRef matches"
        );
    }

    /// CR 608.2i + CR 120.9: A look-back source filter that discriminates on
    /// the source's *zone* must evaluate against the zone recorded at damage
    /// time, not an assumed battlefield. Non-combat damage from an instant
    /// originates on the Stack (CR 608.2b), so an `InZone { Battlefield }`
    /// ("by a permanent") source filter must NOT match a Stack-sourced record,
    /// while an `InZone { Stack }` filter must. This is discriminating: under
    /// the prior hardcoded `Zone::Battlefield` reconstruction the Battlefield
    /// filter would wrongly match.
    #[test]
    fn damage_dealt_this_turn_source_filter_respects_snapshot_zone() {
        let mut state = GameState::new_two_player(42);
        // An instant spell on the Stack deals 3 non-combat damage to player 1.
        let instant = ObjectId(70);
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: instant,
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(1)),
            target_controller: PlayerId(1),
            amount: 3,
            is_combat: false,
            source_name: "Lightning Bolt".to_string(),
            source_core_types: vec![CoreType::Instant],
            source_controller_snapshot: PlayerId(0),
            source_owner: PlayerId(0),
            // The spell lives on the Stack while it deals damage.
            source_zone: Zone::Stack,
            ..Default::default()
        });

        let count = |source: TargetFilter| {
            resolve_quantity(
                &state,
                &QuantityExpr::Ref {
                    qty: QuantityRef::DamageDealtThisTurn {
                        source: Box::new(source),
                        target: Box::new(TargetFilter::Any),
                        aggregate: AggregateFunction::Sum,
                        group_by: None,
                        damage_kind: DamageKindFilter::Any,
                    },
                },
                PlayerId(0),
                instant,
            )
        };

        let on_battlefield =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }]));
        let on_stack = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::InZone { zone: Zone::Stack }]),
        );

        assert_eq!(
            count(on_battlefield),
            0,
            "Stack-sourced damage must NOT match an on-battlefield source filter"
        );
        assert_eq!(
            count(on_stack),
            3,
            "Stack-sourced damage matches an on-stack source filter"
        );
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
                    exclude: None,
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
                    exclude: None,
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

    /// CR 119.2 + CR 700.1: `PlayerCount { OpponentLostLife }` counts only
    /// opponents whose `life_lost_this_turn > 0` — Belbe's mana count base.
    #[test]
    fn player_count_opponent_lost_life_counts_only_damaged_opponents() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        // Controller (P0) and one opponent (P1) lost life; P2 did not.
        state.players[0].life_lost_this_turn = 4;
        state.players[1].life_lost_this_turn = 3;
        state.players[2].life_lost_this_turn = 0;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        };
        // Controller = P0: only P1 qualifies (P0 excluded as self, P2 lost 0).
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 1);

        // Now both opponents have lost life.
        state.players[2].life_lost_this_turn = 1;
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 2);
    }

    /// CR 109.4 + CR 109.5: `resolve_player_count` for
    /// `PlayerFilter::ControlsCount` counts candidates by the controlled-permanent
    /// count comparison. Kept in sync with `effects::matches_player_scope`'s
    /// identical arm. `{ EQ, Fixed(0) }` ≡ old `ControlsNone`; `{ GE, Fixed(1) }`
    /// ≡ old `Controls`.
    #[test]
    fn resolve_player_count_controls_permanent_presence() {
        use crate::types::ability::{Comparator, PlayerRelation, TypeFilter, TypedFilter};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        // P1 controls an Elf; P0 and P2 control nothing.
        let elf = create_object(
            &mut state,
            CardId(800),
            PlayerId(1),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&elf).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
        }

        let elf_filter =
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Elf".to_string())));

        // "each opponent who doesn't control an Elf" (count == 0) — controller P0:
        // opponents are P1 (controls Elf → excluded) and P2 → count 1.
        let none = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::ControlsCount {
                    relation: PlayerRelation::Opponent,
                    filter: elf_filter.clone(),
                    comparator: Comparator::EQ,
                    count: Box::new(QuantityExpr::Fixed { value: 0 }),
                },
            },
        };
        assert_eq!(resolve_quantity(&state, &none, PlayerId(0), ObjectId(1)), 1);

        // "each opponent who controls an Elf" (count >= 1) — only P1 → count 1.
        let controls = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::ControlsCount {
                    relation: PlayerRelation::Opponent,
                    filter: elf_filter,
                    comparator: Comparator::GE,
                    count: Box::new(QuantityExpr::Fixed { value: 1 }),
                },
            },
        };
        assert_eq!(
            resolve_quantity(&state, &controls, PlayerId(0), ObjectId(1)),
            1
        );
    }

    /// CR 109.4 + CR 109.5: discriminating coverage for the comparative
    /// "controls more <type> than you" shape. 3-player board: controller P0
    /// controls 1 creature, opponent P1 controls 2, opponent P2 controls 0.
    /// "the number of opponents who control more creatures than you" must count
    /// exactly the opponent who controls *strictly more* (P1) → 1. A `GE`
    /// comparator against the same `You` count would also include P2's tie-or-
    /// fewer reading wrongly, so this test discriminates the GT semantics.
    #[test]
    fn resolve_player_count_controls_more_creatures_than_you() {
        use crate::types::ability::{
            Comparator, ControllerRef, PlayerRelation, TypeFilter, TypedFilter,
        };
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);

        let add_creature = |state: &mut GameState, owner: PlayerId, id: u64| {
            let c = create_object(
                state,
                CardId(id),
                owner,
                format!("Bear {id}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&c)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        };
        // P0 (controller): 1 creature. P1: 2 creatures. P2: 0 creatures.
        add_creature(&mut state, PlayerId(0), 900);
        add_creature(&mut state, PlayerId(1), 901);
        add_creature(&mut state, PlayerId(1), 902);

        let bare_creature = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature));
        let you_creature = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
        );

        // GT: only P1 (2 > 1) counts; P2 (0 > 1 is false) and P0 (not an
        // opponent) do not → 1.
        let more_than_you = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::ControlsCount {
                    relation: PlayerRelation::Opponent,
                    filter: bare_creature.clone(),
                    comparator: Comparator::GT,
                    count: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: you_creature.clone(),
                        },
                    }),
                },
            },
        };
        assert_eq!(
            resolve_quantity(&state, &more_than_you, PlayerId(0), ObjectId(1)),
            1,
            "only the opponent with strictly more creatures than the controller counts"
        );

        // Discriminator: flipping the comparator to GE (>=1) would also include
        // P2, whose 0 creatures is NOT >= the controller's 1 — but P1's 2 >= 1
        // and would still count. To prove the GT/GE distinction bites, give P2
        // exactly 1 creature (tie with controller). Then GT still yields 1 (only
        // P1), but GE would yield 2 (P1 and P2).
        add_creature(&mut state, PlayerId(2), 903);
        assert_eq!(
            resolve_quantity(&state, &more_than_you, PlayerId(0), ObjectId(1)),
            1,
            "a tied opponent (P2: 1 == controller's 1) is excluded by GT"
        );
        let at_least_you = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::ControlsCount {
                    relation: PlayerRelation::Opponent,
                    filter: bare_creature,
                    comparator: Comparator::GE,
                    count: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: you_creature,
                        },
                    }),
                },
            },
        };
        assert_eq!(
            resolve_quantity(&state, &at_least_you, PlayerId(0), ObjectId(1)),
            2,
            "GE includes the tied opponent — confirms the GT result is comparator-sensitive"
        );
    }

    /// CR 122.1f: discriminating coverage for Glissa's Retriever — "the number
    /// of opponents who have three or more poison counters". The per-candidate
    /// poison total must be read off each candidate player, NOT the controller.
    /// 3-player board: opp1 poison=3 (≥3 → counts), opp2 poison=1 (<3 →
    /// excluded), CONTROLLER poison=5 (≥3 but is not an opponent → must NOT
    /// leak in). Expect 1. A controller-scoped read of the threshold would have
    /// nothing to do with this — the discriminator is that the controller's own
    /// 5 poison never inflates the result, proving `candidate_player_scalar`
    /// reads each candidate directly.
    #[test]
    fn resolve_player_count_opponents_who_have_n_poison_counters_is_per_candidate() {
        use crate::types::ability::{Comparator, PlayerRelation};
        use crate::types::format::FormatConfig;
        use crate::types::player::PlayerCounterKind;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        // Controller P0 has the MOST poison; if the read were controller-scoped
        // instead of per-candidate, every opponent would erroneously satisfy the
        // ≥3 test (the threshold N=3 is fixed; the leak would be on the attribute
        // side). P1 qualifies, P2 does not, P0 is excluded as the controller.
        state.players[0].poison_counters = 5;
        state.players[1].poison_counters = 3;
        state.players[2].poison_counters = 1;

        let glissa = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::Opponent,
                    attr: Box::new(QuantityRef::PlayerCounter {
                        kind: PlayerCounterKind::Poison,
                        scope: CountScope::ScopedPlayer,
                    }),
                    comparator: Comparator::GE,
                    value: Box::new(QuantityExpr::Fixed { value: 3 }),
                },
            },
        };
        assert_eq!(
            resolve_quantity(&state, &glissa, PlayerId(0), ObjectId(1)),
            1,
            "only opp1 (poison 3 ≥ 3) counts; opp2 (1) is excluded and the \
             controller's own 5 poison must NOT leak in — proves per-candidate read"
        );

        // Discriminator on the threshold side: dropping opp1 below the threshold
        // must drop the count to 0, confirming the comparison bites per candidate.
        state.players[1].poison_counters = 2;
        assert_eq!(
            resolve_quantity(&state, &glissa, PlayerId(0), ObjectId(1)),
            0,
            "with no opponent at ≥3, the count is 0 even though the controller has 5"
        );
    }

    /// CR 402.1: discriminating coverage for Wolfcaller's Howl — "the number of
    /// your opponents with four or more cards in hand". Hand size is read off
    /// each candidate. 3-player board: opp1 hand=4 (counts), opp2 hand=2
    /// (excluded), CONTROLLER hand=9 (not an opponent → must not leak). Expect 1.
    #[test]
    fn resolve_player_count_opponents_with_n_cards_in_hand_is_per_candidate() {
        use crate::types::ability::{Comparator, PlayerRelation, PlayerScope};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        let fill_hand = |state: &mut GameState, pid: usize, n: u64| {
            for i in 0..n {
                state.players[pid]
                    .hand
                    .push_back(ObjectId(1000 + pid as u64 * 100 + i));
            }
        };
        fill_hand(&mut state, 0, 9); // controller — must not leak
        fill_hand(&mut state, 1, 4); // opp1 — counts
        fill_hand(&mut state, 2, 2); // opp2 — excluded

        let wolfcaller = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::Opponent,
                    attr: Box::new(QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    }),
                    comparator: Comparator::GE,
                    value: Box::new(QuantityExpr::Fixed { value: 4 }),
                },
            },
        };
        assert_eq!(
            resolve_quantity(&state, &wolfcaller, PlayerId(0), ObjectId(1)),
            1,
            "only opp1 (hand 4 ≥ 4) counts; the controller's 9-card hand must not leak in"
        );
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
                counter_type: Some(CounterType::Loyalty),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 4);
    }

    #[test]
    fn resolve_quantity_counters_on_source_falls_back_to_lki() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Runecarved Obelisk".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 3);
        let lki = state.objects[&source].snapshot_for_mana_spent();
        state.lki_cache.insert(source, lki);
        state.objects.remove(&source);

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(CounterType::Generic("charge".to_string())),
            },
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 3);
    }

    #[test]
    fn resolve_quantity_counters_on_event_source_falls_back_to_lki_after_zone_change() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Runecarved Obelisk".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 3);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut events);
        state.current_trigger_event = Some(
            events
                .iter()
                .find_map(|event| match event {
                    crate::types::events::GameEvent::ZoneChanged { object_id, .. }
                        if *object_id == source =>
                    {
                        Some(event.clone())
                    }
                    _ => None,
                })
                .expect("move_to_zone must emit a ZoneChanged event"),
        );

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::EventSource,
                counter_type: Some(CounterType::Generic("charge".to_string())),
            },
        };
        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::Draw {
                count: expr.clone(),
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(99),
            PlayerId(0),
        );

        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 3);
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

    /// CR 121.1: `CardsDrawnThisTurn` reads each player's per-turn draw
    /// counter and composes through the same PlayerScope aggregate path as
    /// life gained/lost.
    #[test]
    fn resolve_quantity_cards_drawn_this_turn_max_opponent() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        state.players[0].cards_drawn_this_turn = 5;
        state.players[1].cards_drawn_this_turn = 4;
        state.players[2].cards_drawn_this_turn = 2;

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CardsDrawnThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Max,
                },
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 4);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(2), ObjectId(1)), 5);
    }

    /// CR 701.9: `CardsDiscardedThisTurn` reads the per-player discard-count
    /// map populated by discard resolution and composes through PlayerScope.
    #[test]
    fn resolve_quantity_cards_discarded_this_turn_sum_opponents() {
        let mut state = GameState::new_two_player(42);
        crate::game::restrictions::record_discard(&mut state, PlayerId(0));
        crate::game::restrictions::record_discard(&mut state, PlayerId(1));
        crate::game::restrictions::record_discard(&mut state, PlayerId(1));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 2);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(1)), 1);
    }

    /// CR 111.2: `TokensCreatedThisTurn` counts token-creation snapshots by
    /// creator and token characteristics.
    #[test]
    fn resolve_quantity_tokens_created_this_turn_filters_token_snapshots() {
        let mut state = GameState::new_two_player(42);
        let clue = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Clue".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&clue).unwrap();
            obj.controller = PlayerId(0);
            obj.is_token = true;
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.card_types.subtypes = vec!["Clue".to_string()];
        }
        crate::game::restrictions::record_token_created(&mut state, clue);

        let treasure = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.controller = PlayerId(1);
            obj.is_token = true;
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.card_types.subtypes = vec!["Treasure".to_string()];
        }
        crate::game::restrictions::record_token_created(&mut state, treasure);

        let any_tokens = QuantityExpr::Ref {
            qty: QuantityRef::TokensCreatedThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Any,
            },
        };
        let treasure_tokens = QuantityExpr::Ref {
            qty: QuantityRef::TokensCreatedThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                    "Treasure".to_string(),
                ))),
            },
        };

        assert_eq!(
            resolve_quantity(&state, &any_tokens, PlayerId(0), ObjectId(1)),
            1
        );
        assert_eq!(
            resolve_quantity(&state, &treasure_tokens, PlayerId(0), ObjectId(1)),
            1
        );
    }

    /// CR 603.4 + CR 701.25: `PlayerActionsThisTurn` counts repeated typed
    /// player-action events through the same PlayerScope aggregate path as the
    /// other turn-history quantities.
    #[test]
    fn resolve_quantity_player_actions_this_turn_counts_scoped_actions() {
        let mut state = GameState::new_two_player(42);
        state
            .player_actions_this_turn
            .push((PlayerId(0), PlayerActionKind::Surveil));
        state
            .player_actions_this_turn
            .push((PlayerId(1), PlayerActionKind::Surveil));
        state
            .player_actions_this_turn
            .push((PlayerId(1), PlayerActionKind::Surveil));
        state
            .player_actions_this_turn
            .push((PlayerId(1), PlayerActionKind::Scry));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::PlayerActionsThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
                action: PlayerActionKind::Surveil,
            },
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 2);
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(1), ObjectId(1)), 1);
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
            crate::im::Vector::from(vec![
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Creature],
                    supertypes: vec![Supertype::Legendary],
                    subtypes: vec!["Bird".to_string()],
                    keywords: vec![Keyword::Flying],
                    colors: vec![ManaColor::Blue],
                    mana_value: 3,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Artifact],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 1,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
            ]),
        );

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
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
    fn resolve_quantity_lands_played_this_turn_filters_origin_zone() {
        let mut state = GameState::new_two_player(42);
        state.players[0].lands_played_this_turn = 2;
        state.lands_played_this_turn_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![
                LandPlayRecord {
                    from_zone: Zone::Hand,
                },
                LandPlayRecord {
                    from_zone: Zone::Exile,
                },
            ]),
        );

        let unrestricted = QuantityExpr::Ref {
            qty: QuantityRef::LandsPlayedThisTurn {
                player: PlayerScope::Controller,
                from_zones: None,
            },
        };
        let outside_hand = QuantityExpr::Ref {
            qty: QuantityRef::LandsPlayedThisTurn {
                player: PlayerScope::Controller,
                from_zones: Some(vec![
                    Zone::Graveyard,
                    Zone::Library,
                    Zone::Exile,
                    Zone::Command,
                ]),
            },
        };

        assert_eq!(
            resolve_quantity(&state, &unrestricted, PlayerId(0), ObjectId(1)),
            2
        );
        assert_eq!(
            resolve_quantity(&state, &outside_hand, PlayerId(0), ObjectId(1)),
            1
        );
    }

    #[test]
    fn resolve_quantity_spells_cast_this_game_filtered_by_name() {
        // CR 117.1 + CR 201.2: Approach of the Second Sun's game-scope name
        // filter resolves against `state.spells_cast_this_game_by_player`.
        // The condition checks "another spell named ~ this game" via >= 2.
        let mut state = GameState::new_two_player(42);
        state.spells_cast_this_game_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![
                SpellCastRecord {
                    name: "Approach of the Second Sun".to_string(),
                    core_types: vec![CoreType::Sorcery],
                    ..SpellCastRecord::default()
                },
                SpellCastRecord {
                    name: "Lightning Bolt".to_string(),
                    core_types: vec![CoreType::Instant],
                    ..SpellCastRecord::default()
                },
            ]),
        );

        let filter =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Named {
                name: "approach of the second sun".to_string(),
            }]));
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisGame {
                scope: CountScope::Controller,
                filter: Some(filter),
            },
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            1,
            "name filter must match only the prior Approach cast, not Lightning Bolt"
        );

        // Cast a second copy: >= 2 should now hold ("another" semantic).
        state
            .spells_cast_this_game_by_player
            .get_mut(&PlayerId(0))
            .unwrap()
            .push_back(SpellCastRecord {
                name: "Approach of the Second Sun".to_string(),
                core_types: vec![CoreType::Sorcery],
                ..SpellCastRecord::default()
            });
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            2,
            "second Approach cast must satisfy `another spell named ~ this game` (count >= 2)"
        );
    }

    #[test]
    fn half_rounded_up_even() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::DivideRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 20 }),
            divisor: 2,
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
        let expr = QuantityExpr::DivideRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 7 }),
            divisor: 2,
            rounding: crate::types::ability::RoundingMode::Up,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 4);
    }

    #[test]
    fn half_rounded_down_odd() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::DivideRounded {
            inner: Box::new(QuantityExpr::Fixed { value: 7 }),
            divisor: 2,
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
    fn resolve_object_typeline_component_count_glistener_elf() {
        use crate::types::ability::PtValue;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Embiggen".to_string(),
            Zone::Stack,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Glistener Elf".to_string(),
            Zone::Battlefield,
        );
        {
            use crate::types::card_type::CardType;
            let obj = state.objects.get_mut(&target).unwrap();
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![
                    "Phyrexian".to_string(),
                    "Elf".to_string(),
                    "Warrior".to_string(),
                ],
            };
        }

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectTypelineComponentCount {
                scope: ObjectScope::Target,
            },
        };
        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Quantity(expr.clone()),
                toughness: PtValue::Quantity(expr.clone()),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        assert_eq!(
            resolve_quantity_with_targets(&state, &expr, &ability),
            4,
            "Creature + Phyrexian + Elf + Warrior = 4 typeline components"
        );
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
                player: crate::types::ability::TargetFilter::Controller,
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

        state.current_trigger_event = Some(crate::types::events::GameEvent::SpellCast {
            card_id: CardId(2),
            controller: PlayerId(0),
            object_id: target,
        });
        let event_source_expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectColorCount {
                scope: ObjectScope::EventSource,
            },
        };
        assert_eq!(
            resolve_quantity(&state, &event_source_expr, PlayerId(0), source),
            2
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
    fn resolve_multiply_saturates_overflow() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Multiply {
            factor: i32::MAX,
            inner: Box::new(QuantityExpr::Fixed { value: 2 }),
        };

        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            i32::MAX
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

    /// CR 603.2c: When the batched-trigger subject count is set,
    /// `EventContextAmount` reads it ahead of any event-extracted amount
    /// (issue #707).
    #[test]
    fn resolve_event_context_amount_uses_trigger_match_count_when_set() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_match_count = Some(3);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
    }

    /// CR 603.2c: With no match-count set, `EventContextAmount` falls through
    /// to `extract_amount_from_event` as before — the new precedence rule
    /// must not regress existing scalar-event resolution.
    #[test]
    fn resolve_event_context_amount_falls_through_when_match_count_none() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_match_count = None;
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

    /// CR 603.2c: When both the batched match-count and an event-extracted
    /// amount are present, the match-count wins — the trigger is "one or
    /// more <FILTER> <verb>" and the "that many" semantics belong to the
    /// filtered subject count, not the event's intrinsic amount.
    #[test]
    fn resolve_event_context_amount_match_count_takes_precedence_over_event() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_match_count = Some(7);
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 2,
            is_combat: false,
            excess: 0,
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 7);
    }

    /// CR 706.4: A die rolled earlier in the resolution outranks the
    /// triggering event's intrinsic amount, so "roll a d20. <effect> equal to
    /// the result" consumes the roll (17), not the combat damage (6). This is
    /// the building-block guard for Ancient Copper/Gold/Silver Dragon and
    /// Adorable Kitten (issue #1602).
    #[test]
    fn resolve_event_context_amount_prefers_die_result_over_event() {
        let mut state = GameState::new_two_player(42);
        state.die_result_this_resolution = Some(17);
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 6,
            is_combat: true,
            excess: 0,
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            17
        );
    }

    /// CR 706.4: With no die rolled this resolution, the cascade
    /// falls through to the triggering event's amount as before — the new slot
    /// must not regress event-extracted resolution.
    #[test]
    fn resolve_event_context_amount_falls_through_when_no_die_result() {
        let mut state = GameState::new_two_player(42);
        state.die_result_this_resolution = None;
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 6,
            is_combat: true,
            excess: 0,
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 6);
    }

    /// CR 603.2c + CR 706.2: The batched-trigger match-count still outranks a
    /// die result — the die slot is inserted BELOW match-count in the cascade,
    /// so a "one or more <FILTER>" batched trigger keeps its filtered-subject
    /// "that many" semantics even when a die was rolled this resolution.
    #[test]
    fn resolve_event_context_amount_match_count_outranks_die_result() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_match_count = Some(3);
        state.die_result_this_resolution = Some(17);
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 3);
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
            qty: QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
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
                base_power: Some(6),
                base_toughness: Some(5),
                mana_value: 3,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        );
        state.current_trigger_event =
            Some(crate::types::events::GameEvent::CreatureDestroyed { object_id: dead_id });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(99)),
            6
        );
    }

    /// CR 400.7j + CR 117.1 + CR 608.2k: Regression guard for Greater Good
    /// (issue #334). When an activated ability with a sacrifice cost
    /// references "the sacrificed creature's power", the parser emits
    /// `Power { scope: CostPaidObject }`. No trigger event is in scope for
    /// activated abilities, so the resolver must fall back (slot 1) to the
    /// resolving ability's `cost_paid_object` snapshot captured at
    /// cost-payment time.
    #[test]
    fn resolve_event_context_source_power_cost_paid_object_fallback() {
        use crate::types::ability::{CostPaidObjectSnapshot, ResolvedAbility};
        use crate::types::game_state::LKISnapshot;

        let state = GameState::new_two_player(42);
        // No current_trigger_event — this is an activated ability resolution.
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::CostPaidObject,
                    },
                },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        ability.set_cost_paid_object_recursive(CostPaidObjectSnapshot {
            object_id: ObjectId(99),
            lki: LKISnapshot {
                name: "Regal Force".to_string(),
                power: Some(5),
                toughness: Some(5),
                base_power: Some(5),
                base_toughness: Some(5),
                mana_value: 6,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![ManaColor::Green],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        });
        let power = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(
            power, 5,
            "Power {{ CostPaidObject }} must fall back to cost-paid object's LKI power \
             when no trigger event is in scope (Greater Good: sacrificed 5/5 → draw 5)"
        );

        let toughness = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(toughness, 5);
        let cmc = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(cmc, 6);
    }

    /// Regression guard: when neither a trigger event nor a cost-paid-object
    /// snapshot is in scope, `Power { CostPaidObject }` must still return 0
    /// rather than panic or hit an unexpected fallback (e.g. the source
    /// object's own power).
    #[test]
    fn resolve_event_context_source_power_no_context_returns_zero() {
        use crate::types::ability::ResolvedAbility;

        let mut state = GameState::new_two_player(42);
        // Source has a real power, to prove we DON'T read it.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Greater Good".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().power = Some(7);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::CostPaidObject,
                    },
                },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(
            resolved, 0,
            "Power {{ CostPaidObject }} with no trigger event and no cost-paid \
             snapshot must return 0 (not the source object's own power)"
        );
    }

    /// CR 608.2k slot ordering: when both a cost-paid-object snapshot (slot 1)
    /// and a trigger event (slot 2) are in scope (theoretical — triggered
    /// abilities don't carry activation costs in practice), `cost_paid_object`
    /// wins per the engine's pinned slot-1-first order. Guards the fallback
    /// ordering contract.
    #[test]
    fn resolve_power_cost_paid_object_takes_priority_over_trigger_event() {
        use crate::types::ability::{CostPaidObjectSnapshot, ResolvedAbility};
        use crate::types::game_state::LKISnapshot;

        let mut state = GameState::new_two_player(42);
        let trigger_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Triggering Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&trigger_source).unwrap().power = Some(3);
        state.current_trigger_event = Some(crate::types::events::GameEvent::DamageDealt {
            source_id: trigger_source,
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        });

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::CostPaidObject,
                    },
                },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        // Cost-paid snapshot with a DIFFERENT power, so we can detect which path won.
        ability.set_cost_paid_object_recursive(CostPaidObjectSnapshot {
            object_id: ObjectId(99),
            lki: LKISnapshot {
                name: "Sacrificed Hulk".to_string(),
                power: Some(99),
                toughness: Some(99),
                base_power: Some(99),
                base_toughness: Some(99),
                mana_value: 99,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        });
        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(
            resolved, 99,
            "CR 608.2k slot 1 (cost_paid_object) takes priority over slot 2 \
             (trigger-event source) per the engine's pinned ordering"
        );
    }

    /// CR 608.2k + CR 202.3 + CR 701.21a — Birthing Ritual (issue #420b): the
    /// resolution-effect `Sacrifice` ("you may sacrifice a creature. If you do,
    /// ... put a creature card with mana value X or less ..., where X is 1 plus
    /// the sacrificed creature's mana value") records the sacrificed permanent
    /// into `effect_context_object`, not `cost_paid_object` (which is only set
    /// for activation/cast *costs*). The parser emits
    /// `Offset { Ref(ObjectManaValue { CostPaidObject }), +1 }` for the X bound;
    /// the `CostPaidObject` mana-value resolver must fall back to
    /// `effect_context_object` so the bound resolves to (sac MV + 1) rather
    /// than 0. Sacrificed MV 3 → bound 4.
    #[test]
    fn resolve_object_mana_value_cost_paid_falls_back_to_effect_context_object() {
        use crate::types::ability::{CostPaidObjectSnapshot, ResolvedAbility};
        use crate::types::game_state::LKISnapshot;

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        // The sacrificed creature (MV 3) is recorded as the effect-context
        // object — no cost-paid object exists for a resolution-effect sacrifice.
        ability.set_effect_context_object_recursive(CostPaidObjectSnapshot {
            object_id: ObjectId(50),
            lki: LKISnapshot {
                name: "Sacrificed Creature".to_string(),
                power: Some(2),
                toughness: Some(2),
                base_power: Some(2),
                base_toughness: Some(2),
                mana_value: 3,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        });
        assert!(
            ability.cost_paid_object.is_none(),
            "precondition: no cost-paid object for a resolution-effect sacrifice"
        );

        // The exact AST the concurrent parser agent emits for "X is 1 plus the
        // sacrificed creature's mana value".
        let bound = QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
            offset: 1,
        };
        assert_eq!(
            resolve_quantity_with_targets(&state, &bound, &ability),
            4,
            "CostPaidObject mana value must fall back to effect_context_object \
             (MV 3) so the X bound resolves to 1 + 3 = 4"
        );
    }

    /// CR 608.2k — precedence: when both `cost_paid_object` and
    /// `effect_context_object` are present, `cost_paid_object` (the canonical
    /// referent for `ObjectScope::CostPaidObject`) wins.
    #[test]
    fn resolve_object_mana_value_cost_paid_object_takes_priority_over_effect_context() {
        use crate::types::ability::{CostPaidObjectSnapshot, ResolvedAbility};
        use crate::types::game_state::LKISnapshot;

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let snapshot = |name: &str, mana_value: u32| CostPaidObjectSnapshot {
            object_id: ObjectId(50),
            lki: LKISnapshot {
                name: name.to_string(),
                power: Some(1),
                toughness: Some(1),
                base_power: Some(1),
                base_toughness: Some(1),
                mana_value,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        };
        // Both fields set, with DIFFERENT mana values so the winning path is
        // observable.
        ability.set_effect_context_object_recursive(snapshot("Effect Context", 9));
        ability.set_cost_paid_object_recursive(snapshot("Cost Paid", 3));

        let bound = QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
            offset: 1,
        };
        assert_eq!(
            resolve_quantity_with_targets(&state, &bound, &ability),
            4,
            "cost_paid_object (MV 3) must win over effect_context_object (MV 9): \
             1 + 3 = 4"
        );
    }

    /// CR 608.2c — `ObjectScope::Anaphoric` mana value reads
    /// `effect_context_object` (the earlier-instruction referent) as slot 1.
    #[test]
    fn resolve_object_mana_value_anaphoric_reads_effect_context_object() {
        use crate::types::ability::{CostPaidObjectSnapshot, ResolvedAbility};
        use crate::types::game_state::LKISnapshot;

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        ability.set_effect_context_object_recursive(CostPaidObjectSnapshot {
            object_id: ObjectId(50),
            lki: LKISnapshot {
                name: "Revealed Card".to_string(),
                power: Some(0),
                toughness: Some(0),
                base_power: Some(0),
                base_toughness: Some(0),
                mana_value: 3,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Sorcery],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        });
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Anaphoric,
            },
        };
        assert_eq!(
            resolve_quantity_with_targets(&state, &expr, &ability),
            3,
            "Anaphoric slot 1 must read effect_context_object's mana value"
        );
    }

    /// CR 608.2c vs CR 608.2k — divergent priority pin: when both slots are
    /// populated, `Anaphoric` reads `effect_context_object` (608.2c) while
    /// `CostPaidObject` reads `cost_paid_object` (608.2k). This is the test
    /// that locks the two arms' priority split.
    #[test]
    fn resolve_object_mana_value_anaphoric_vs_cost_paid_divergent_priority() {
        use crate::types::ability::{CostPaidObjectSnapshot, ResolvedAbility};
        use crate::types::game_state::LKISnapshot;

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let snapshot = |name: &str, mana_value: u32| CostPaidObjectSnapshot {
            object_id: ObjectId(50),
            lki: LKISnapshot {
                name: name.to_string(),
                power: Some(1),
                toughness: Some(1),
                base_power: Some(1),
                base_toughness: Some(1),
                mana_value,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        };
        ability.set_effect_context_object_recursive(snapshot("Effect Context", 7));
        ability.set_cost_paid_object_recursive(snapshot("Cost Paid", 3));

        let anaphoric = QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Anaphoric,
            },
        };
        let cost_paid = QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject,
            },
        };
        assert_eq!(
            resolve_quantity_with_targets(&state, &anaphoric, &ability),
            7,
            "Anaphoric must read effect_context_object (CR 608.2c slot 1)"
        );
        assert_eq!(
            resolve_quantity_with_targets(&state, &cost_paid, &ability),
            3,
            "CostPaidObject must still read cost_paid_object (CR 608.2k slot 1)"
        );
    }

    /// CR 608.2c — `ObjectScope::Anaphoric` power reads `effect_context_object`
    /// as slot 1 (the `resolve_object_pt` analogue of the mana-value test).
    #[test]
    fn resolve_object_pt_anaphoric_reads_effect_context_object() {
        use crate::types::ability::{CostPaidObjectSnapshot, ResolvedAbility};
        use crate::types::game_state::LKISnapshot;

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let snapshot = |name: &str, power: i32| CostPaidObjectSnapshot {
            object_id: ObjectId(50),
            lki: LKISnapshot {
                name: name.to_string(),
                power: Some(power),
                toughness: Some(power),
                base_power: Some(power),
                base_toughness: Some(power),
                mana_value: 0,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
            },
        };
        ability.set_effect_context_object_recursive(snapshot("Effect Context", 5));
        ability.set_cost_paid_object_recursive(snapshot("Cost Paid", 2));

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Anaphoric,
            },
        };
        assert_eq!(
            resolve_quantity_with_targets(&state, &expr, &ability),
            5,
            "Anaphoric power must read effect_context_object (CR 608.2c slot 1)"
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
                base_power: Some(3),
                base_toughness: Some(3),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
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

    /// CR 107.3: `Power { base, exponent }` computes `base^exponent`. With a
    /// fixed exponent the math is straightforward.
    #[test]
    fn resolve_power_fixed_exponent() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Power {
            base: 2,
            exponent: Box::new(QuantityExpr::Fixed { value: 5 }),
        };
        assert_eq!(
            resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)),
            32
        );
    }

    /// CR 107.3 + CR 107.3a: Mathemagics — "draws 2ˣ cards" with X=6 must
    /// resolve to 64. This is the bug report this variant was added for:
    /// before the fix, the parser silently dropped the `ˣ` and produced
    /// `Fixed { value: 2 }`, so casting with X=6 drew 2 cards instead of 64.
    #[test]
    fn resolve_power_variable_x_via_chosen_x() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::Power {
            base: 2,
            exponent: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }),
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
        ability.chosen_x = Some(6);
        assert_eq!(resolve_quantity_with_targets(&state, &expr, &ability), 64);
    }

    /// CR 107.3 + CR 107.1b: Overflow saturates rather than panicking; a
    /// negative resolved exponent clamps to 0 (yielding `base^0 = 1`).
    #[test]
    fn resolve_power_saturates_and_clamps() {
        let state = GameState::new_two_player(42);

        // 2^31 overflows i32 — must saturate to i32::MAX, not panic.
        let big = QuantityExpr::Power {
            base: 2,
            exponent: Box::new(QuantityExpr::Fixed { value: 31 }),
        };
        assert_eq!(
            resolve_quantity(&state, &big, PlayerId(0), ObjectId(1)),
            i32::MAX
        );

        // Negative exponent clamps to 0; 5^0 = 1.
        let neg = QuantityExpr::Power {
            base: 5,
            exponent: Box::new(QuantityExpr::Fixed { value: -3 }),
        };
        assert_eq!(resolve_quantity(&state, &neg, PlayerId(0), ObjectId(1)), 1);
    }

    /// CR 107.1b: A negative effect-result calculation uses zero instead.
    #[test]
    fn resolve_clamp_min_floors_negative_quantity_result() {
        let state = GameState::new_two_player(42);
        let expr = QuantityExpr::ClampMin {
            inner: Box::new(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Fixed { value: 0 }),
                offset: -1,
            }),
            minimum: 0,
        };

        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), ObjectId(1)), 0);
    }

    /// CR 603.4 + CR 109.3: The `Aggregate` resolver must exclude the
    /// triggering object from its population when the filter carries
    /// `FilterProp::OtherThanTriggerObject`. This mirrors the existing
    /// `ObjectCount` exclusion path and supports the Selvala-class
    /// "each other creature's power" superlative comparison.
    #[test]
    fn resolve_aggregate_other_than_trigger_object_excludes_triggering() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Selvala".to_string(),
            Zone::Battlefield,
        );
        // Three creatures on the battlefield with power 2, 4, 6.
        let c1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        let c2 = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        let triggering = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Triggering Creature".to_string(),
            Zone::Battlefield,
        );
        for (id, p, t) in [(c1, 2i32, 2i32), (c2, 4, 4), (triggering, 6, 6)] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(p);
            obj.toughness = Some(t);
        }
        // Set the triggering event to be the entering of `triggering`.
        state.current_trigger_event = Some(crate::types::events::GameEvent::ZoneChanged {
            object_id: triggering,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                triggering,
                None,
                Zone::Battlefield,
            )),
        });

        let other_creatures = || {
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::OtherThanTriggerObject]),
            )
        };
        // Max: triggering creature (power 6) must be excluded; max of remaining is 4.
        let max_expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Max,
                property: ObjectProperty::Power,
                filter: other_creatures(),
            },
        };
        assert_eq!(resolve_quantity(&state, &max_expr, PlayerId(0), source), 4);

        // Sum: the folded `object_count_matching_ids` delegation must keep the
        // `OtherThanTriggerObject` exclusion — sum is 2 + 4 = 6 (NOT 12 with the
        // triggering 6/6 included).
        let sum_expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                filter: other_creatures(),
            },
        };
        assert_eq!(resolve_quantity(&state, &sum_expr, PlayerId(0), source), 6);
    }

    /// Issue #338 — Greater Good end-to-end. Drives the REAL engine pipeline:
    /// parse the card from Oracle text, activate `Sacrifice a creature: Draw
    /// cards equal to the sacrificed creature's power`, sacrifice a 4/4, and
    /// resolve. Two assertions discriminate the fix:
    ///
    /// 1. **Parser** — the draw quantity must parse to
    ///    `Power { ObjectScope::CostPaidObject }`, NOT a trigger-event-context
    ///    referent. This assertion FAILS on pre-fix code, where the participle-possessive
    ///    "the sacrificed creature's power" was mis-classified as an
    ///    event-context referent.
    /// 2. **Runtime** — driving the engine through `apply_as_current`, exactly
    ///    4 cards (the sacrificed 4/4's power) are drawn before the sub-ability
    ///    discards 3.
    ///
    /// CR 608.2k: the sacrificed creature is a specific untargeted object
    /// previously referred to by the ability's cost.
    #[test]
    fn greater_good_draws_equal_to_sacrificed_creature_power_end_to_end() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::actions::GameAction;
        use crate::types::game_state::{PayCostKind, WaitingFor};
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // Greater Good is an enchantment; modeled here as a permanent carrying
        // the activated ability parsed from its real Oracle text.
        let greater_good = scenario
            .add_creature_from_oracle(
                P0,
                "Greater Good",
                0,
                0,
                "Sacrifice a creature: Draw cards equal to the sacrificed \
                 creature's power, then discard three cards.",
            )
            .id();

        // The creature to sacrifice — a 4/4. Its power (4) is the draw count.
        let victim = scenario.add_creature(P0, "Sacrificial Ox", 4, 4).id();

        // Library must hold enough cards for the draw to be observable.
        scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8"]);

        let mut runner = scenario.build();

        // The draw quantity must have parsed through the cost-paid-object
        // chain — NOT a trigger-event-context referent. This is the
        // discriminating assertion that fails on pre-fix code.
        let draw_count = {
            let gg = &runner.state().objects[&greater_good];
            let ability = gg
                .abilities
                .iter()
                .find(|a| matches!(*a.effect, Effect::Draw { .. }))
                .expect("Greater Good must parse a Draw activated ability");
            match &*ability.effect {
                Effect::Draw { count, .. } => count.clone(),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            draw_count,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            "Greater Good's draw quantity must route through \
             parse_cost_paid_object_ref → Power{{CostPaidObject}}, not a \
             trigger-event-context referent (issue #338)"
        );

        let hand_count = |runner: &crate::game::scenario::GameRunner| {
            runner
                .state()
                .players
                .iter()
                .find(|p| p.id == P0)
                .map(|p| p.hand.len())
                .unwrap()
        };
        let hand_before = hand_count(&runner);

        // Activate the sacrifice ability.
        let ability_index = runner.state().objects[&greater_good]
            .abilities
            .iter()
            .position(|a| matches!(*a.effect, Effect::Draw { .. }))
            .unwrap();
        runner
            .act(GameAction::ActivateAbility {
                source_id: greater_good,
                ability_index,
            })
            .expect("activation must succeed");

        // CR 701.21a: paying a "sacrifice a creature" activation cost requires
        // the controller to choose which permanent to sacrifice.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::PayCost {
                    kind: PayCostKind::Sacrifice,
                    ..
                }
            ),
            "activating a sacrifice-cost ability must prompt for the permanent"
        );
        runner
            .act(GameAction::SelectCards {
                cards: vec![victim],
            })
            .expect("sacrifice selection must succeed");

        // Resolve everything on the stack.
        // Resolve the Draw, stopping at the discard-choice prompt (the "then
        // discard three cards" sub-ability requires the controller to pick
        // cards). The discriminating signal is the draw count, so the test
        // asserts the hand at the moment the draw has fully resolved.
        for _ in 0..40 {
            match runner.state().waiting_for {
                WaitingFor::DiscardChoice { .. } => break,
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                _ => {}
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }

        // The 4/4 was sacrificed → exactly 4 cards drawn (its power). On
        // pre-fix code the quantity mis-classified to a trigger-event-context
        // referent; the parser assertion above is the primary discriminator, this
        // confirms the value resolves correctly end-to-end.
        let hand_after = hand_count(&runner);
        assert_eq!(
            hand_after,
            hand_before + 4,
            "Greater Good must draw exactly 4 cards equal to the sacrificed \
             4/4's power (hand before {hand_before}, after {hand_after})"
        );
        // The sacrificed creature must have left the battlefield.
        assert!(
            !runner.state().battlefield.contains(&victim),
            "the sacrificed creature must have left the battlefield"
        );
    }

    /// Create `n` battlefield lands controlled by `owner`.
    fn add_lands(state: &mut GameState, owner: PlayerId, n: usize) {
        for i in 0..n {
            let id = create_object(
                state,
                CardId(9000 + i as u64),
                owner,
                "Forest".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
    }

    fn lands_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))
    }

    #[test]
    fn controlled_by_each_player_min_picks_the_fewest() {
        // CR 107.1: P0 has 3 lands, P1 has 1 → Min resolves to 1.
        let mut state = GameState::new_two_player(42);
        add_lands(&mut state, PlayerId(0), 3);
        add_lands(&mut state, PlayerId(1), 1);
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ControlledByEachPlayer {
                filter: lands_filter(),
                aggregate: AggregateFunction::Min,
            },
        };
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0)), 1);
    }

    #[test]
    fn controlled_by_each_player_max_picks_the_most() {
        // P0 has 3 lands, P1 has 1 → Max resolves to 3.
        let mut state = GameState::new_two_player(42);
        add_lands(&mut state, PlayerId(0), 3);
        add_lands(&mut state, PlayerId(1), 1);
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ControlledByEachPlayer {
                filter: lands_filter(),
                aggregate: AggregateFunction::Max,
            },
        };
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0)), 3);
    }

    #[test]
    fn controlled_by_each_player_min_zero_when_a_player_has_none() {
        // A player controlling no matching objects drives Min to 0.
        let mut state = GameState::new_two_player(42);
        add_lands(&mut state, PlayerId(0), 4);
        // P1 has no lands.
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ControlledByEachPlayer {
                filter: lands_filter(),
                aggregate: AggregateFunction::Min,
            },
        };
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0)), 0);
    }

    #[test]
    fn controlled_by_each_player_min_across_three_players() {
        // CR 102.1: the aggregate spans every player — P0=5, P1=2, P2=4 → Min 2.
        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 42);
        add_lands(&mut state, PlayerId(0), 5);
        add_lands(&mut state, PlayerId(1), 2);
        add_lands(&mut state, PlayerId(2), 4);
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::ControlledByEachPlayer {
                filter: lands_filter(),
                aggregate: AggregateFunction::Min,
            },
        };
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0)), 2);
    }

    #[test]
    fn controlled_by_each_player_prefers_clause_snapshot() {
        // CR 608.2e: when a clause-local snapshot is present, the resolver
        // returns the frozen value rather than the live board count — proving
        // the snapshot mechanism overrides live evaluation.
        let mut state = GameState::new_two_player(42);
        add_lands(&mut state, PlayerId(0), 3);
        add_lands(&mut state, PlayerId(1), 1);
        let qref = QuantityRef::ControlledByEachPlayer {
            filter: lands_filter(),
            aggregate: AggregateFunction::Min,
        };
        // Live board would yield 1; freeze a different value into the snapshot.
        let mut snap = crate::types::game_state::ClauseMinimumSnapshot::default();
        snap.insert(qref.clone(), 7);
        state.clause_minimum_snapshot = Some(snap);
        let qty = QuantityExpr::Ref { qty: qref };
        assert_eq!(
            resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0)),
            7,
            "snapshot value must override the live board count"
        );
    }

    #[test]
    fn hand_size_all_players_min_prefers_clause_snapshot() {
        // CR 608.2e: the discard clause's `HandSize { AllPlayers { Min } }`
        // also honors the clause snapshot. Live hands differ from the frozen
        // value; the frozen value must win.
        let mut state = GameState::new_two_player(42);
        let qref = QuantityRef::HandSize {
            player: PlayerScope::AllPlayers {
                aggregate: AggregateFunction::Min,
                exclude: None,
            },
        };
        let mut snap = crate::types::game_state::ClauseMinimumSnapshot::default();
        snap.insert(qref.clone(), 5);
        state.clause_minimum_snapshot = Some(snap);
        let qty = QuantityExpr::Ref { qty: qref };
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0)), 5);
    }

    #[test]
    fn hand_size_all_players_min_live_when_no_snapshot() {
        // Without a snapshot, `HandSize { AllPlayers { Min } }` resolves live —
        // the empty starting hands of a fresh two-player game give Min 0.
        let state = GameState::new_two_player(42);
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Min,
                    exclude: None,
                },
            },
        };
        assert_eq!(resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0)), 0);
    }

    /// CR 608.2h + CR 400.7: `Aggregate{Sum, Power}` over an exile-link filter
    /// must read the LKI-cached at-exile power, not the live `obj.power`
    /// (which is `None` off battlefield because the layer system does not
    /// maintain P/T for non-battlefield objects).
    ///
    /// This is the Oversimplify regression test: pre-fix, the aggregate
    /// returned 0 for every exiled creature because `obj.power.unwrap_or(0)`
    /// silently collapsed. The fix at `resolve_ref`'s `Aggregate` arm chains
    /// `state.lki_cache.get(&id)` after the live read.
    #[test]
    fn aggregate_power_lki_fallback_for_exiled_creatures() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Oversimplify".to_string(),
            Zone::Stack,
        );

        // Two exiled creatures with no live power (off battlefield), each
        // having an LKI snapshot. The aggregate must sum the LKI values.
        for (card_id, power, toughness) in [(51, 3i32, 3i32), (52, 5, 5)] {
            let exiled = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Exiled {card_id}"),
                Zone::Exile,
            );
            // Live power is None (the runtime path that exiles a battlefield
            // creature unsets `power` because the layer system stops applying).
            {
                let obj = state.objects.get_mut(&exiled).unwrap();
                obj.power = None;
                obj.toughness = None;
                obj.card_types.core_types.push(CoreType::Creature);
            }
            state.lki_cache.insert(
                exiled,
                crate::types::game_state::LKISnapshot {
                    name: format!("Exiled {card_id}"),
                    power: Some(power),
                    toughness: Some(toughness),
                    base_power: Some(power),
                    base_toughness: Some(toughness),
                    mana_value: 0,
                    controller: PlayerId(0),
                    owner: PlayerId(0),
                    card_types: vec![CoreType::Creature],
                    subtypes: vec![],
                    supertypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    counters: Default::default(),
                    chosen_attributes: vec![],
                },
            );
            state.exile_links.push(ExileLink {
                source_id: source,
                exiled_id: exiled,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        let expr = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                filter: TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::creature()),
                        TargetFilter::ExiledBySource,
                    ],
                },
            },
        };

        // 3 + 5 = 8 from LKI; pre-fix this returned 0 because obj.power was None.
        assert_eq!(resolve_quantity(&state, &expr, PlayerId(0), source), 8);
    }
}
