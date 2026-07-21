//! Mana source analysis for auto-pay and AI candidate generation.
//!
//! `ManaSourceOption` describes a single activatable mana path, annotated
//! with a single typed `ManaSourcePenalty` that captures the full penalty
//! axis (damage on resolution, pay-life on activation, sacrifice) plus its
//! amount where known. Every consumer (auto-tap sort, `max_x_value`
//! free-producer gating, `UntapLandForMana` undoability) reads the penalty
//! via the enum's methods — no consumer re-inspects the underlying
//! `AbilityDefinition`, eliminating the drift class where independent bool
//! flags could diverge between construction and consumption.
//!
//! CR 605.3a defines mana abilities; CR 605.3b establishes their atomic
//! inline resolution — which is why any irreversible sub-effect (damage,
//! life loss, sacrifice) disqualifies a source from UI-level undo.

use crate::types::ability::ManaSpendRestriction;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, ManaProduction,
    PlayerFilter, QuantityExpr, TargetFilter, TriggerDefinition, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::card_type::Supertype;
use crate::types::game_state::{GameState, ProductionOverride};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{
    ManaColor, ManaCostShard, ManaPip, ManaRestriction, ManaType, PaymentContext,
};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use crate::types::TriggerMode;

use super::mana_abilities;
use super::mana_payment;
use super::restrictions;
use super::triggers::trigger_source_context_for_latch;

/// CR 605.3b — Complete classification of a mana ability's penalty axis.
///
/// Constructed **once** per `ManaSourceOption` in `scan_mana_abilities` via
/// `mana_ability_penalty`. Every consumer reads this via the provided methods;
/// no consumer re-inspects the underlying `AbilityDefinition`. This is the
/// single-authority design that eliminates drift between auto-tap ordering,
/// `max_x_value` gating, and `UntapLandForMana` undoability.
///
/// Ordering of variants is NOT significant — callers must go through
/// `tier_byte()` + `priority_amount()` for sort, `expected_life_cost()` for
/// AI scoring, `is_undoable()` for UI undo gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManaSourcePenalty {
    /// Tap-only cost, no controller-harming continuation, and the resolution
    /// chain is pure `Effect::Mana`. Basic lands, mana dorks, mana rocks,
    /// filter lands, Verge lands, commander-color lands, Chromatic Lantern, etc.
    None,

    /// CR 605.3b: The resolution chain contains a non-mana sub-effect that is
    /// not classified into a more specific variant (i.e., not damage to
    /// controller, not life loss to controller). Examples include depletion-
    /// counter lands (Gemstone Mine), self-haste mana lands (Urza's Tower
    /// variants), `RemoveCounter`-on-self lands, and `Effect::Unimplemented`
    /// continuations whose semantics the parser hasn't decoded yet.
    ///
    /// Conservative default: any side-effect we don't otherwise understand is
    /// assumed to commit irreversible game state, so `is_undoable()` returns
    /// `false`. Sorts within the same outer bucket as `None` (`tier_byte() = 0`)
    /// but with a worse `priority_amount()` than basics, so auto-tap still
    /// prefers truly free sources first.
    HasIrreversibleContinuation,

    /// CR 605.3b: Resolution chain contains a `DealDamage { target: Controller }`
    /// and/or `LoseLife { target: Controller|None }` whose amounts sum to a
    /// fixed total. `fixed_amount = None` means the chain contains a non-fixed
    /// (`QuantityExpr::Ref` / `DivideRounded` / etc.) amount — treated as
    /// maximally bad in `priority_amount()` for conservative auto-tap.
    ///
    /// Examples: painlands (1), Ancient Tomb (2), future N-damage lands,
    /// hypothetical X-damage pain land (`None`).
    DealsDamageOnResolution { fixed_amount: Option<u16> },

    /// Cost contains `AbilityCost::PayLife { amount }`. `fixed_amount` is
    /// `Some(n)` iff the `QuantityExpr` collapses to `Fixed { value: n }`;
    /// otherwise `None` (War-Room-style commanders'-colors cost).
    ///
    /// Examples: Mana Confluence (1), Starting Town (1).
    PaysLifeOnActivation { fixed_amount: Option<u16> },

    /// CR 605.3b + CR 701.21: Cost contains an `AbilityCost::Sacrifice(_)`
    /// component — bare or nested in a `Composite`, for any target filter. The
    /// activation sacrifices a permanent (the source itself OR another), so the
    /// sacrifice is irreversible and the activation is never rewindable.
    ///
    /// Covers both self-sac tokens (Treasure, Gold, Lotus Petal —
    /// `Composite[Tap, Sacrifice{SelfRef}]`) and sacrifice-engine mana sources
    /// (Krark-Clan Ironworks — bare `Sacrifice{Typed(Artifact)}`; Ashnod's
    /// Altar — `Sacrifice{Typed(Creature)}`; Phyrexian Tower).
    Sacrifices,
}

impl ManaSourcePenalty {
    /// High-order auto-tap bucket. **Lower = preferred.** The sort caller
    /// composes this with `card_tier` (land vs creature-animated vs dork vs
    /// deprioritized) and `priority_amount()` to form the final key.
    ///
    /// Bucket layout:
    ///   0 → None, HasIrreversibleContinuation, DealsDamageOnResolution,
    ///       PaysLifeOnActivation (all live in the card-type-dominated
    ///        ordering — a painland producing colored mana should still
    ///        outrank a combat-relevant mana dork of the same color)
    ///   1 → Sacrifices (always last — source will not come back)
    ///
    /// This preserves the historical behavior where painlands (tier 0 +
    /// `harms_controller=true`) sorted before mana dorks (tier 1 +
    /// `harms_controller=false`): `card_tier` dominates, penalty is a
    /// *within-card-tier* tiebreak via `priority_amount()`.
    pub fn tier_byte(self) -> u8 {
        match self {
            Self::None
            | Self::HasIrreversibleContinuation
            | Self::DealsDamageOnResolution { .. }
            | Self::PaysLifeOnActivation { .. } => 0,
            Self::Sacrifices => 1,
        }
    }

    /// Within-bucket tiebreak. **Lower = preferred.** Packs a sub-tier into
    /// the high 16 bits and the fixed amount into the low 16 bits:
    ///
    ///   sub_tier 0 → None (no penalty at all)
    ///   sub_tier 1 → HasIrreversibleContinuation (unknown side effect)
    ///   sub_tier 2 → DealsDamageOnResolution (sort by amount)
    ///   sub_tier 3 → PaysLifeOnActivation    (sort by amount)
    ///
    /// `Some(n)` → `n`, `None` → `u16::MAX` (conservative "unknown = worst"
    /// tiebreak). Sacrifices returns 0 because `tier_byte()` already sorts
    /// it last — no within-bucket distinction is needed.
    pub fn priority_amount(self) -> u32 {
        const SUB_NONE: u32 = 0 << 16;
        const SUB_IRREVERSIBLE: u32 = 1 << 16;
        const SUB_DAMAGE: u32 = 2 << 16;
        const SUB_PAY_LIFE: u32 = 3 << 16;

        fn amt(a: Option<u16>) -> u32 {
            a.unwrap_or(u16::MAX) as u32
        }

        match self {
            Self::None => SUB_NONE,
            Self::HasIrreversibleContinuation => SUB_IRREVERSIBLE,
            Self::DealsDamageOnResolution { fixed_amount } => SUB_DAMAGE | amt(fixed_amount),
            Self::PaysLifeOnActivation { fixed_amount } => SUB_PAY_LIFE | amt(fixed_amount),
            Self::Sacrifices => 0,
        }
    }

    /// CR 120.3: Damage dealt to a player by a source without infect causes
    /// that player to lose that much life — so damage-to-controller and
    /// pay-life are equivalent 1:1 life debits for AI scoring purposes.
    /// Sacrifice is not a life cost (scored elsewhere in AI); `None`
    /// amounts return 0 rather than guessing — AI policies that care about
    /// variable cost must score it themselves with game context.
    pub fn expected_life_cost(self) -> u32 {
        match self {
            Self::None | Self::HasIrreversibleContinuation | Self::Sacrifices => 0,
            Self::DealsDamageOnResolution { fixed_amount }
            | Self::PaysLifeOnActivation { fixed_amount } => fixed_amount.unwrap_or(0) as u32,
        }
    }

    /// CR 605.3b: An activation is undoable only when nothing irreversible
    /// happens — no damage, no life pay, no sacrifice, no unclassified
    /// non-mana sub-effect. The classification precedence in
    /// `mana_ability_penalty` guarantees that `None` is reached *only* when
    /// the resolution chain is pure `Effect::Mana`; every other chain shape
    /// routes to `HasIrreversibleContinuation` or a more specific harm
    /// variant. So this collapses cleanly into a single arm.
    pub fn is_undoable(self) -> bool {
        matches!(self, Self::None)
    }

    /// Convenience for `max_x_value` / free-producer counting: the source
    /// imposes no irreversible cost on activation.
    pub fn is_free(self) -> bool {
        matches!(self, Self::None)
    }

    /// CR 605.3a: a player may activate a mana ability whenever they have
    /// priority, so a priority window offering one is only auto-passable when
    /// the activation is pure, reversible mana production. A `Sacrifices`
    /// activation is different: the sacrificed permanent leaving the
    /// battlefield is a goal a player may independently want (CR 603.6
    /// leaves-the-battlefield triggers, recursion engines, graveyard-matters,
    /// sac-fodder), so it is a meaningful priority decision. Life/damage/
    /// depletion penalties are pure costs no rational player pays with nothing
    /// to spend the mana on (mana empties at end of step per CR 500.5), so they
    /// stay auto-passable.
    pub fn is_meaningful_priority_activation(self) -> bool {
        matches!(self, Self::Sacrifices)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManaSourceOption {
    pub object_id: ObjectId,
    pub ability_index: Option<usize>,
    /// Indexing/representative color for this row.
    /// For normal sources this is the exact mana type produced.
    /// For filter-land combination rows (`atomic_combination.is_some()`),
    /// this mirrors the first color of the combination and is used only for
    /// shard-matching lookup; the full mana production is driven by
    /// `atomic_combination`.
    pub mana_type: ManaType,
    /// True when the source object could produce two or more colors of mana.
    /// Used for `{Z}` cost payment.
    pub source_could_produce_two_or_more_colors: bool,
    /// CR 605.3b — classification of this activation's penalty axis.
    /// Single source of truth for auto-tap prioritization, `max_x_value`
    /// free-producer gating, AI life-cost scoring, and `UntapLandForMana`
    /// undoability. Constructed by `scan_mana_abilities` via
    /// `mana_ability_penalty`.
    pub penalty: ManaSourcePenalty,
    /// CR 605.3b + CR 106.1a/b: Complete pre-chosen multi-mana sequence for a
    /// single activation. When `Some`, one activation of this ability produces
    /// **every** mana type listed here atomically — the shard assigner must treat
    /// all combos sharing the same `(object_id, ability_index)` as alternatives
    /// (pick at most one).
    ///
    /// For aura-bonus options (Wild Growth, Fertile Ground), the land's own types
    /// come first followed by the aura's bonus types. `taps_for_mana_overrides`
    /// holds the per-aura color choice for the resolver so the land's own ability
    /// is not incorrectly asked to over-produce.
    pub atomic_combination: Option<Vec<ManaType>>,
    /// CR 106.6: Resolved spend restrictions attached to the mana this option
    /// produces. Auto-tap filters with the same `PaymentContext` used by the
    /// eventual pool spend, so it does not tap a source whose mana would be
    /// rejected after production.
    pub restrictions: Vec<ManaRestriction>,
    /// Per-aura color overrides for inline `TapsForMana` trigger resolution.
    /// Each entry maps an aura's `ObjectId` to the `ProductionOverride` the
    /// auto-tap resolver should use when that aura's triggered mana ability fires.
    /// Empty for options that carry no aura bonus.
    pub taps_for_mana_overrides: Vec<(ObjectId, ProductionOverride)>,
}

/// Check whether an ability cost includes a tap component.
/// Matches both `AbilityCost::Tap` and `Composite` costs containing `Tap`.
/// True when `cost` contains a component satisfying `pred`, checking both a bare
/// cost and every component of a `Composite`. Single walker behind all
/// component-presence predicates (`has_tap_component`, `has_untap_component`,
/// `cost_includes_sacrifice`, `cost_includes_loyalty`).
pub(crate) fn cost_has_component(
    cost: &Option<AbilityCost>,
    pred: impl Fn(&AbilityCost) -> bool,
) -> bool {
    match cost {
        Some(AbilityCost::Composite { costs }) => costs.iter().any(&pred),
        Some(c) => pred(c),
        None => false,
    }
}

pub(crate) fn has_tap_component(cost: &Option<AbilityCost>) -> bool {
    cost_has_component(cost, |c| matches!(c, AbilityCost::Tap))
}

/// CR 107.6 + CR 302.6: True when the cost includes the untap symbol ({Q}).
/// Like {T}, a {Q} cost on a creature is gated by summoning sickness (CR 302.6
/// names both symbols) and requires the source to currently be tapped. Matches a
/// bare `Untap` cost and one nested inside a `Composite` (Pili-Pala: `{2}, {Q}`).
pub(crate) fn has_untap_component(cost: &Option<AbilityCost>) -> bool {
    cost_has_component(cost, |c| matches!(c, AbilityCost::Untap))
}

/// CR 605.3a + CR 106.12 + CR 107.6: True when paying this mana-ability cost is
/// conclusively decided by the non-simulating cheap gate, so
/// `can_activate_mana_ability_now` may skip the full-state legality clone.
///
/// Sound only for an infallible production+payment path: no cost (`None`), or a
/// cost whose every component is the tap/untap symbol. The
/// `has_tap_component || has_untap_component` anchor documents that the cheap
/// gate's {T}/{Q} state checks make the skip safe, and guards the degenerate
/// empty `Composite { costs: [] }`, whose `all()` would otherwise be vacuously
/// true with no real {T}/{Q} to gate on.
pub(crate) fn cost_conclusively_payable_by_cheap_gate(cost: &Option<AbilityCost>) -> bool {
    match cost {
        None => true,
        Some(inner) => {
            (has_tap_component(cost) || has_untap_component(cost))
                && inner.all_components_cheap_gate_covered()
        }
    }
}

/// CR 701.21: True when paying this ability's cost sacrifices a permanent
/// (the source itself or another). Matches a bare `Sacrifice` cost and a
/// `Sacrifice` nested inside a `Composite`, for any target filter.
fn cost_includes_sacrifice(cost: &Option<AbilityCost>) -> bool {
    cost_has_component(cost, |c| matches!(c, AbilityCost::Sacrifice(_)))
}

/// Fold an `Option<u16>` sum with saturation and non-fixed poisoning.
/// Threading this as a small private helper keeps the recursive walker and
/// the composite-cost loop in sync: any `None` contribution collapses the
/// accumulator to `None` forever, matching "unknown amount poisons the sum."
fn fold_amount(acc: Option<u16>, rhs: Option<u16>) -> Option<u16> {
    match (acc, rhs) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        _ => None,
    }
}

/// Collapse a `QuantityExpr` to a fixed `u16` amount if possible. Any
/// non-`Fixed` shape (Ref / DivideRounded / Offset / Multiply / …) returns
/// `None` — the caller treats that as "unknown amount, poison the sum."
fn quantity_expr_to_fixed_amount(expr: &QuantityExpr) -> Option<u16> {
    match expr {
        QuantityExpr::Fixed { value } => Some((*value).clamp(0, u16::MAX as i32) as u16),
        _ => None,
    }
}

/// Returns `Some(amount)` iff the cost contains a `PayLife` component.
/// The inner `Option<u16>` is `Some(n)` when the cost's `QuantityExpr`
/// collapses to a fixed value and `None` when it is dynamic. Returns
/// `None` (outer) when no `PayLife` component exists anywhere in the cost.
///
/// If a (hypothetical) `Composite` cost contains multiple `PayLife`
/// components, fixed amounts are summed; any dynamic component poisons
/// the total to `None` — mirroring the chain walker's semantics.
fn cost_life_payment_amount(cost: &Option<AbilityCost>) -> Option<Option<u16>> {
    match cost {
        Some(AbilityCost::PayLife { amount }) => Some(quantity_expr_to_fixed_amount(amount)),
        Some(AbilityCost::Composite { costs }) => {
            let mut found = false;
            let mut total: Option<u16> = Some(0);
            for c in costs {
                if let AbilityCost::PayLife { amount } = c {
                    found = true;
                    total = fold_amount(total, quantity_expr_to_fixed_amount(amount));
                }
            }
            found.then_some(total)
        }
        _ => None,
    }
}

/// Contribution of a single `Effect` to the controller-harm amount.
/// `None` = not a controller-harming effect; `Some(None)` = harming but
/// amount is dynamic (poisons the caller's sum); `Some(Some(n))` = fixed
/// amount `n`.
///
/// CR 605.3b: A mana ability's continuation effects resolve inline as part
/// of the same resolution. A `DealDamage`/`LoseLife` effect scoped to
/// `Controller` therefore hits the activator atomically.
fn effect_controller_harm_amount(effect: &Effect) -> Option<Option<u16>> {
    match effect {
        Effect::DealDamage {
            target: TargetFilter::Controller,
            amount,
            ..
        } => Some(quantity_expr_to_fixed_amount(amount)),
        Effect::LoseLife {
            target: None | Some(TargetFilter::Controller),
            amount,
        } => Some(quantity_expr_to_fixed_amount(amount)),
        _ => None,
    }
}

/// Walk a mana ability's continuation graph summing controller-harm amounts.
///
/// Returns `Some(Some(n))` when one or more controller-harming effects are
/// found and all their amounts are fixed (summed into `n`); `Some(None)`
/// when one is found but any amount is dynamic; `None` when no harming
/// effect exists anywhere in the chain.
///
/// Note: currently only visits `sub_ability` and `else_ability` branches.
/// Modal / `repeat_for` branches are out of scope for this helper — if the
/// parser ever emits them as children of a mana ability, this function must
/// be extended.
fn chain_harms_controller_amount(ability: &AbilityDefinition) -> Option<Option<u16>> {
    fn walk(ability: &AbilityDefinition, acc: &mut (bool, Option<u16>)) {
        if let Some(contribution) = effect_controller_harm_amount(&ability.effect) {
            acc.0 = true;
            acc.1 = fold_amount(acc.1, contribution);
        }
        if let Some(sub) = ability.sub_ability.as_deref() {
            walk(sub, acc);
        }
        if let Some(other) = ability.else_ability.as_deref() {
            walk(other, acc);
        }
    }
    let mut acc = (false, Some(0_u16));
    walk(ability, &mut acc);
    acc.0.then_some(acc.1)
}

/// CR 605.3b: Returns `true` iff the continuation chain (sub_ability /
/// else_ability) contains any effect other than `Effect::Mana`.
///
/// Top-level `ability.effect` is the mana production itself and is never
/// inspected here — only the chain links. A non-mana effect anywhere in the
/// transitive sub-graph means the activation commits irreversible state and
/// should not be marked undoable.
fn chain_has_non_mana_effect(ability: &AbilityDefinition) -> bool {
    fn walk(ability: &AbilityDefinition) -> bool {
        if !matches!(*ability.effect, Effect::Mana { .. }) {
            return true;
        }
        if let Some(sub) = ability.sub_ability.as_deref() {
            if walk(sub) {
                return true;
            }
        }
        if let Some(other) = ability.else_ability.as_deref() {
            if walk(other) {
                return true;
            }
        }
        false
    }
    if let Some(sub) = ability.sub_ability.as_deref() {
        if walk(sub) {
            return true;
        }
    }
    if let Some(other) = ability.else_ability.as_deref() {
        if walk(other) {
            return true;
        }
    }
    false
}

/// CR 605.3b: Single classification authority for a mana ability's penalty axis.
///
/// Precedence, when multiple categories apply:
///   Sacrifice > PayLife > Damage/LoseLife > HasIrreversibleContinuation > None
/// This matches the auto-tap tier order: an ability that both sacrifices
/// itself and pays life (no known printed card, but hypothetically allowed)
/// is classified by the worst category that applies. The
/// `HasIrreversibleContinuation` arm is the conservative catch-all for chain
/// shapes whose semantics we don't otherwise classify (depletion-counter
/// lands, self-haste lands, `Effect::Unimplemented` tails, etc.) — assumed
/// to commit irreversible state so they aren't offered as undoable.
pub(crate) fn mana_ability_penalty(ability: &AbilityDefinition) -> ManaSourcePenalty {
    if cost_includes_sacrifice(&ability.cost) {
        return ManaSourcePenalty::Sacrifices;
    }
    if let Some(fixed_amount) = cost_life_payment_amount(&ability.cost) {
        return ManaSourcePenalty::PaysLifeOnActivation { fixed_amount };
    }
    if let Some(fixed_amount) = chain_harms_controller_amount(ability) {
        return ManaSourcePenalty::DealsDamageOnResolution { fixed_amount };
    }
    if chain_has_non_mana_effect(ability) {
        return ManaSourcePenalty::HasIrreversibleContinuation;
    }
    ManaSourcePenalty::None
}

/// CR 603.2e + CR 701.26: Controller-harm amount from a self-referential
/// `TriggerMode::Taps` sibling trigger on `object_id` — City of Brass prints
/// its self-damage as a *separate* "Whenever this land becomes tapped, it
/// deals 1 damage to you" triggered ability (fires on ANY tap, not only a mana
/// activation), rather than folding the damage into the mana ability's own
/// resolution chain the way painlands do (Adarkar Wastes: "{T}: Add {W} or
/// {U}. This land deals 1 damage to you." is ONE ability). Tarnished Citadel
/// likewise has a mana penalty, but its colored mana ability embeds the damage
/// in its own chain. `mana_ability_penalty` only walks a mana ability's own
/// chain, so it never sees City of Brass's sibling trigger.
///
/// Returns `Some(amount)` when at least one such trigger is found (summed the
/// same way `chain_harms_controller_amount` sums a single chain), `None` when
/// the object carries no self-referential harmful `Taps` trigger.
fn object_self_tap_harm_amount(state: &GameState, object_id: ObjectId) -> Option<Option<u16>> {
    let obj = state.objects.get(&object_id)?;
    let source_context = trigger_source_context_for_latch(state, obj);
    let mut harm_amounts = obj
        .trigger_definitions
        .iter_all()
        .map(|entry| entry.definition())
        .filter(|trigger| trigger.mode == TriggerMode::Taps)
        .filter(|trigger| {
            trigger.valid_card.as_ref().is_none_or(|filter| {
                super::trigger_matchers::target_filter_matches_object(
                    state,
                    object_id,
                    filter,
                    &source_context,
                )
            })
        })
        .filter_map(|trigger| trigger.execute.as_deref())
        .filter_map(chain_harms_controller_amount)
        .peekable();
    harm_amounts.peek()?;
    Some(harm_amounts.fold(Some(0_u16), fold_amount))
}

/// CR 605.3b: Single classification authority for a mana-source OBJECT's full
/// penalty axis — [`mana_ability_penalty`]'s ability-chain classification
/// merged with any self-referential `Taps` sibling trigger that harms the
/// controller ([`object_self_tap_harm_amount`]). This is the version every
/// real consumer (auto-tap sort, the priority-meaningful gate,
/// `UntapLandForMana` undoability) must call instead of `mana_ability_penalty`
/// directly, so a City-of-Brass-shaped card can never be misclassified as
/// `None` just because its damage lives on a sibling trigger instead of the
/// mana ability's own chain.
///
/// A `Sacrifices`/`PaysLifeOnActivation` classification from the ability's own
/// chain is already worse than or equal to anything a sibling `Taps` trigger
/// could add, so those short-circuit unchanged. Otherwise, a sibling harm
/// amount folds into (or replaces) `DealsDamageOnResolution`.
pub(crate) fn object_mana_ability_penalty(
    state: &GameState,
    object_id: ObjectId,
    ability: &AbilityDefinition,
) -> ManaSourcePenalty {
    let own = mana_ability_penalty(ability);
    if !has_tap_component(&ability.cost) {
        return own;
    }
    if matches!(
        own,
        ManaSourcePenalty::Sacrifices | ManaSourcePenalty::PaysLifeOnActivation { .. }
    ) {
        return own;
    }
    match object_self_tap_harm_amount(state, object_id) {
        Some(sibling_amount) => ManaSourcePenalty::DealsDamageOnResolution {
            fixed_amount: match own {
                ManaSourcePenalty::DealsDamageOnResolution { fixed_amount } => {
                    fold_amount(fixed_amount, sibling_amount)
                }
                _ => sibling_amount,
            },
        },
        None => own,
    }
}

/// CR 119.3 (life) / CR 120.3 (damage → life loss): The benefit twin of
/// [`effect_controller_harm_amount`]. Returns `true` iff a single `Effect`
/// favors the player who caused the trigger (the tapper): opponent-scoped
/// damage / life loss, or controller-scoped life gain. Presence-only (no amount)
/// — a mana-tap hold is a hold-on-presence decision, not a value sum.
///
/// Exact AST shapes (verified against `types/ability.rs`): opponent scope is
/// `TargetFilter::Typed(TypedFilter { controller: Some(ControllerRef::Opponent),
/// .. })`; the bare `TargetFilter::Opponent` variant identifies an announcing
/// player for opponent-choice target slots and is intentionally not a damage
/// scope. `DamageEachPlayer { player_filter: Opponent }` is Zhur-Taa Druid's live
/// shape. `GainLife.player` is a `TargetFilter` that serde-defaults to
/// `Controller` (never `None` in the AST). Everything else — self /
/// triggering-player scope, `Unimplemented`, `GenericEffect`, and every unmodeled
/// shape — falls through the documented default arm to `false` (default-pass).
/// Broadening beyond CR 119 / CR 120 is a deliberate follow-up.
fn effect_benefits_trigger_controller(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::DamageEachPlayer {
            player_filter: PlayerFilter::Opponent,
            ..
        } | Effect::DealDamage {
            target: TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }),
            ..
        } | Effect::LoseLife {
            target: Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            })),
            ..
        } | Effect::GainLife {
            player: TargetFilter::Controller,
            ..
        }
    )
}

/// CR 603.2: Walk a trigger's `execute` continuation graph (top effect plus the
/// `sub_ability` / `else_ability` links, mirroring `chain_harms_controller_amount`)
/// and return `true` iff any link's effect benefits the trigger's controller. A
/// trigger with no `execute` chain benefits no one.
pub(crate) fn trigger_chain_benefits_controller(trigger: &TriggerDefinition) -> bool {
    fn walk(ability: &AbilityDefinition) -> bool {
        if effect_benefits_trigger_controller(&ability.effect) {
            return true;
        }
        if let Some(sub) = ability.sub_ability.as_deref() {
            if walk(sub) {
                return true;
            }
        }
        if let Some(other) = ability.else_ability.as_deref() {
            if walk(other) {
                return true;
            }
        }
        false
    }
    trigger.execute.as_deref().is_some_and(walk)
}

/// CR 605.1b (+ CR 603.3): True when `trigger` is a *non-mana* tap-triggered
/// ability — mode `TapsForMana` or `ManaAdded` whose `execute` chain contains
/// any effect other than mana production.
///
/// Such a trigger FAILS CR 605.1b's mana-ability criteria (it does not "add mana
/// to a player's mana pool when it resolves"), so it goes on the stack and uses
/// a priority window (CR 603.3) — unlike a pure-mana tap trigger, which is a
/// mana ability that resolves inline without the stack (CR 605.4a). That stack
/// use is exactly why a beneficial one is worth holding for. A whole-`Effect::Mana`
/// chain (the aura mana-fixing case: Utopia Sprawl, Fertile Ground) is excluded —
/// mirrors `chain_has_non_mana_effect`, but here the top-level `execute.effect`
/// is also inspected (it carries the trigger's own effect, not mana production).
pub(crate) fn is_non_mana_tap_trigger(trigger: &TriggerDefinition) -> bool {
    matches!(
        trigger.mode,
        TriggerMode::TapsForMana | TriggerMode::ManaAdded
    ) && trigger.execute.as_deref().is_some_and(|execute| {
        !matches!(*execute.effect, Effect::Mana { .. }) || chain_has_non_mana_effect(execute)
    })
}

/// CR 605.1b: Battlefield permanents controlled by `player` that carry a
/// *beneficial* non-mana tap trigger — one that both [`is_non_mana_tap_trigger`]
/// (uses the stack) and [`trigger_chain_benefits_controller`] (favors the
/// tapper). This is the clone-free stage-1 AST gate for the G1 beneficial
/// mana-tap hold: a linear battlefield walk with early-out, no state clones.
pub(crate) fn beneficial_non_mana_tap_trigger_sources(
    state: &GameState,
    player: PlayerId,
) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|object_id| {
            state.objects.get(object_id).is_some_and(|obj| {
                obj.controller == player
                    && obj.trigger_definitions.iter_all().any(|entry| {
                        let trigger = entry.definition();
                        is_non_mana_tap_trigger(trigger)
                            && trigger_chain_benefits_controller(trigger)
                    })
            })
        })
        .collect()
}

/// Return all currently activatable tap-mana options for a land.
///
/// This is used by legal action generation and auto-pay. It evaluates supported
/// activation restrictions (currently land-subtype control clauses) and returns
/// one or more candidate colors for the source.
pub fn activatable_land_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Vec<ManaSourceOption> {
    land_mana_options(state, object_id, controller, true, true, None, None)
}

pub(crate) fn activatable_land_mana_options_indexed_gated(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    aura_sources: &[ObjectId],
    gates: &mana_abilities::ManaActivationGates,
) -> Vec<ManaSourceOption> {
    land_mana_options(
        state,
        object_id,
        controller,
        true,
        true,
        Some(aura_sources),
        Some(gates),
    )
}

/// Auto-tap land mana options for the board-global cost sweep. The sweep
/// precomputes the TapsForMana trigger-source list once
/// (`taps_for_mana_trigger_sources`) and threads it through each land, avoiding
/// a per-land full-battlefield scan. Byte-identical to a per-land form.
pub(crate) fn auto_tap_land_mana_options_indexed(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    aura_sources: &[ObjectId],
) -> Vec<ManaSourceOption> {
    land_mana_options(
        state,
        object_id,
        controller,
        true,
        false,
        Some(aura_sources),
        None,
    )
}

/// Return display pips for a land based on mana abilities that are currently
/// available under game-state conditions.
///
/// Unlike `activatable_land_mana_options`, this ignores tapped state so frame
/// pips remain stable while permanents are tapped. Each pip mirrors a
/// `ManaProduction` variant so colorless and commander-identity producers
/// reach the frontend with full fidelity (a previous `Vec<ManaColor>` shape
/// silently dropped both classes).
pub fn display_land_mana_pips(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Vec<ManaPip> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller {
        return Vec::new();
    }
    if !obj.card_types.core_types.contains(&CoreType::Land) {
        return Vec::new();
    }

    let mut pips: Vec<ManaPip> = Vec::new();
    let push = |pips: &mut Vec<ManaPip>, pip: ManaPip| {
        if !pips.contains(&pip) {
            pips.push(pip);
        }
    };

    let mut had_explicit_mana_ability = false;
    for (ability_index, ability) in obj.abilities.iter().enumerate() {
        if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
            continue;
        }
        if !has_tap_component(&ability.cost) {
            continue;
        }
        if !activation_condition_satisfied(state, controller, object_id, ability_index, ability) {
            continue;
        }
        let Effect::Mana { produced, .. } = &*ability.effect else {
            continue;
        };
        had_explicit_mana_ability = true;
        // CR 106.1 / CR 106.4 / CR 903.4: Exhaustively project every
        // ManaProduction variant into typed display pips. No wildcard arm —
        // future variants force this match to be updated.
        match produced {
            // CR 106.1a: Each named color becomes its own pip.
            ManaProduction::Fixed { colors, .. } => {
                for color in colors {
                    push(&mut pips, ManaPip::Color(*color));
                }
            }
            // CR 106.1b: Pure colorless producer (e.g., War Room, Wastes).
            ManaProduction::Colorless { .. } => {
                push(&mut pips, ManaPip::Colorless);
            }
            // CR 106.1: Mixed colorless + colored (Ravnica bounce lands).
            ManaProduction::Mixed {
                colorless_count,
                colors,
            } => {
                if *colorless_count > 0 {
                    push(&mut pips, ManaPip::Colorless);
                }
                for color in colors {
                    push(&mut pips, ManaPip::Color(*color));
                }
            }
            // CR 106.4: One color picked from the listed set per activation.
            ManaProduction::AnyOneColor { color_options, .. } => {
                push(&mut pips, ManaPip::OneOfColors(color_options.clone()));
            }
            // CR 106.4: Each unit independently chosen across the listed set.
            ManaProduction::AnyCombination { color_options, .. } => {
                push(
                    &mut pips,
                    ManaPip::CombinationOfColors(color_options.clone()),
                );
            }
            // CR 106.1a: Display the chosen color when available. If the
            // ability also has a fixed alternative ("{G} or one mana of the
            // chosen color"), show both available mana choices.
            ManaProduction::ChosenColor {
                fixed_alternative, ..
            } => match (fixed_alternative, obj.chosen_color()) {
                (None, None) => {
                    push(&mut pips, ManaPip::OneOfColors(ManaColor::ALL.to_vec()));
                }
                _ => {
                    for mana_type in
                        chosen_color_mana_type_options(state, object_id, *fixed_alternative)
                    {
                        if let Some(color) = mana_type_to_color(mana_type) {
                            push(&mut pips, ManaPip::Color(color));
                        }
                    }
                }
            },
            // CR 106.7: Dynamically computed from opponent lands.
            ManaProduction::OpponentLandColors { .. } => {
                let colors: Vec<ManaColor> = opponent_land_color_options(state, controller)
                    .into_iter()
                    .filter_map(mana_type_to_color)
                    .collect();
                if !colors.is_empty() {
                    push(&mut pips, ManaPip::OneOfColors(colors));
                }
            }
            // CR 106.7 + CR 106.1b: Reflecting Pool class — surface the full
            // type union (including Colorless) as a per-unit choice. Each
            // colored type emits a `Color` pip; a Colorless contribution emits
            // a separate `Colorless` pip so the frame faithfully shows the
            // full option set.
            ManaProduction::AnyTypeProduceableBy { land_filter, .. } => {
                let types =
                    produceable_mana_types_by_filter(state, land_filter, controller, object_id);
                let colors: Vec<ManaColor> = types
                    .iter()
                    .copied()
                    .filter_map(mana_type_to_color)
                    .collect();
                if !colors.is_empty() {
                    push(&mut pips, ManaPip::OneOfColors(colors));
                }
                if types.contains(&ManaType::Colorless) {
                    push(&mut pips, ManaPip::Colorless);
                }
            }
            // CR 605.1a + CR 406.1: Colors of cards exiled-with this source.
            ManaProduction::ChoiceAmongExiledColors { source } => {
                let colors = super::effects::mana::exiled_color_options(state, *source, object_id);
                let colors: Vec<ManaColor> =
                    colors.into_iter().filter_map(mana_type_to_color).collect();
                if !colors.is_empty() {
                    push(&mut pips, ManaPip::OneOfColors(colors));
                }
            }
            // CR 605.3b + CR 106.1a: Filter lands — emit each combination's
            // colors as the union of its component colors.
            ManaProduction::ChoiceAmongCombinations { options } => {
                let mut union: Vec<ManaColor> = Vec::new();
                for combo in options {
                    for color in combo {
                        if !union.contains(color) {
                            union.push(*color);
                        }
                    }
                }
                if !union.is_empty() {
                    push(&mut pips, ManaPip::OneOfColors(union));
                }
            }
            // CR 903.4 + CR 903.4f: Defer pip color resolution to the
            // frontend, which reads `commander_color_identity` off the
            // controller. Encoding as a typed variant preserves the
            // "commander identity" semantic across the wire.
            ManaProduction::AnyInCommandersColorIdentity { .. } => {
                push(&mut pips, ManaPip::AnyInCommandersIdentity);
            }
            // CR 106.1 + CR 109.1: Faeburrow-style "one of each color among
            // permanents you control".
            ManaProduction::DistinctColorsAmongPermanents { filter } => {
                let colors = super::effects::mana::distinct_colors_among_permanents(
                    state, None, object_id, filter,
                );
                if !colors.is_empty() {
                    push(&mut pips, ManaPip::CombinationOfColors(colors));
                }
            }
            // CR 106.1: Determine distinct colors among matching permanents to display
            // pips.
            ManaProduction::AnyOneColorAmongPermanents { filter, .. } => {
                let colors = super::effects::mana::distinct_colors_among_permanents(
                    state, None, object_id, filter,
                );
                if !colors.is_empty() {
                    push(&mut pips, ManaPip::OneOfColors(colors));
                }
            }
            // CR 106.1 + CR 202.2c: Omnath, Locus of All — colors are read from a
            // target object resolved at trigger-resolution time, not from this
            // permanent's frame, so there is no static pip to display here.
            ManaProduction::AnyCombinationOfObjectColors { .. } => {}
            // CR 603.7c + CR 106.3: Resolves only inside a TapsForMana
            // trigger; outside a trigger context there is no pre-resolution
            // pip to display, so contribute nothing.
            ManaProduction::TriggerEventManaType => {}
        }
    }

    // Legacy fallback for basic-land subtype-only objects with no explicit
    // mana ability (matches the basic-subtype fallback in `land_mana_options`).
    if !had_explicit_mana_ability {
        if let Some(mana_type) = obj
            .card_types
            .subtypes
            .iter()
            .find_map(|s| mana_payment::land_subtype_to_mana_type(s))
        {
            match mana_type_to_color(mana_type) {
                Some(color) => push(&mut pips, ManaPip::Color(color)),
                None => push(&mut pips, ManaPip::Colorless),
            }
        }
    }

    // CR 605.1b + CR 106.12a: include pips from TapsForMana-triggered auras
    // (Wild Growth, Fertile Ground, Utopia Sprawl, etc.) so the land frame
    // reflects its full tapped output.
    //
    // Each aura's choices drive the pip kind:
    // - Fixed (Wild Growth: {G}): one concrete pip, added without dedup so
    //   the frame shows two {G} symbols for Forest + Wild Growth.
    // - AnyOneColor (Fertile Ground: any color): a OneOfColors pip via the
    //   deduplicating `push` helper — same semantics as a City of Brass pip.
    for (_aura_id, aura_choices) in taps_for_mana_aura_bonus(state, object_id, controller) {
        if aura_choices.len() == 1 {
            // Fixed bonus: add a concrete pip for each mana type produced.
            for &mana_type in &aura_choices {
                match mana_type_to_color(mana_type) {
                    Some(color) => pips.push(ManaPip::Color(color)),
                    None => pips.push(ManaPip::Colorless),
                }
            }
        } else {
            // Choice bonus: emit OneOfColors (deduped) so the frame shows one
            // multi-color symbol rather than separate per-color pips.
            let colors: Vec<ManaColor> = aura_choices
                .iter()
                .filter_map(|&mt| mana_type_to_color(mt))
                .collect();
            if !colors.is_empty() {
                push(&mut pips, ManaPip::OneOfColors(colors));
            }
        }
    }

    pips
}

/// CR 605.1b: Return activatable tap-mana options for ANY untapped permanent.
/// CR 302.6: Creatures with summoning sickness cannot activate tap abilities.
///
/// Used by auto-pay affordability checks and AI candidate generation to include
/// non-land mana sources (mana dorks, Treasure tokens, mana artifacts).
pub fn activatable_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Vec<ManaSourceOption> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller || obj.tapped {
        return Vec::new();
    }
    // CR 602.5a + CR 302.6: Creatures with summoning sickness cannot activate tap abilities,
    // unless a CanActivateAbilitiesAsThoughHaste static (Tyvar) lifts the gate.
    if restrictions::summoning_sick_for_tap_ability(state, obj) {
        return Vec::new();
    }
    scan_mana_abilities(state, obj, object_id, controller, true, None)
}

pub(crate) fn auto_tap_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Vec<ManaSourceOption> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller || obj.tapped {
        return Vec::new();
    }
    if restrictions::summoning_sick_for_tap_ability(state, obj) {
        return Vec::new();
    }
    scan_mana_abilities(state, obj, object_id, controller, false, None)
}

/// CR 107.1b + CR 601.2f: Maximum *net* mana a single battlefield object can
/// contribute to a cast — the largest net output of any one of its activatable
/// `{T}` mana abilities (only one can be activated per tap), where net output
/// is gross production minus the mana paid to activate.
///
/// Lets `max_x_value` count multi-mana producers (Sol Ring, Ravnica bounce
/// lands, `{T}: Add {C} for each ~`) at their full output instead of a flat
/// one-mana-per-producer, which capped the X chooser below what the caster
/// could actually pay. Netting the activation cost keeps cost-bearing sources
/// (filter lands' `{1}, {T}: Add two mana`) from overstating affordable X.
pub fn max_mana_yield(state: &GameState, object_id: ObjectId, controller: PlayerId) -> u32 {
    let Some(obj) = state.objects.get(&object_id) else {
        return 0;
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller || obj.tapped {
        return 0;
    }
    // CR 602.5a + CR 302.6: Summoning-sick mana-creatures cannot tap for mana,
    // unless a CanActivateAbilitiesAsThoughHaste static (Tyvar) lifts the gate.
    if restrictions::summoning_sick_for_tap_ability(state, obj) {
        return 0;
    }

    let explicit_max = obj
        .abilities
        .iter()
        .enumerate()
        .filter(|(idx, ability)| {
            is_active_tap_mana_ability(state, object_id, controller, *idx, ability, true, None)
        })
        .filter_map(|(_, ability)| match &*ability.effect {
            Effect::Mana { produced, .. } => {
                let resolved =
                    super::ability_utils::build_resolved_from_def(ability, object_id, controller);
                let gross = super::effects::mana::resolve_mana_types_for_ability(
                    produced, state, &resolved,
                )
                .len() as u32;
                // CR 605.3b: Net the mana paid to activate this ability —
                // gross output overstates what a filter land actually adds.
                let activation_cost = mana_abilities::mana_sub_cost_of(&ability.cost)
                    .map_or(0, |cost| cost.mana_value());
                Some(gross.saturating_sub(activation_cost))
            }
            _ => None,
        })
        .max();

    // CR 605.1b + CR 106.12a: add aura TapsForMana bonus to the land's yield
    // so X-value choosers and castability gates account for Wild Growth etc.
    // Each outer element of `taps_for_mana_aura_bonus` is one aura that adds
    // exactly one mana unit; the inner vec holds the color alternatives (1 for
    // Fixed, N for AnyOneColor) — only the count of auras matters here.
    let aura_bonus = if obj.card_types.core_types.contains(&CoreType::Land) {
        taps_for_mana_aura_bonus(state, object_id, controller).len() as u32
    } else {
        0
    };

    match explicit_max {
        Some(amount) => amount + aura_bonus,
        // CR 305.1: Subtype-only basic lands carry no explicit mana ability;
        // `land_mana_options` synthesizes a single one-mana option for them.
        None if !activatable_mana_options(state, object_id, controller).is_empty() => {
            1 + aura_bonus
        }
        None => aura_bonus,
    }
}

/// CR 117.1d + CR 601.2g: Maximum net mana this permanent could contribute via
/// **any** mana ability the controller could currently activate, including
/// non-tap-cost mana abilities (Sacrifice — KCI, Phyrexian Altar, Ashnod's
/// Altar; Discard — Lion's Eye Diamond; Pay Life; etc.).
///
/// Unlike [`max_mana_yield`], this is NOT restricted to abilities that include
/// `{T}` in their cost. It exists so the castability gate
/// ([`super::casting::can_feasibly_pay_mana_cost`]) and X-spell maximum
/// ([`super::casting_costs::max_x_value`]) can answer the question
/// "could the player feasibly pay this cost by activating mana abilities
/// **manually** during cost payment?" — not "could auto-tap alone cover this?".
///
/// This is a read-only feasibility scan. Auto-payment must continue to use
/// [`max_mana_yield`] / [`is_active_tap_mana_ability`] because the auto-tap
/// simulator can't auto-sacrifice or auto-discard.
//
// CR 117.1d: A player may activate a mana ability whenever a rule or effect
// asks for a mana payment (including during the spell's cost-payment step).
// CR 601.2g: After total cost is determined, the player has a chance to
// activate mana abilities before paying. Affordability must reflect what the
// player COULD pay manually, not only what the engine could auto-tap.
fn mana_ability_allowed_for_payment(
    restrictions: &[ManaSpendRestriction],
    state: &GameState,
    object_id: ObjectId,
    payment_context: Option<&PaymentContext<'_>>,
) -> bool {
    let Some(ctx) = payment_context else {
        return true;
    };
    super::effects::mana::resolve_restrictions(restrictions, state, object_id)
        .iter()
        .all(|restriction| restriction.allows(ctx))
}

pub(crate) fn feasible_mana_capacity(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    payment_context: Option<&PaymentContext<'_>>,
) -> u32 {
    let Some(obj) = state.objects.get(&object_id) else {
        return 0;
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller {
        return 0;
    }

    let explicit_max = obj
        .abilities
        .iter()
        .enumerate()
        .filter(|(idx, ability)| {
            // CR 605.1a: Must be an activated mana ability.
            if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
                return false;
            }
            // CR 605.3a + CR 117.1d: The engine-authoritative "is the cost
            // currently payable?" check covers sacrifice supply
            // (`find_eligible_sacrifice_targets`), discard-hand availability,
            // life total, tap eligibility, and summoning sickness via the
            // shared `is_payable_for_mana_ability` + activation simulation.
            // We delegate to it rather than re-implementing the cost gate.
            if !mana_abilities::can_activate_mana_ability_now(
                state, controller, object_id, *idx, ability,
            ) {
                return false;
            }
            // CR 604: Static activation restrictions ("only during your
            // upkeep", etc.) must hold — mirrors `is_active_tap_mana_ability`.
            if !activation_condition_satisfied(state, controller, object_id, *idx, ability) {
                return false;
            }
            // CR 106.6: Restricted mana only counts toward this spell when
            // the restriction permits it (issue #2011: Eldrazi Temple).
            match &*ability.effect {
                Effect::Mana { restrictions, .. } => mana_ability_allowed_for_payment(
                    restrictions,
                    state,
                    object_id,
                    payment_context,
                ),
                _ => false,
            }
        })
        .filter_map(|(_, ability)| match &*ability.effect {
            Effect::Mana { produced, .. } => {
                let resolved =
                    super::ability_utils::build_resolved_from_def(ability, object_id, controller);
                let gross = super::effects::mana::resolve_mana_types_for_ability(
                    produced, state, &resolved,
                )
                .len() as u32;
                // CR 605.3b: Net the mana paid to activate. Non-mana cost
                // components (Sacrifice / Discard / PayLife / Exile) have no
                // mana sub-cost, so `mana_sub_cost_of` returns `None` and the
                // net equals the gross (e.g., KCI activation cost is one
                // sacrificed artifact, no mana — full 2 colorless yield).
                //
                // Caveat: collapsing mana sub-cost to a single `mana_value`
                // assumes the sub-cost is paid from the SAME color domain as
                // the produced mana (filter-land case: pay {1} or any color,
                // produce {U/W}). For exotic abilities whose mana sub-cost is
                // a different color than what they produce — e.g. a
                // hypothetical `{T}, Pay {U}: Add {2}{R}` — this net of 2 is
                // pessimistic-but-safe in one direction (we never claim
                // capacity we can't produce) but over-states feasibility in
                // the other (the {U} sub-cost has to come from another
                // source, which this scan doesn't model). Such cards are
                // vanishingly rare in real Magic; if one is added the
                // sub-cost color domain should be threaded through here
                // rather than collapsed to `mana_value`.
                let activation_cost = mana_abilities::mana_sub_cost_of(&ability.cost)
                    .map_or(0, |cost| cost.mana_value());
                Some(gross.saturating_sub(activation_cost))
            }
            _ => None,
        })
        .max();

    match explicit_max {
        Some(amount) => amount,
        // CR 305.1: Subtype-only basic-land fallback (same as `max_mana_yield`).
        None if obj.card_types.core_types.contains(&CoreType::Land)
            && !activatable_mana_options(state, object_id, controller).is_empty() =>
        {
            1
        }
        None => 0,
    }
}

/// CR 117.1d + CR 601.2g: True when cost payment can involve a currently
/// activatable non-tap mana ability that auto-tap cannot choose for the player
/// (Treasure/Spawn/KCI-style sacrifice mana, Lion's Eye Diamond discard mana,
/// pay-life mana abilities, etc.).
pub(crate) fn has_activatable_non_tap_mana_ability_for_payment(
    state: &GameState,
    controller: PlayerId,
    exclude: Option<ObjectId>,
    payment_context: Option<&PaymentContext<'_>>,
) -> bool {
    state.battlefield.iter().any(|&object_id| {
        if Some(object_id) == exclude {
            return false;
        }
        let Some(obj) = state.objects.get(&object_id) else {
            return false;
        };
        if obj.zone != Zone::Battlefield || obj.controller != controller {
            return false;
        }
        obj.abilities.iter().enumerate().any(|(idx, ability)| {
            if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
                return false;
            }
            if has_tap_component(&ability.cost) {
                return false;
            }
            if !mana_abilities::can_activate_mana_ability_now(
                state, controller, object_id, idx, ability,
            ) {
                return false;
            }
            if !activation_condition_satisfied(state, controller, object_id, idx, ability) {
                return false;
            }
            match &*ability.effect {
                Effect::Mana { restrictions, .. } => mana_ability_allowed_for_payment(
                    restrictions,
                    state,
                    object_id,
                    payment_context,
                ),
                _ => false,
            }
        })
    })
}

/// CR 117.1d + CR 601.2g: One activation's producible mana shape for the
/// castability gate's colored-shard coverage check (issue #583 / #1234).
#[derive(Debug, Clone)]
enum ActivatableManaProfileKind {
    Exact(Vec<ManaType>),
    AnyOneColor { count: u32, options: Vec<ManaType> },
    AnyCombination { count: u32, options: Vec<ManaType> },
    CombinationChoices(Vec<Vec<ManaType>>),
}

#[derive(Debug, Clone)]
struct ActivatableManaProfile {
    object_id: ObjectId,
    kind: ActivatableManaProfileKind,
}

fn resolved_production_count(
    produced: &ManaProduction,
    state: &GameState,
    resolved: &crate::types::ability::ResolvedAbility,
) -> u32 {
    super::effects::mana::resolve_mana_types_for_ability(produced, state, resolved).len() as u32
}

fn profile_kind_from_production(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    produced: &ManaProduction,
    resolved: &crate::types::ability::ResolvedAbility,
) -> Option<ActivatableManaProfileKind> {
    match produced {
        ManaProduction::ChoiceAmongCombinations { options } => {
            Some(ActivatableManaProfileKind::CombinationChoices(
                options
                    .iter()
                    .map(|combo| combo.iter().map(mana_color_to_type).collect())
                    .collect(),
            ))
        }
        ManaProduction::AnyOneColor { color_options, .. } => {
            Some(ActivatableManaProfileKind::AnyOneColor {
                count: resolved_production_count(produced, state, resolved),
                options: color_options.iter().map(mana_color_to_type).collect(),
            })
        }
        ManaProduction::AnyCombination { color_options, .. } => {
            Some(ActivatableManaProfileKind::AnyCombination {
                count: resolved_production_count(produced, state, resolved),
                options: color_options.iter().map(mana_color_to_type).collect(),
            })
        }
        ManaProduction::ChosenColor {
            fixed_alternative, ..
        } => {
            let count = resolved_production_count(produced, state, resolved);
            let options = chosen_color_mana_type_options(state, object_id, *fixed_alternative);
            if options.is_empty() {
                return None;
            }
            if count <= 1 && options.len() == 1 {
                Some(ActivatableManaProfileKind::Exact(options))
            } else {
                Some(ActivatableManaProfileKind::AnyOneColor { count, options })
            }
        }
        ManaProduction::OpponentLandColors { .. }
        | ManaProduction::AnyTypeProduceableBy { .. }
        | ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::AnyInCommandersColorIdentity { .. }
        | ManaProduction::AnyOneColorAmongPermanents { .. } => {
            let options = mana_options_from_production(state, controller, object_id, produced);
            if options.is_empty() {
                return None;
            }
            Some(ActivatableManaProfileKind::AnyOneColor {
                count: resolved_production_count(produced, state, resolved),
                options,
            })
        }
        ManaProduction::DistinctColorsAmongPermanents { .. } => {
            let types =
                super::effects::mana::resolve_mana_types_for_ability(produced, state, resolved);
            if types.is_empty() {
                None
            } else {
                Some(ActivatableManaProfileKind::Exact(types))
            }
        }
        ManaProduction::TriggerEventManaType => None,
        // CR 106.1 + CR 202.2c: Omnath, Locus of All — a one-shot triggered mana
        // effect, never an activatable mana ability tapped during cost payment.
        // No target-bound object exists in this profile context, so it surfaces
        // no activatable profile (CR 106.5).
        ManaProduction::AnyCombinationOfObjectColors { .. } => None,
        _ => {
            let types =
                super::effects::mana::resolve_mana_types_for_ability(produced, state, resolved);
            if types.is_empty() {
                None
            } else {
                Some(ActivatableManaProfileKind::Exact(types))
            }
        }
    }
}

fn activatable_mana_profiles_for_object(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    payment_context: Option<&PaymentContext<'_>>,
) -> Vec<ActivatableManaProfile> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller {
        return Vec::new();
    }

    obj.abilities
        .iter()
        .enumerate()
        .filter_map(|(idx, ability)| {
            if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
                return None;
            }
            if !mana_abilities::can_activate_mana_ability_now(
                state, controller, object_id, idx, ability,
            ) {
                return None;
            }
            if !activation_condition_satisfied(state, controller, object_id, idx, ability) {
                return None;
            }
            let Effect::Mana {
                produced,
                restrictions,
                ..
            } = &*ability.effect
            else {
                return None;
            };
            if !mana_ability_allowed_for_payment(restrictions, state, object_id, payment_context) {
                return None;
            }
            let resolved =
                super::ability_utils::build_resolved_from_def(ability, object_id, controller);
            profile_kind_from_production(state, object_id, controller, produced, &resolved)
                .map(|kind| ActivatableManaProfile { object_id, kind })
        })
        .collect()
}

fn collect_activatable_mana_profiles(
    state: &GameState,
    player: PlayerId,
    exclude: Option<ObjectId>,
    payment_context: Option<&PaymentContext<'_>>,
) -> Vec<ActivatableManaProfile> {
    state
        .battlefield
        .iter()
        .filter(|id| Some(**id) != exclude)
        .flat_map(|&id| activatable_mana_profiles_for_object(state, id, player, payment_context))
        .collect()
}

fn shard_payment_options(shard: ManaCostShard) -> Option<Vec<ManaType>> {
    use super::mana_payment::{shard_to_mana_type, ShardRequirement};
    Some(match shard_to_mana_type(shard) {
        ShardRequirement::Single(mana_type) => vec![mana_type],
        ShardRequirement::Hybrid(a, b) => vec![a, b],
        ShardRequirement::TwoGenericHybrid(mana_type) => vec![mana_type, ManaType::Colorless],
        ShardRequirement::ColorlessHybrid(mana_type) => vec![ManaType::Colorless, mana_type],
        ShardRequirement::Phyrexian(mana_type) => vec![mana_type],
        ShardRequirement::HybridPhyrexian(a, b) => vec![a, b],
        ShardRequirement::TwoGenericHybridPhyrexian(mana_type) => {
            vec![mana_type, ManaType::Colorless]
        }
        ShardRequirement::Snow | ShardRequirement::TwoOrMoreColorSource | ShardRequirement::X => {
            return None;
        }
    })
}

fn group_profiles_by_object(
    profiles: Vec<ActivatableManaProfile>,
) -> Vec<(ObjectId, Vec<ActivatableManaProfileKind>)> {
    // Issue #4878: BTreeMap (not HashMap) so the grouped Vec below is
    // ObjectId-sorted and deterministic across processes, rather than
    // following per-process HashMap iteration order.
    use std::collections::BTreeMap;
    let mut grouped: BTreeMap<ObjectId, Vec<ActivatableManaProfileKind>> = BTreeMap::new();
    for profile in profiles {
        grouped
            .entry(profile.object_id)
            .or_default()
            .push(profile.kind);
    }
    grouped.into_iter().collect()
}

fn profile_applications(
    profile: &ActivatableManaProfileKind,
    requirements: &[Vec<ManaType>],
) -> Vec<(Vec<Vec<ManaType>>, u32)> {
    match profile {
        ActivatableManaProfileKind::Exact(types) => {
            let mut remaining = requirements.to_vec();
            for mana_type in types {
                let Some(pos) = remaining.iter().position(|opts| opts.contains(mana_type)) else {
                    return Vec::new();
                };
                remaining.remove(pos);
            }
            vec![(remaining, types.len() as u32)]
        }
        ActivatableManaProfileKind::AnyOneColor { count, options } => options
            .iter()
            .flat_map(|&color| {
                combination_assignments(*count, std::slice::from_ref(&color), requirements)
            })
            .collect(),
        ActivatableManaProfileKind::AnyCombination { count, options } => {
            combination_assignments(*count, options, requirements)
        }
        ActivatableManaProfileKind::CombinationChoices(choices) => choices
            .iter()
            .flat_map(|choice| {
                profile_applications(
                    &ActivatableManaProfileKind::Exact(choice.clone()),
                    requirements,
                )
            })
            .collect(),
    }
}

fn assign_profiles_to_requirements(
    objects: &[(ObjectId, Vec<ActivatableManaProfileKind>)],
    object_index: usize,
    requirements: Vec<Vec<ManaType>>,
) -> Option<u32> {
    if requirements.is_empty() {
        return Some(0);
    }
    if object_index >= objects.len() {
        return None;
    }
    if let Some(consumed) =
        assign_profiles_to_requirements(objects, object_index + 1, requirements.clone())
    {
        return Some(consumed);
    }
    for profile in &objects[object_index].1 {
        for (remaining, consumed) in profile_applications(profile, &requirements) {
            if let Some(rest) =
                assign_profiles_to_requirements(objects, object_index + 1, remaining)
            {
                return Some(consumed + rest);
            }
        }
    }
    None
}

/// CR 106.1 + CR 601.2g: Enumerate the colored shards one flexible source can
/// cover, allowing later sources to cover the remainder. A one-mana
/// `AnyOneColor` source must not be rejected simply because the spell has two
/// colored shards; Relic of Legends plus a dual land is the common case.
fn combination_assignments(
    count: u32,
    options: &[ManaType],
    requirements: &[Vec<ManaType>],
) -> Vec<(Vec<Vec<ManaType>>, u32)> {
    if requirements.is_empty() {
        // All shards are covered; any leftover `count` is simply surplus mana
        // the player never produces (or lets drain). Rejecting over-production
        // here would falsely mark e.g. a power-3 combination source as unable
        // to pay a two-shard cost.
        return vec![(Vec::new(), 0)];
    }
    if count == 0 {
        return vec![(requirements.to_vec(), 0)];
    }
    let mut applications = Vec::new();
    for (index, payment_options) in requirements.iter().enumerate() {
        for &color in payment_options {
            if !options.contains(&color) {
                continue;
            }
            let mut next_requirements = requirements.to_vec();
            next_requirements.remove(index);
            for (remaining, inner) in
                combination_assignments(count - 1, options, &next_requirements)
            {
                applications.push((remaining, 1 + inner));
            }
        }
    }
    applications
}

fn assign_profiles_to_shards(
    profiles: &[ActivatableManaProfile],
    shards: &[ManaCostShard],
) -> Option<u32> {
    let requirements: Vec<Vec<ManaType>> = shards
        .iter()
        .filter_map(|shard| shard_payment_options(*shard))
        .collect();
    if requirements.len() != shards.len() {
        return None;
    }
    let grouped = group_profiles_by_object(profiles.to_vec());
    assign_profiles_to_requirements(&grouped, 0, requirements)
}

/// CR 117.1d + CR 601.2g: Whether residual mana shards could be paid by
/// activating currently legal mana abilities (non-tap sources like Vivi
/// Ornitier's {0} combination mana, Lion's Eye Diamond, etc.).
///
/// Returns `(covered, consumed_pips)` where `consumed_pips` is the total mana
/// produced by activations used for shard coverage — callers must subtract
/// this from generic capacity to avoid double-counting one activation.
pub(crate) fn can_cover_shards_with_activatable_mana(
    state: &GameState,
    player: PlayerId,
    exclude: Option<ObjectId>,
    payment_context: Option<&PaymentContext<'_>>,
    shards: &[ManaCostShard],
) -> (bool, u32) {
    if shards.is_empty() {
        return (true, 0);
    }
    let profiles = collect_activatable_mana_profiles(state, player, exclude, payment_context);
    assign_profiles_to_shards(&profiles, shards)
        .map(|consumed| (true, consumed))
        .unwrap_or((false, 0))
}

fn land_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    require_untapped: bool,
    require_current_payability: bool,
    // Precomputed TapsForMana trigger-source list for the board-global sweeps;
    // `None` means compute it for this land (single-land / display / test
    // callers). Byte-identical either way — the indexed and full scans visit the
    // same trigger-bearing permanents.
    aura_sources: Option<&[ObjectId]>,
    gates: Option<&mana_abilities::ManaActivationGates>,
) -> Vec<ManaSourceOption> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Battlefield || obj.controller != controller {
        return Vec::new();
    }
    if !obj.card_types.core_types.contains(&CoreType::Land) {
        return Vec::new();
    }
    if require_untapped && obj.tapped {
        return Vec::new();
    }
    // CR 602.5a + CR 302.6: Land-creatures (e.g., Dryad Arbor) have summoning sickness and
    // cannot activate tap abilities the turn they enter the battlefield, unless a
    // CanActivateAbilitiesAsThoughHaste static (Tyvar) lifts the gate.
    if require_untapped && restrictions::summoning_sick_for_tap_ability(state, obj) {
        return Vec::new();
    }

    let mut options = scan_mana_abilities(
        state,
        obj,
        object_id,
        controller,
        require_current_payability,
        gates,
    );

    // Legacy fallback for basic-land subtype-only objects (no explicit mana ability).
    if options.is_empty() {
        if let Some(mana_type) = obj
            .card_types
            .subtypes
            .iter()
            .find_map(|s| mana_payment::land_subtype_to_mana_type(s))
        {
            options.push(ManaSourceOption {
                object_id,
                ability_index: None,
                mana_type,
                source_could_produce_two_or_more_colors: source_could_produce_two_or_more_colors(
                    state, object_id, controller,
                ),
                penalty: ManaSourcePenalty::None,
                atomic_combination: None,
                restrictions: Vec::new(),
                taps_for_mana_overrides: Vec::new(),
            });
        }
    }

    // CR 605.1b + CR 106.12a: fold in bonus mana from TapsForMana-triggered
    // auras (Wild Growth, Fertile Ground, Utopia Sprawl, Verdant Haven, etc.).
    // Each aura fires atomically with the land's tap — so we extend each
    // option's `atomic_combination` rather than adding a second option (which
    // would let the planner double-tap the same land).
    //
    // For Fixed auras (Wild Growth: {G}): one bonus type → no fan-out, same
    // number of options.
    // For AnyOneColor auras (Fertile Ground: any color): N bonus choices →
    // fan-out into N options per base option, one per reachable color, so the
    // planner can pick whichever color satisfies the pending cost.
    //
    // `taps_for_mana_overrides` carries the per-aura choice so the auto-tap
    // resolver can thread the correct `ProductionOverride` into the triggered
    // mana ability at resolution time. `atomic_combination` includes the full
    // output (land + aura) for planner coverage checks; `production_override_for_option`
    // caps it to the land's own portion when dispatching the land's own ability.
    let aura_bonus = match aura_sources {
        Some(sources) => taps_for_mana_aura_bonus_indexed(state, object_id, controller, sources),
        None => taps_for_mana_aura_bonus(state, object_id, controller),
    };
    for (aura_id, aura_choices) in aura_bonus {
        // aura_choices: [ManaType; N] where N=1 for Fixed, N=5 for any-color.
        // Cross-product: replace each option with N options (one per choice).
        options = options
            .into_iter()
            .flat_map(|opt| {
                aura_choices.iter().map(move |&bonus| {
                    let mut combined = opt
                        .atomic_combination
                        .clone()
                        .unwrap_or_else(|| vec![opt.mana_type]);
                    combined.push(bonus);
                    let distinct_color_count = combined
                        .iter()
                        .filter_map(|&mt| mana_type_to_color(mt))
                        .collect::<std::collections::HashSet<_>>()
                        .len();
                    // Carry the existing overrides plus this aura's choice.
                    let mut overrides = opt.taps_for_mana_overrides.clone();
                    overrides.push((aura_id, ProductionOverride::SingleColor(bonus)));
                    ManaSourceOption {
                        object_id: opt.object_id,
                        ability_index: opt.ability_index,
                        mana_type: combined[0],
                        source_could_produce_two_or_more_colors: opt
                            .source_could_produce_two_or_more_colors
                            || distinct_color_count >= 2,
                        penalty: opt.penalty,
                        atomic_combination: Some(combined),
                        restrictions: opt.restrictions.clone(),
                        taps_for_mana_overrides: overrides,
                    }
                })
            })
            .collect();
        // Deduplicate: if two base options already have the same combined
        // output (e.g., a land producing {G} twice), keep one.
        options.dedup();
    }

    options
}

/// CR 605.1a + CR 605.3a: Predicate for "this is an activated mana ability
/// with a `{T}` component that `controller` could currently activate."
/// Single authority shared by `scan_mana_abilities` (which builds per-color
/// `ManaSourceOption` rows) and `max_mana_yield` (which sums total output) so
/// the two never diverge on which abilities count as mana sources.
fn is_active_tap_mana_ability(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    ability_index: usize,
    ability: &AbilityDefinition,
    require_current_payability: bool,
    gates: Option<&mana_abilities::ManaActivationGates>,
) -> bool {
    if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
        return false;
    }
    if require_current_payability {
        let activatable = match gates {
            Some(gates) => mana_abilities::can_activate_mana_ability_now_gated(
                state,
                controller,
                object_id,
                ability_index,
                ability,
                gates,
            ),
            None => mana_abilities::can_activate_mana_ability_now(
                state,
                controller,
                object_id,
                ability_index,
                ability,
            ),
        };
        if !activatable {
            return false;
        }
    }
    if !has_tap_component(&ability.cost) {
        return false;
    }
    activation_condition_satisfied(state, controller, object_id, ability_index, ability)
}

/// Scan an object's abilities for activated mana abilities with a tap cost component.
/// Type-agnostic — works for lands, creatures, artifacts, etc.
fn scan_mana_abilities(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    object_id: ObjectId,
    controller: PlayerId,
    require_current_payability: bool,
    gates: Option<&mana_abilities::ManaActivationGates>,
) -> Vec<ManaSourceOption> {
    let mut options = Vec::new();
    for (ability_index, ability) in obj.abilities.iter().enumerate() {
        if !is_active_tap_mana_ability(
            state,
            object_id,
            controller,
            ability_index,
            ability,
            require_current_payability,
            gates,
        ) {
            continue;
        }

        let penalty = object_mana_ability_penalty(state, object_id, ability);
        let source_could_produce_two_or_more_colors =
            source_could_produce_two_or_more_colors(state, object_id, controller);
        for row in emit_source_rows(
            state,
            controller,
            object_id,
            ability_index,
            ability,
            require_current_payability,
        ) {
            let option = ManaSourceOption {
                object_id,
                ability_index: Some(ability_index),
                mana_type: row.mana_type,
                source_could_produce_two_or_more_colors,
                penalty,
                atomic_combination: row.atomic_combination,
                restrictions: row.restrictions,
                taps_for_mana_overrides: Vec::new(),
            };
            if !options.contains(&option) {
                options.push(option);
            }
        }
    }
    options
}

/// Per-ability source row: either a plain per-color candidate or a full
/// multi-mana combination. Used by `scan_mana_abilities` to build
/// `ManaSourceOption` rows uniformly across `ManaProduction` variants.
struct SourceRow {
    mana_type: ManaType,
    atomic_combination: Option<Vec<ManaType>>,
    restrictions: Vec<ManaRestriction>,
}

fn emit_source_rows(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    _ability_index: usize,
    ability: &AbilityDefinition,
    require_current_payability: bool,
) -> Vec<SourceRow> {
    let Effect::Mana {
        produced,
        restrictions,
        ..
    } = &*ability.effect
    else {
        return Vec::new();
    };
    let concrete_restrictions =
        super::effects::mana::resolve_restrictions(restrictions, state, object_id);
    match produced {
        // CR 605.3b + CR 106.1a: Filter-land combinations. Emit one row per
        // combination so the auto-tap shard assigner can pick whichever
        // combination satisfies the pending cost.
        ManaProduction::ChoiceAmongCombinations { options } => options
            .iter()
            .filter_map(|combo| {
                let types: Vec<ManaType> = combo.iter().map(mana_color_to_type).collect();
                types.first().copied().map(|first| SourceRow {
                    mana_type: first,
                    atomic_combination: Some(types),
                    restrictions: concrete_restrictions.clone(),
                })
            })
            .collect(),
        // CR 605.3b + CR 106.1a/b: Multi-mana from one activation must surface
        // as a single atomic combination so auto-tap does not plan two taps of
        // the same source (issue #2011).
        ManaProduction::Fixed { .. }
        | ManaProduction::Colorless { .. }
        | ManaProduction::Mixed { .. } => {
            let resolved =
                super::ability_utils::build_resolved_from_def(ability, object_id, controller);
            let types =
                super::effects::mana::resolve_mana_types_for_ability(produced, state, &resolved);
            if types.is_empty() {
                return Vec::new();
            }
            vec![SourceRow {
                mana_type: types[0],
                atomic_combination: (types.len() > 1).then_some(types),
                restrictions: concrete_restrictions.clone(),
            }]
        }
        // CR 106.5: a pure chosen-color source with no color chosen cannot
        // produce mana. Display/preview paths may show all five colors, but
        // auto-tap must not plan a payment around an undefined mana type.
        ManaProduction::ChosenColor {
            fixed_alternative: None,
            ..
        } if !require_current_payability
            && state
                .objects
                .get(&object_id)
                .and_then(|obj| obj.chosen_color())
                .is_none() =>
        {
            Vec::new()
        }
        _ => mana_options_from_production(state, controller, object_id, produced)
            .into_iter()
            .map(|mana_type| SourceRow {
                mana_type,
                atomic_combination: None,
                restrictions: concrete_restrictions.clone(),
            })
            .collect(),
    }
}

/// CR 605.3b — Mana abilities must still satisfy activation conditions.
/// Delegates to the shared restriction checker so that `RequiresCondition`,
/// once-per-turn limits, sorcery-speed, and all other restriction types
/// are enforced uniformly for mana source analysis.
pub(crate) fn activation_condition_satisfied(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    ability_index: usize,
    ability: &AbilityDefinition,
) -> bool {
    restrictions::check_activation_restrictions(
        state,
        controller,
        object_id,
        ability_index,
        &ability.activation_restrictions,
    )
    .is_ok()
}

/// CR 106.1: Resolve the mana-type option set for `ChosenColor` production.
///
/// Gate lands ("Add {B} or one mana of the chosen color") carry a
/// `fixed_alternative` alongside the as-enters chosen color — both are legal
/// outputs once the color is chosen. Pure chosen-color producers (Utopia Sprawl)
/// with no color chosen yet fall back to all five colors so preview and
/// mana-source analysis paths surface a non-empty set.
pub(crate) fn chosen_color_mana_type_options(
    state: &GameState,
    object_id: ObjectId,
    fixed_alternative: Option<ManaColor>,
) -> Vec<ManaType> {
    let mut options = Vec::new();
    if let Some(fixed) = fixed_alternative {
        options.push(mana_color_to_type(&fixed));
    }
    if let Some(chosen) = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.chosen_color())
    {
        let chosen_type = mana_color_to_type(&chosen);
        if !options.contains(&chosen_type) {
            options.push(chosen_type);
        }
    } else if fixed_alternative.is_none() {
        return ManaColor::ALL.iter().map(mana_color_to_type).collect();
    }
    options
}

fn mana_options_from_production(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    produced: &ManaProduction,
) -> Vec<ManaType> {
    match produced {
        ManaProduction::Fixed { colors, .. } => {
            let mut options = Vec::new();
            for color in colors {
                let mana_type = mana_color_to_type(color);
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
            options
        }
        ManaProduction::Colorless { .. } => vec![ManaType::Colorless],
        ManaProduction::AnyOneColor { color_options, .. }
        | ManaProduction::AnyCombination { color_options, .. } => {
            let mut options = Vec::new();
            for color in color_options {
                let mana_type = mana_color_to_type(color);
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
            options
        }
        // CR 106.1: Resolve chosen-color production, including the fixed-color
        // alternative on Gate lands ("Add {B} or one mana of the chosen color").
        ManaProduction::ChosenColor {
            fixed_alternative, ..
        } => chosen_color_mana_type_options(state, object_id, *fixed_alternative),
        // CR 106.7: Compute colors dynamically from opponent-controlled lands.
        ManaProduction::OpponentLandColors { .. } => opponent_land_color_options(state, controller),
        // CR 106.7 + CR 106.1b: Compute the full type set (incl. Colorless)
        // from lands matching `land_filter` (Reflecting Pool class).
        ManaProduction::AnyTypeProduceableBy { land_filter, .. } => {
            produceable_mana_types_by_filter(state, land_filter, controller, object_id)
        }
        // CR 605.1a + CR 406.1 + CR 610.3: Compute colors dynamically from cards
        // exiled-with this source via `state.exile_links` (Pit of Offerings).
        ManaProduction::ChoiceAmongExiledColors { source } => {
            super::effects::mana::exiled_color_options(state, *source, object_id)
        }
        // CR 605.3b + CR 106.1a: Filter lands — union of all colors appearing
        // across the combination options. Used for UI frame-color display
        // (`display_land_mana_colors`). The shard assigner in `casting_costs`
        // does NOT consume this — it uses `atomic_combination` on each
        // source row so combos are picked atomically.
        ManaProduction::ChoiceAmongCombinations { options } => {
            let mut out = Vec::new();
            for combo in options {
                for color in combo {
                    let mana_type = mana_color_to_type(color);
                    if !out.contains(&mana_type) {
                        out.push(mana_type);
                    }
                }
            }
            out
        }
        // CR 106.1: Mixed colorless + colored (e.g. {C}{W}, {C}{C}{R}).
        ManaProduction::Mixed {
            colorless_count,
            colors,
        } => {
            let mut out = Vec::new();
            if *colorless_count > 0 {
                out.push(ManaType::Colorless);
            }
            for color in colors {
                let mana_type = mana_color_to_type(color);
                if !out.contains(&mana_type) {
                    out.push(mana_type);
                }
            }
            out
        }
        // CR 903.4 + CR 903.4f: Compute colors dynamically from the
        // activator's commander(s)' combined color identity.
        ManaProduction::AnyInCommandersColorIdentity { .. } => {
            super::commander::commander_color_identity(state, controller)
                .iter()
                .map(mana_color_to_type)
                .collect()
        }
        // CR 106.1 + CR 109.1: Faeburrow-style "one of each color among permanents
        // you control". Delegates to the shared resolver so the cost-payment path
        // and direct activation see identical option sets.
        ManaProduction::DistinctColorsAmongPermanents { filter } => {
            super::effects::mana::distinct_colors_among_permanents(state, None, object_id, filter)
                .iter()
                .map(mana_color_to_type)
                .collect()
        }
        // CR 106.1: Determine available mana options from colors among matching permanents.
        ManaProduction::AnyOneColorAmongPermanents { filter, .. } => {
            super::effects::mana::distinct_colors_among_permanents(state, None, object_id, filter)
                .iter()
                .map(mana_color_to_type)
                .collect()
        }
        // CR 106.1 + CR 202.2c: Omnath, Locus of All — colors come from a target
        // object bound at trigger resolution, not available in this mana-source
        // enumeration context (no target / no ability). Contributes no option set
        // (CR 106.5), mirroring TriggerEventManaType.
        ManaProduction::AnyCombinationOfObjectColors { .. } => Vec::new(),
        // CR 603.7c + CR 106.3: "add one mana of any type that land produced"
        // resolves only inside a triggered ability (TapsForMana). For the mana
        // source enumeration path (cost-payment auto-tap, direct activation),
        // there is no trigger context — this variant has no pre-resolution
        // option set and contributes nothing to mana-source analysis.
        ManaProduction::TriggerEventManaType => Vec::new(),
    }
}

/// CR 205.4g / CR 106.3 / CR 107.4h: a permanent with the "snow" supertype is a snow
/// source; mana produced by its abilities is snow mana (spendable for {S}). Reads the
/// LAYERED (post-CR-613) supertype set on the object, so continuously-granted Snow counts
/// and continuously-removed Snow does not (CR 205.4b). A missing object / ObjectId(0)
/// sentinel is not a snow source.
pub(crate) fn source_is_snow(state: &GameState, source_id: ObjectId) -> bool {
    state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.card_types.supertypes.contains(&Supertype::Snow))
}

pub(crate) fn source_could_produce_two_or_more_colors(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };

    let mut colors = Vec::new();
    for ability in obj.abilities.iter() {
        if ability.kind != AbilityKind::Activated
            || !super::mana_abilities::is_mana_ability(ability)
        {
            continue;
        }
        let Effect::Mana { produced, .. } = &*ability.effect else {
            continue;
        };
        collect_production_colors(state, controller, object_id, produced, &mut colors);
        if colors.len() >= 2 {
            return true;
        }
    }

    for subtype in &obj.card_types.subtypes {
        if let Some(mana_type) = mana_payment::land_subtype_to_mana_type(subtype) {
            if mana_type != ManaType::Colorless && !colors.contains(&mana_type) {
                colors.push(mana_type);
            }
            if colors.len() >= 2 {
                return true;
            }
        }
    }

    false
}

pub(crate) fn mana_production_could_produce_two_or_more_colors(
    state: &GameState,
    controller: PlayerId,
    source_id: ObjectId,
    produced: &ManaProduction,
) -> bool {
    let mut colors = Vec::new();
    collect_production_colors(state, controller, source_id, produced, &mut colors);
    colors.len() >= 2
}

fn collect_production_colors(
    state: &GameState,
    controller: PlayerId,
    source_id: ObjectId,
    produced: &ManaProduction,
    colors: &mut Vec<ManaType>,
) {
    for mana_type in mana_options_from_production(state, controller, source_id, produced) {
        if mana_type == ManaType::Colorless || colors.contains(&mana_type) {
            continue;
        }
        colors.push(mana_type);
    }
}

/// CR 106.7: Compute the mana colors that lands controlled by opponents could produce.
///
/// Iterates over all opponent-controlled lands on the battlefield and collects the
/// union of mana colors their non-`OpponentLandColors` mana abilities could produce.
/// `OpponentLandColors` abilities are excluded to prevent infinite recursion when
/// an opponent also controls a card like Exotic Orchard.
pub(crate) fn opponent_land_color_options(
    state: &GameState,
    controller: PlayerId,
) -> Vec<ManaType> {
    let opponents = super::players::opponents(state, controller);
    let mut options = Vec::new();
    // CR 730.2: iterate `state.battlefield` (the independent-permanent list) so an
    // absorbed merge component is never counted as a separate mana source.
    for object_id in state.battlefield.iter() {
        let Some(obj) = state.objects.get(object_id) else {
            continue;
        };
        if !opponents.contains(&obj.controller) {
            continue;
        }
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            continue;
        }
        // Scan each mana ability, skipping recursive producers to prevent
        // mutual recursion (Exotic Orchard ↔ Exotic Orchard, Exotic Orchard ↔
        // Reflecting Pool). The skip set is symmetric with the one in
        // `produceable_mana_types_by_filter` — both directions exclude each
        // other so a cross-controller cycle yields the empty set (CR 106.5).
        for ability in obj.abilities.iter() {
            if ability.kind != AbilityKind::Activated
                || !super::mana_abilities::is_mana_ability(ability)
            {
                continue;
            }
            if !has_tap_component(&ability.cost) {
                continue;
            }
            let Effect::Mana { produced, .. } = &*ability.effect else {
                continue;
            };
            // CR 106.7: Skip both recursive producers. `OpponentLandColors`
            // facing itself yields no mana; `AnyTypeProduceableBy` (Reflecting
            // Pool class) is excluded because (a) recursing into it would
            // re-anchor `ControllerRef::You` to the wrong player and (b) the
            // mutual cycle terminates cleanly only when both sides skip each
            // other.
            if matches!(
                produced,
                ManaProduction::OpponentLandColors { .. }
                    | ManaProduction::AnyTypeProduceableBy { .. }
            ) {
                continue;
            }
            for mana_type in mana_options_from_production(state, controller, *object_id, produced) {
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
        }
        // Fallback: basic-land subtype-only objects (no explicit mana ability).
        // Check whether this specific object contributed any colors above — if not,
        // fall back to its land subtypes. (Must be per-object, not global, otherwise
        // once any land adds a color via an explicit ability, later basic lands with
        // no explicit ability silently skip the fallback.)
        let obj_had_explicit_ability = obj.abilities.iter().any(|ability| {
            if ability.kind != AbilityKind::Activated
                || !super::mana_abilities::is_mana_ability(ability)
                || !has_tap_component(&ability.cost)
            {
                return false;
            }
            !matches!(
                &*ability.effect,
                Effect::Mana {
                    produced: ManaProduction::OpponentLandColors { .. }
                        | ManaProduction::AnyTypeProduceableBy { .. },
                    ..
                }
            )
        });
        if !obj_had_explicit_ability {
            if let Some(mana_type) = obj
                .card_types
                .subtypes
                .iter()
                .find_map(|s| super::mana_payment::land_subtype_to_mana_type(s))
            {
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
        }
    }
    options
}

/// CR 605.1b + CR 106.12a: Enumerate the mana bonus options that
/// `TapsForMana`-triggered auras (Wild Growth, Fertile Ground, Utopia Sprawl,
/// Verdant Haven, etc.) would contribute when `land_id` is tapped by
/// `controller`.
///
/// Returns one inner `Vec<ManaType>` per aura per *color choice*:
/// - `Fixed` auras (Wild Growth): one element `[Green]` — a single concrete
///   bonus added unconditionally.
/// - `AnyOneColor` auras (Fertile Ground): one element per color option
///   (`[White]`, `[Blue]`, … `[Green]`) — the planner must pick exactly one
///   color per activation.
///
/// Callers use this to fan out `land_mana_options` into one
/// `ManaSourceOption` per reachable combination, preserving choice semantics
/// so a Forest + Fertile Ground correctly covers `{W}`, `{U}`, `{B}`, `{R}`,
/// or `{G}` as the bonus color.  `max_mana_yield` just takes `.len()` on the
/// outer vec (one bonus unit per aura regardless of color count).
///
/// Reuses `taps_for_mana_card_matches` — the same predicate the trigger
/// resolver uses — so planning and firing cannot drift.
/// Returns `(aura_object_id, color_choices)` per attached TapsForMana aura.
/// Callers use `aura_object_id` to build `taps_for_mana_overrides` on the
/// resulting `ManaSourceOption` so the resolver can thread the chosen color
/// into the aura's triggered mana ability at inline resolution time.
/// Battlefield objects carrying at least one `TapsForMana` trigger. The hot
/// board-global auto-tap / activatable sweeps compute this list ONCE before
/// their per-land loop and thread it into `taps_for_mana_aura_bonus_indexed`,
/// turning a per-land full-battlefield scan into a per-land walk over only the
/// (usually tiny) set of trigger-bearing permanents.
pub(crate) fn taps_for_mana_trigger_sources(state: &GameState) -> Vec<ObjectId> {
    crate::game::perf_counters::record_mana_aura_trigger_scan();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|object_id| {
            state.objects.get(object_id).is_some_and(|obj| {
                obj.trigger_definitions
                    .iter_all()
                    .any(|entry| entry.definition.mode == TriggerMode::TapsForMana)
            })
        })
        .collect()
}

pub(crate) fn taps_for_mana_aura_bonus(
    state: &GameState,
    land_id: ObjectId,
    controller: PlayerId,
) -> Vec<(ObjectId, Vec<ManaType>)> {
    let sources = taps_for_mana_trigger_sources(state);
    taps_for_mana_aura_bonus_indexed(state, land_id, controller, &sources)
}

/// Indexed arity of `taps_for_mana_aura_bonus`: iterates only the precomputed
/// `sources` (trigger-bearing permanents) instead of the whole battlefield.
/// Byte-identical to the full scan — preserves the `land_id` self-skip, the
/// per-source `taps_for_mana_card_matches` card-identity authority, and the
/// deliberate absence of a controller filter (Aura-Theft semantics).
pub(crate) fn taps_for_mana_aura_bonus_indexed(
    state: &GameState,
    land_id: ObjectId,
    controller: PlayerId,
    sources: &[ObjectId],
) -> Vec<(ObjectId, Vec<ManaType>)> {
    let mut per_aura: Vec<(ObjectId, Vec<ManaType>)> = Vec::new();
    for &object_id in sources.iter() {
        // Skip the land itself — we're looking for OTHER permanents whose
        // TapsForMana trigger fires when `land_id` is tapped.
        if object_id == land_id {
            continue;
        }
        let Some(obj) = state.objects.get(&object_id) else {
            continue;
        };
        let source_context = trigger_source_context_for_latch(state, obj);
        // Intentionally NOT filtering by obj.controller: an opponent can
        // control an aura attached to your land (e.g., via Aura Theft), and
        // the trigger still fires for the tapping player when the land is
        // tapped. `taps_for_mana_card_matches` handles the attachment check.
        for entry in obj.trigger_definitions.iter_all() {
            let trigger = entry.definition();
            if trigger.mode != TriggerMode::TapsForMana {
                continue;
            }
            if !super::trigger_matchers::taps_for_mana_card_matches(
                trigger,
                state,
                land_id,
                &source_context,
            ) {
                continue;
            }
            let Some(execute) = trigger.execute.as_deref() else {
                continue;
            };
            let Effect::Mana { produced, .. } = &*execute.effect else {
                continue;
            };
            // For Fixed production the choices are collapsed to one concrete
            // option; for AnyOneColor each color is a separate choice.
            // `mana_options_from_production` already does this enumeration.
            let choices = mana_options_from_production(state, controller, object_id, produced);
            if !choices.is_empty() {
                per_aura.push((object_id, choices));
            }
        }
    }
    per_aura
}

/// CR 605.1b + CR 605.3b: Enumerate object ids on the battlefield whose
/// `TapsForMana` triggered ability would fire when `land_id` is tapped for
/// mana by `controller`. Returns the trigger-source object ids (the auras /
/// equipment / static permanents whose trigger contributed mana to the pool
/// keyed at `source_id = aura_id`).
///
/// Used by `handle_untap_land_for_mana` to refund coupled bonus mana when the
/// player invokes the manual untap convenience: refunding only the land's
/// mana would strand the aura's contribution and allow an infinite
/// tap-untap-tap exploit (Fertile Ground, Wild Growth, Utopia Sprawl, Trace
/// of Abundance, Verdant Haven, Market Festival, Weirding Wood, Overgrowth).
///
/// Reuses `taps_for_mana_card_matches` — the same single-authority card-identity
/// predicate `match_taps_for_mana` uses at trigger-firing time — so the refund
/// and firing paths cannot drift. No `GameEvent` is synthesized: the predicate
/// is probed directly with the land as the mana source.
pub(crate) fn aura_taps_for_mana_sources_for_land(
    state: &GameState,
    land_id: ObjectId,
    controller: PlayerId,
) -> Vec<ObjectId> {
    let mut sources = Vec::new();
    // CR 730.2: iterate the independent-permanent list (excludes absorbed merge components).
    for &object_id in state.battlefield.iter() {
        let Some(obj) = state.objects.get(&object_id) else {
            continue;
        };
        let source_context = trigger_source_context_for_latch(state, obj);
        if obj.controller != controller {
            continue;
        }
        for entry in obj.trigger_definitions.iter_all() {
            let trigger = entry.definition();
            if trigger.mode != TriggerMode::TapsForMana {
                continue;
            }
            // CR 605.1b: A `TapsForMana` trigger fires when its `valid_card`
            // resolves to the land that produced mana. `AttachedTo` is the
            // canonical aura-on-land shape; `SelfRef` covers the
            // self-tapping land case (already refunded by the land's own
            // `source_id`, but kept here for completeness).
            if super::trigger_matchers::taps_for_mana_card_matches(
                trigger,
                state,
                land_id,
                &source_context,
            ) && object_id != land_id
                && !sources.contains(&object_id)
            {
                sources.push(object_id);
            }
        }
    }
    sources
}

/// CR 106.7 + CR 106.1b: Compute the mana types (W/U/B/R/G/C) that lands
/// matching `land_filter` could produce, surveyed from the perspective of
/// `controller`. Used by `ManaProduction::AnyTypeProduceableBy` (Reflecting
/// Pool, Naga Vitalist, Incubation Druid, Cactus Preserve, Horizon of Progress).
///
/// Differs from `opponent_land_color_options` in two ways:
/// 1. The land scope is parameterized via `TargetFilter` rather than hard-coded
///    to opponents — so "you control" / "an opponent controls" / future
///    "any player controls" variants slot in by passing a different filter.
/// 2. Returns the full *type* set including `Colorless`, matching CR 106.1b's
///    definition of "type" (six types) versus "color" (five colors). Reflecting
///    Pool reads "any **type**", so a Wastes you control contributes `Colorless`.
///
/// Per CR 106.7 the surveyed lands' mana abilities are inspected for what types
/// they *could* produce; cost-payability is ignored. Both `OpponentLandColors`
/// and `AnyTypeProduceableBy` abilities on the surveyed lands are skipped to
/// prevent infinite mutual recursion (e.g., two Reflecting Pools facing each
/// other with no other lands produce no mana, per CR 106.5).
pub(crate) fn produceable_mana_types_by_filter(
    state: &GameState,
    land_filter: &TargetFilter,
    controller: PlayerId,
    self_source_id: ObjectId,
) -> Vec<ManaType> {
    use crate::game::filter::{matches_target_filter, FilterContext};
    // CR 109.4: `ControllerRef::You` resolves against the activator. Anchor the
    // filter context to the explicit controller so the "you control" predicate
    // is correct even when the source object has left play (e.g., self-sac
    // costs) or in synthetic test contexts.
    let filter_ctx = FilterContext::from_source_with_controller(self_source_id, controller);
    let mut options = Vec::new();
    // CR 730.2: iterate the independent-permanent list (excludes absorbed merge components).
    for object_id in state.battlefield.iter() {
        let Some(obj) = state.objects.get(object_id) else {
            continue;
        };
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            continue;
        }
        if !matches_target_filter(state, *object_id, land_filter, &filter_ctx) {
            continue;
        }
        // CR 106.7: Survey each mana ability, skipping the recursive variants
        // so a self-referential cycle yields the empty set (CR 106.5).
        let mut obj_had_explicit_ability = false;
        for ability in obj.abilities.iter() {
            if ability.kind != AbilityKind::Activated
                || !super::mana_abilities::is_mana_ability(ability)
            {
                continue;
            }
            if !has_tap_component(&ability.cost) {
                continue;
            }
            let Effect::Mana { produced, .. } = &*ability.effect else {
                continue;
            };
            // CR 106.7: Skip recursive producers to prevent infinite mutual
            // recursion (Reflecting Pool ↔ Reflecting Pool, Reflecting Pool ↔
            // Exotic Orchard).
            if matches!(
                produced,
                ManaProduction::OpponentLandColors { .. }
                    | ManaProduction::AnyTypeProduceableBy { .. }
            ) {
                obj_had_explicit_ability = true;
                continue;
            }
            obj_had_explicit_ability = true;
            for mana_type in mana_options_from_production(state, controller, *object_id, produced) {
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
        }
        // Fallback: basic-land subtype-only objects (no explicit mana ability).
        if !obj_had_explicit_ability {
            if let Some(mana_type) = obj
                .card_types
                .subtypes
                .iter()
                .find_map(|s| super::mana_payment::land_subtype_to_mana_type(s))
            {
                if !options.contains(&mana_type) {
                    options.push(mana_type);
                }
            }
        }
    }
    options
}

pub fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
    }
}

pub fn mana_type_to_color(mana_type: ManaType) -> Option<ManaColor> {
    match mana_type {
        ManaType::White => Some(ManaColor::White),
        ManaType::Blue => Some(ManaColor::Blue),
        ManaType::Black => Some(ManaColor::Black),
        ManaType::Red => Some(ManaColor::Red),
        ManaType::Green => Some(ManaColor::Green),
        ManaType::Colorless => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ChosenAttribute, ManaContribution,
        QuantityExpr, SacrificeCost,
    };
    use crate::types::identifiers::CardId;

    fn verge_ability(color: ManaColor) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    use crate::game::test_fixtures::brushland_colored_ability;

    fn add_verge_land(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        unconditional_color: ManaColor,
        conditional_color: ManaColor,
        condition_text: &str,
    ) -> ObjectId {
        use crate::types::ability::ActivationRestriction;

        let verge = create_object(
            state,
            CardId(100),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&verge).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(verge_ability(unconditional_color));
        Arc::make_mut(&mut obj.abilities).push(
            verge_ability(conditional_color).activation_restrictions(vec![
                ActivationRestriction::RequiresCondition {
                    condition: crate::parser::oracle_condition::parse_restriction_condition(
                        condition_text,
                    ),
                },
            ]),
        );
        verge
    }

    /// Build a single-ability land with a given `ManaProduction` and run
    /// `display_land_mana_pips` against it. Used by the parametric pip table
    /// below to verify every variant projects to the expected `Vec<ManaPip>`.
    fn pips_for_production(production: ManaProduction) -> Vec<ManaPip> {
        pips_for_production_with_chosen_color(production, None)
    }

    fn pips_for_production_with_chosen_color(
        production: ManaProduction,
        chosen_color: Option<ManaColor>,
    ) -> Vec<ManaPip> {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        if let Some(color) = chosen_color {
            obj.chosen_attributes.push(ChosenAttribute::Color(color));
        }
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: production,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(ability);
        display_land_mana_pips(&state, id, PlayerId(0))
    }

    /// Build a single-ability `{T}`-cost producer with a given `ManaProduction`
    /// and return its `max_mana_yield`.
    fn yield_for_production(production: ManaProduction) -> u32 {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Test Producer".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: production,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(ability);
        max_mana_yield(&state, id, PlayerId(0))
    }

    /// CR 107.1b: `max_mana_yield` reports a producer's full mana output, not a
    /// flat 1 — so the `max_x_value` X-chooser bound reflects multi-mana
    /// sources (Sol Ring, bounce lands) instead of capping below affordability.
    #[test]
    fn max_mana_yield_counts_full_output_of_multi_mana_producers() {
        // Basic-land shape: one colored mana.
        assert_eq!(
            yield_for_production(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
            1,
        );
        // Sol Ring shape: one activation yields two colorless.
        assert_eq!(
            yield_for_production(ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 2 },
            }),
            2,
        );
        // Multi-color fixed sequence (e.g. a {W}{U} bounce-land output).
        assert_eq!(
            yield_for_production(ManaProduction::Fixed {
                colors: vec![ManaColor::White, ManaColor::Blue],
                contribution: ManaContribution::Base,
            }),
            2,
        );
    }

    /// CR 605.3a: A single `{T}` pays for only one mana ability — an object
    /// with several mana abilities yields the largest, never their sum. A
    /// tapped object can activate none of them.
    #[test]
    fn max_mana_yield_takes_best_ability_and_respects_tapped() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Multi-Mode Rock".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            for count in [2, 3] {
                Arc::make_mut(&mut obj.abilities).push(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::Mana {
                            produced: ManaProduction::Colorless {
                                count: QuantityExpr::Fixed { value: count },
                            },
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    )
                    .cost(AbilityCost::Tap),
                );
            }
        }
        // Best single ability is the count-3 mode — not 2 + 3 = 5.
        assert_eq!(max_mana_yield(&state, id, PlayerId(0)), 3);

        state.objects.get_mut(&id).unwrap().tapped = true;
        assert_eq!(max_mana_yield(&state, id, PlayerId(0)), 0);
    }

    /// CR 605.3b: A filter land (`{1}, {T}: Add two mana`) nets one mana — its
    /// gross output of two must not overstate the X a caster can afford.
    #[test]
    fn max_mana_yield_nets_out_activation_cost() {
        use crate::types::mana::{ManaCost, ManaUnit};

        let mut state = GameState::new_two_player(42);
        // Prime the pool so the `{1}` activation cost is currently payable —
        // otherwise `can_activate_mana_ability_now` rejects the ability.
        state.players[0]
            .mana_pool
            .add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));

        let id = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Filter Land".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 2 },
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                shards: vec![],
                                generic: 1,
                            },
                        },
                        AbilityCost::Tap,
                    ],
                }),
            );
        }
        // Gross output 2, minus the `{1}` activation cost → net 1.
        assert_eq!(max_mana_yield(&state, id, PlayerId(0)), 1);
    }

    /// Parametric coverage of every `ManaProduction` variant — each row asserts
    /// the projection a typical card of that shape produces. War Room
    /// (`Colorless`) and Command Tower (`AnyInCommandersColorIdentity`) are
    /// the bug-class examples; the broader table guarantees the building block
    /// holds for every variant.
    #[test]
    fn display_pips_cover_every_mana_production_variant() {
        // CR 106.1a: Fixed colored producer (Plains-style).
        let plains = pips_for_production(ManaProduction::Fixed {
            colors: vec![ManaColor::White],
            contribution: ManaContribution::Base,
        });
        assert_eq!(plains, vec![ManaPip::Color(ManaColor::White)]);

        // CR 106.1b: Pure colorless producer (War Room, Wastes).
        let war_room = pips_for_production(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        });
        assert_eq!(war_room, vec![ManaPip::Colorless]);

        // CR 106.1: Mixed colorless + colored (Karoo bounce land).
        let karoo = pips_for_production(ManaProduction::Mixed {
            colorless_count: 1,
            colors: vec![ManaColor::White],
        });
        assert_eq!(
            karoo,
            vec![ManaPip::Colorless, ManaPip::Color(ManaColor::White)]
        );

        // CR 106.4: AnyOneColor producer (City of Brass).
        let city_of_brass = pips_for_production(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        });
        assert_eq!(
            city_of_brass,
            vec![ManaPip::OneOfColors(ManaColor::ALL.to_vec())]
        );

        // CR 106.4: AnyCombination producer (Cascading Cataracts pays {5}).
        let cataracts = pips_for_production(ManaProduction::AnyCombination {
            count: QuantityExpr::Fixed { value: 5 },
            color_options: ManaColor::ALL.to_vec(),
        });
        assert_eq!(
            cataracts,
            vec![ManaPip::CombinationOfColors(ManaColor::ALL.to_vec())]
        );

        // CR 106.1a: ChosenColor producer with no chosen color yet falls back
        // to all five colors so the frame still renders something.
        let chosen = pips_for_production(ManaProduction::ChosenColor {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
            fixed_alternative: None,
        });
        assert_eq!(chosen, vec![ManaPip::OneOfColors(ManaColor::ALL.to_vec())]);

        // CR 106.1a: Thriving Grove-style producers display both the fixed
        // alternative and the source's chosen color.
        let fixed_or_chosen = pips_for_production_with_chosen_color(
            ManaProduction::ChosenColor {
                count: QuantityExpr::Fixed { value: 1 },
                contribution: ManaContribution::Base,
                fixed_alternative: Some(ManaColor::Green),
            },
            Some(ManaColor::Red),
        );
        assert_eq!(
            fixed_or_chosen,
            vec![
                ManaPip::Color(ManaColor::Green),
                ManaPip::Color(ManaColor::Red)
            ]
        );

        // CR 605.3b + CR 106.1a: Filter-land combinations (Mystic Gate-style)
        // collapse to the union of all colors across combos.
        let filter = pips_for_production(ManaProduction::ChoiceAmongCombinations {
            options: vec![
                vec![ManaColor::White, ManaColor::White],
                vec![ManaColor::White, ManaColor::Blue],
                vec![ManaColor::Blue, ManaColor::Blue],
            ],
        });
        assert_eq!(
            filter,
            vec![ManaPip::OneOfColors(vec![
                ManaColor::White,
                ManaColor::Blue
            ])]
        );

        // CR 903.4: Commander identity producer (Command Tower) — typed pip
        // surfaces the semantic so the frontend can resolve identity from
        // `Player::commander_color_identity`.
        let command_tower = pips_for_production(ManaProduction::AnyInCommandersColorIdentity {
            count: QuantityExpr::Fixed { value: 1 },
            contribution: ManaContribution::Base,
        });
        assert_eq!(command_tower, vec![ManaPip::AnyInCommandersIdentity]);

        // CR 106.7: OpponentLandColors with no opponent lands contributes
        // nothing; the variant is covered (no panic) and yields an empty pip
        // list.
        let exotic_orchard = pips_for_production(ManaProduction::OpponentLandColors {
            count: QuantityExpr::Fixed { value: 1 },
        });
        assert!(exotic_orchard.is_empty());

        // CR 603.7c: TapsForMana requires a trigger context and contributes
        // nothing pre-resolution.
        let trigger_only = pips_for_production(ManaProduction::TriggerEventManaType);
        assert!(trigger_only.is_empty());
    }

    #[test]
    fn gate_land_fixed_or_chosen_exposes_both_mana_options() {
        // Issue #2933: Black Dragon Gate — `{T}: Add {B} or one mana of the
        // chosen color` must surface both Black and the as-enters chosen color
        // in activatable land mana options (not just the chosen color).
        use crate::types::ability::ChosenAttribute;

        let mut state = GameState::new_two_player(42);
        let gate = create_object(
            &mut state,
            CardId(347),
            PlayerId(0),
            "Black Dragon Gate".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&gate).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.chosen_attributes
                .push(ChosenAttribute::Color(ManaColor::Red));
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::ChosenColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            contribution: ManaContribution::Base,
                            fixed_alternative: Some(ManaColor::Black),
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let options = activatable_land_mana_options(&state, gate, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(
            types.contains(&ManaType::Black),
            "Gate land must offer printed {{B}}, got {types:?}"
        );
        assert!(
            types.contains(&ManaType::Red),
            "Gate land must offer chosen color Red, got {types:?}"
        );
        assert_eq!(types.len(), 2, "expected exactly two distinct options");

        assert_eq!(
            chosen_color_mana_type_options(&state, gate, Some(ManaColor::Black)),
            vec![ManaType::Black, ManaType::Red]
        );
        assert!(source_could_produce_two_or_more_colors(
            &state,
            gate,
            PlayerId(0)
        ));
    }

    #[test]
    fn conditional_mana_blocked_without_supporting_land() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Gloomlake Verge",
            ManaColor::Blue,
            ManaColor::Black,
            "you control an Island or a Swamp",
        );

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(types.contains(&ManaType::Blue));
        assert!(!types.contains(&ManaType::Black));
    }

    #[test]
    fn conditional_mana_allowed_with_supporting_land() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Gloomlake Verge",
            ManaColor::Blue,
            ManaColor::Black,
            "you control an Island or a Swamp",
        );
        let island = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        let island_obj = state.objects.get_mut(&island).unwrap();
        island_obj.card_types.core_types.push(CoreType::Land);
        island_obj.card_types.subtypes.push("Island".to_string());

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(types.contains(&ManaType::Blue));
        assert!(types.contains(&ManaType::Black));
    }

    #[test]
    fn display_colors_ignore_tapped_state() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Gloomlake Verge",
            ManaColor::Blue,
            ManaColor::Black,
            "you control an Island or a Swamp",
        );
        let swamp = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        let swamp_obj = state.objects.get_mut(&swamp).unwrap();
        swamp_obj.card_types.core_types.push(CoreType::Land);
        swamp_obj.card_types.subtypes.push("Swamp".to_string());
        state.objects.get_mut(&verge).unwrap().tapped = true;

        let pips = display_land_mana_pips(&state, verge, PlayerId(0));
        assert!(pips.contains(&ManaPip::Color(ManaColor::Blue)));
        assert!(pips.contains(&ManaPip::Color(ManaColor::Black)));
    }

    #[test]
    fn riverpyre_verge_blocks_blue_without_island_or_mountain() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Riverpyre Verge",
            ManaColor::Red,
            ManaColor::Blue,
            "you control an Island or a Mountain",
        );

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(
            types.contains(&ManaType::Red),
            "unconditional red should be available"
        );
        assert!(
            !types.contains(&ManaType::Blue),
            "blue should NOT be available without Island/Mountain"
        );
    }

    #[test]
    fn riverpyre_verge_allows_blue_with_mountain() {
        let mut state = GameState::new_two_player(42);
        let verge = add_verge_land(
            &mut state,
            PlayerId(0),
            "Riverpyre Verge",
            ManaColor::Red,
            ManaColor::Blue,
            "you control an Island or a Mountain",
        );
        let mountain = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&mountain).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());

        let options = activatable_land_mana_options(&state, verge, PlayerId(0));
        let types: Vec<_> = options.iter().map(|o| o.mana_type).collect();
        assert!(types.contains(&ManaType::Red));
        assert!(
            types.contains(&ManaType::Blue),
            "blue should be available with Mountain in play"
        );
    }

    // ── activatable_mana_options tests ────────────────────────────────

    #[test]
    fn creature_mana_dork_returns_mana_options() {
        let mut state = GameState::new_two_player(42);
        let elf = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&elf).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(verge_ability(ManaColor::Green));
        // No summoning sickness: entered on a previous turn
        obj.entered_battlefield_turn = Some(0);
        state.turn_number = 2;

        let options = activatable_mana_options(&state, elf, PlayerId(0));
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].mana_type, ManaType::Green);
        assert_eq!(options[0].penalty, ManaSourcePenalty::None);
    }

    #[test]
    fn creature_with_summoning_sickness_returns_empty() {
        // CR 302.6: Creature that just entered can't activate tap abilities.
        let mut state = GameState::new_two_player(42);
        let elf = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&elf).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(verge_ability(ManaColor::Green));
        obj.entered_battlefield_turn = Some(1);
        obj.summoning_sick = true;
        state.turn_number = 1; // Same turn — summoning sickness

        let options = activatable_mana_options(&state, elf, PlayerId(0));
        assert!(
            options.is_empty(),
            "should be empty due to summoning sickness"
        );
    }

    #[test]
    fn treasure_token_returns_sacrifice_option() {
        // CR 111.10a: Treasure — "{T}, Sacrifice this artifact: Add one mana of any color."
        let mut state = GameState::new_two_player(42);
        let treasure = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&treasure).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Treasure".to_string());

        use crate::types::ability::{ManaContribution, ManaProduction, QuantityExpr, TargetFilter};
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        let obj = state.objects.get_mut(&treasure).unwrap();
        Arc::make_mut(&mut obj.abilities).push(ability);

        let options = activatable_mana_options(&state, treasure, PlayerId(0));
        assert!(!options.is_empty(), "Treasure should have mana options");
        assert!(
            options.iter().all(|o| o.penalty == ManaSourcePenalty::Sacrifices),
            "all Treasure options should classify as Sacrifices (sacrifice dominates any other penalty axis)"
        );
        assert!(
            options.iter().all(|o| !o.penalty.is_undoable()),
            "Treasure activations are irreversible because they sacrifice the source"
        );
        // Should have 5 color options
        assert_eq!(options.len(), 5);
    }

    #[test]
    fn life_payment_mana_source_marks_controller_harm() {
        let mut state = GameState::new_two_player(42);
        let town = create_object(
            &mut state,
            CardId(303),
            PlayerId(0),
            "Starting Town".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&town).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::PayLife {
                        amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }),
        );

        let options = activatable_land_mana_options(&state, town, PlayerId(0));
        assert!(
            !options.is_empty(),
            "Starting Town should expose mana options"
        );
        assert!(
            options.iter().all(|o| o.penalty
                == ManaSourcePenalty::PaysLifeOnActivation {
                    fixed_amount: Some(1)
                }),
            "pay-life mana sources should classify as PaysLifeOnActivation(1)"
        );
        assert!(
            options.iter().all(|o| !o.penalty.is_undoable()),
            "pay-life mana sources should not be undoable"
        );
    }

    #[test]
    fn pain_land_option_marks_controller_harm_and_non_undoable() {
        let mut state = GameState::new_two_player(42);
        let brushland = create_object(
            &mut state,
            CardId(304),
            PlayerId(0),
            "Brushland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&brushland).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(brushland_colored_ability());

        let options = activatable_land_mana_options(&state, brushland, PlayerId(0));
        assert_eq!(
            options.len(),
            2,
            "Brushland should expose green and white rows"
        );
        assert!(
            options.iter().all(|o| o.penalty
                == ManaSourcePenalty::DealsDamageOnResolution {
                    fixed_amount: Some(1)
                }),
            "pain-land colored rows should classify as DealsDamageOnResolution(1)"
        );
        assert!(
            options.iter().all(|o| !o.penalty.is_undoable()),
            "pain-land activations with damage continuations are not pure-mana undoable"
        );
    }

    /// Covers the `LoseLife` arm of `effect_controller_harm_amount`:
    /// a mana ability whose continuation drains the controller's life
    /// (rather than dealing damage) must still classify as
    /// DealsDamageOnResolution (the enum groups damage + life-loss as one
    /// resolution-time harm axis per CR 120.3).
    #[test]
    fn lose_life_mana_source_marks_controller_harm() {
        use crate::types::ability::Effect as AbilityEffect;

        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(305),
            PlayerId(0),
            "Hypothetical Drain Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                AbilityEffect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::Black, ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap)
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                AbilityEffect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: Some(crate::types::ability::TargetFilter::Controller),
                },
            )),
        );

        let options = activatable_land_mana_options(&state, land, PlayerId(0));
        assert!(!options.is_empty());
        assert!(
            options.iter().all(|o| o.penalty
                == ManaSourcePenalty::DealsDamageOnResolution {
                    fixed_amount: Some(1)
                }),
            "LoseLife(Controller) sub-effect should classify as DealsDamageOnResolution(1)"
        );
        assert!(
            options.iter().all(|o| !o.penalty.is_undoable()),
            "life-loss continuations are not pure-mana undoable"
        );
    }

    // ── ManaSourcePenalty classification ──────────────────────────────

    fn damage_sub_ability(amount: QuantityExpr) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount,
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            },
        )
    }

    fn mana_ability_with_damage(amount: QuantityExpr) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(damage_sub_ability(amount))
    }

    #[test]
    fn ancient_tomb_classifies_as_two_damage() {
        let ability = mana_ability_with_damage(QuantityExpr::Fixed { value: 2 });
        let penalty = mana_ability_penalty(&ability);
        assert_eq!(
            penalty,
            ManaSourcePenalty::DealsDamageOnResolution {
                fixed_amount: Some(2)
            },
        );
        assert_eq!(penalty.expected_life_cost(), 2);

        let painland_penalty = ManaSourcePenalty::DealsDamageOnResolution {
            fixed_amount: Some(1),
        };
        assert!(
            penalty.priority_amount() > painland_penalty.priority_amount(),
            "2-damage painland should sort after 1-damage painland within the same tier_byte"
        );
        assert_eq!(penalty.tier_byte(), painland_penalty.tier_byte());
    }

    #[test]
    fn variable_damage_pain_land_uses_none_amount() {
        let ability = mana_ability_with_damage(QuantityExpr::Ref {
            qty: crate::types::ability::QuantityRef::HandSize {
                player: crate::types::ability::PlayerScope::Controller,
            },
        });
        let penalty = mana_ability_penalty(&ability);
        assert_eq!(
            penalty,
            ManaSourcePenalty::DealsDamageOnResolution { fixed_amount: None }
        );
        assert_eq!(penalty.tier_byte(), 0);

        let fixed_max = ManaSourcePenalty::DealsDamageOnResolution {
            fixed_amount: Some(u16::MAX - 1),
        };
        assert!(
            penalty.priority_amount() > fixed_max.priority_amount(),
            "unknown amount (None) must sort strictly worse than any known amount (conservative worst)"
        );
    }

    /// Issue #5912: City of Brass prints its self-damage as a *separate*
    /// "Whenever this land becomes tapped, it deals 1 damage to you" trigger
    /// (`TriggerMode::Taps`, `valid_card: SelfRef`), NOT folded into the
    /// `{T}: Add one mana of any color.` ability's own resolution chain like a
    /// painland. `mana_ability_penalty` only walks the ability's own chain, so
    /// it misclassifies this shape as `None` (byte-identical to a basic land) —
    /// this is the reach-guard proving the gap exists before the fix.
    #[test]
    fn city_of_brass_ability_alone_misclassifies_as_none() {
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: ManaColor::ALL.to_vec(),
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);

        assert_eq!(
            mana_ability_penalty(&ability),
            ManaSourcePenalty::None,
            "the ability's own chain is pure Effect::Mana with no embedded damage"
        );
    }

    /// Issue #5912: [`object_mana_ability_penalty`] must see past the ability
    /// chain and classify a City-of-Brass-shaped object (mana ability + sibling
    /// self-referential `Taps` damage trigger) as `DealsDamageOnResolution`, so
    /// auto-tap sorts it worse than a truly free source (Island) for the same
    /// generic-mana slot.
    #[test]
    fn object_mana_ability_penalty_sees_city_of_brass_self_tap_damage_trigger() {
        let mut state = GameState::new_two_player(42);
        let city_of_brass = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "City of Brass".to_string(),
            Zone::Battlefield,
        );
        let ability_index;
        {
            let obj = state.objects.get_mut(&city_of_brass).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            let ability = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: ManaColor::ALL.to_vec(),
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap);
            ability_index = obj.abilities.len();
            Arc::make_mut(&mut obj.abilities).push(ability);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Taps)
                    .valid_card(TargetFilter::SelfRef)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::DealDamage {
                            amount: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                            damage_source: None,
                            excess: None,
                        },
                    )),
            );
        }

        let ability = state.objects.get(&city_of_brass).unwrap().abilities[ability_index].clone();
        let penalty = object_mana_ability_penalty(&state, city_of_brass, &ability);
        assert_eq!(
            penalty,
            ManaSourcePenalty::DealsDamageOnResolution {
                fixed_amount: Some(1)
            },
            "the sibling self-tap damage trigger must be folded into the classification"
        );
        assert!(
            penalty.priority_amount() > ManaSourcePenalty::None.priority_amount(),
            "must sort strictly worse than a truly free source (Island) in the same tier_byte"
        );
    }

    /// A sibling `Taps` trigger fires only when the selected mana ability taps
    /// its source. A no-cost mana ability on the same City-of-Brass-shaped
    /// object must retain its own penalty classification rather than borrowing
    /// damage from an event it cannot cause.
    #[test]
    fn object_mana_ability_penalty_ignores_self_tap_trigger_for_no_tap_ability() {
        let mut state = GameState::new_two_player(42);
        let city_of_brass = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "City of Brass".to_string(),
            Zone::Battlefield,
        );
        let ability_index;
        {
            let obj = state.objects.get_mut(&city_of_brass).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            let ability = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: ManaColor::ALL.to_vec(),
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            );
            ability_index = obj.abilities.len();
            Arc::make_mut(&mut obj.abilities).push(ability);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Taps)
                    .valid_card(TargetFilter::SelfRef)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::DealDamage {
                            amount: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                            damage_source: None,
                            excess: None,
                        },
                    )),
            );
        }

        let ability = state.objects.get(&city_of_brass).unwrap().abilities[ability_index].clone();
        assert_eq!(
            object_mana_ability_penalty(&state, city_of_brass, &ability),
            ManaSourcePenalty::None,
            "a no-tap mana ability cannot fire the sibling Taps trigger"
        );
    }

    /// Build a `{T}`-cost tap-mana producer whose cost is `cost`. Used by the
    /// sacrifice-classifier tests to drive the real `mana_ability_penalty`
    /// against the real parsed cost shapes.
    fn mana_ability_with_cost(cost: AbilityCost) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(cost)
    }

    /// CR 701.21: Krark-Clan Ironworks — "Sacrifice an artifact: Add {C}{C}."
    /// Its parsed cost is a BARE `Sacrifice` with a `Typed(Artifact)` target
    /// (not `Composite`, not `SelfRef`). Before the classifier was widened this
    /// fell through to `None`, so the priority gate auto-passed it (issue #544).
    #[test]
    fn krark_clan_ironworks_classifies_as_sacrifices() {
        use crate::types::ability::{TargetFilter, TypeFilter, TypedFilter};
        let ability = mana_ability_with_cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            1,
        )));
        assert_eq!(
            mana_ability_penalty(&ability),
            ManaSourcePenalty::Sacrifices,
            "a bare Sacrifice cost with a Typed target must classify as Sacrifices"
        );
    }

    /// CR 701.21: Phyrexian Tower / Ashnod's Altar shape — `Composite[Tap,
    /// Sacrifice{Typed(Creature)}]`. Sacrifices a permanent other than the
    /// source, nested inside a `Composite`.
    #[test]
    fn phyrexian_tower_shape_classifies_as_sacrifices() {
        use crate::types::ability::{TargetFilter, TypedFilter};
        let ability = mana_ability_with_cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::creature()),
                    1,
                )),
            ],
        });
        assert_eq!(
            mana_ability_penalty(&ability),
            ManaSourcePenalty::Sacrifices,
            "a Composite cost containing a Sacrifice{{Typed(Creature)}} must classify as Sacrifices"
        );
    }

    /// CR 701.21: self-sac token shape (Treasure / Gold / Lotus Petal) —
    /// `Composite[Tap, Sacrifice{SelfRef}]`. Must still classify as
    /// `Sacrifices` after the classifier was widened (no regression).
    #[test]
    fn self_sac_token_shape_still_classifies_as_sacrifices() {
        use crate::types::ability::TargetFilter;
        let ability = mana_ability_with_cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        assert_eq!(
            mana_ability_penalty(&ability),
            ManaSourcePenalty::Sacrifices,
            "a Composite cost containing a Sacrifice{{SelfRef}} must still classify as Sacrifices"
        );
    }

    #[test]
    fn penalty_total_order() {
        let none = ManaSourcePenalty::None;
        let irreversible = ManaSourcePenalty::HasIrreversibleContinuation;
        let damage_1 = ManaSourcePenalty::DealsDamageOnResolution {
            fixed_amount: Some(1),
        };
        let damage_2 = ManaSourcePenalty::DealsDamageOnResolution {
            fixed_amount: Some(2),
        };
        let damage_none = ManaSourcePenalty::DealsDamageOnResolution { fixed_amount: None };
        let pay_1 = ManaSourcePenalty::PaysLifeOnActivation {
            fixed_amount: Some(1),
        };
        let pay_none = ManaSourcePenalty::PaysLifeOnActivation { fixed_amount: None };
        let sacrifice = ManaSourcePenalty::Sacrifices;

        // Within tier_byte == 0, priority_amount orders
        // None < HasIrreversibleContinuation < damage-1 < damage-2 < damage-None
        // < pay-1 < pay-None.
        let ordered = [
            none,
            irreversible,
            damage_1,
            damage_2,
            damage_none,
            pay_1,
            pay_none,
        ];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].priority_amount() < pair[1].priority_amount(),
                "{:?} should sort before {:?}",
                pair[0],
                pair[1]
            );
            assert_eq!(pair[0].tier_byte(), 0);
            assert_eq!(pair[1].tier_byte(), 0);
        }
        // Sacrifice is always last via tier_byte, regardless of priority_amount.
        assert_eq!(sacrifice.tier_byte(), 1);
        for p in ordered {
            assert!(
                p.tier_byte() < sacrifice.tier_byte(),
                "{p:?} should sort in a lower tier than Sacrifices"
            );
        }

        // Undoability / freeness: only None qualifies.
        for p in [
            irreversible,
            damage_1,
            damage_none,
            pay_1,
            pay_none,
            sacrifice,
        ] {
            assert!(!p.is_undoable(), "{p:?} must not be undoable");
            assert!(!p.is_free(), "{p:?} must not be free");
        }
        assert!(none.is_undoable());
        assert!(none.is_free());

        // Expected life cost: damage + pay-life contribute fixed amounts;
        // None / HasIrreversibleContinuation / Sacrifices are 0; dynamic
        // amounts saturate to 0 per the "don't guess" rule.
        assert_eq!(none.expected_life_cost(), 0);
        assert_eq!(irreversible.expected_life_cost(), 0);
        assert_eq!(damage_1.expected_life_cost(), 1);
        assert_eq!(damage_2.expected_life_cost(), 2);
        assert_eq!(damage_none.expected_life_cost(), 0);
        assert_eq!(pay_1.expected_life_cost(), 1);
        assert_eq!(pay_none.expected_life_cost(), 0);
        assert_eq!(sacrifice.expected_life_cost(), 0);

        // Full fixture sort — simulates the `casting_costs.rs` auto-tap key
        // `(tier_byte, card_tier, priority_amount)` for a representative
        // mix of card types. Pre-refactor behavior: basic land < painland-1
        // < painland-2 < mana dork < animated land < deprioritized < Treasure.
        #[derive(Debug)]
        struct Fixture {
            label: &'static str,
            card_tier: u32,
            penalty: ManaSourcePenalty,
        }
        let mut fixtures = [
            Fixture {
                label: "basic land",
                card_tier: 0,
                penalty: none,
            },
            Fixture {
                label: "depletion land",
                card_tier: 0,
                penalty: irreversible,
            },
            Fixture {
                label: "painland-1",
                card_tier: 0,
                penalty: damage_1,
            },
            Fixture {
                label: "painland-2",
                card_tier: 0,
                penalty: damage_2,
            },
            Fixture {
                label: "mana dork",
                card_tier: 1,
                penalty: none,
            },
            Fixture {
                label: "animated land",
                card_tier: 2,
                penalty: none,
            },
            Fixture {
                label: "deprioritized rock",
                card_tier: 3,
                penalty: none,
            },
            Fixture {
                label: "treasure",
                card_tier: 0,
                penalty: sacrifice,
            },
        ];
        fixtures.sort_by_key(|f| {
            (
                f.penalty.tier_byte() as u32,
                f.card_tier,
                f.penalty.priority_amount(),
            )
        });
        let sorted_labels: Vec<_> = fixtures.iter().map(|f| f.label).collect();
        assert_eq!(
            sorted_labels,
            vec![
                "basic land",
                "depletion land",
                "painland-1",
                "painland-2",
                "mana dork",
                "animated land",
                "deprioritized rock",
                "treasure",
            ],
            "full fixture sort must match the pre-refactor auto-tap behavior, \
             with HasIrreversibleContinuation wedged between basics and painlands"
        );
    }

    /// Regression anchor: synthesize a mana ability whose continuation chain
    /// contains an effect we don't otherwise classify (here `Untap` of self
    /// stands in for the "self-haste / depletion-counter / unimplemented tail"
    /// class). The classifier must route it to `HasIrreversibleContinuation`
    /// — never to `None` — so `is_undoable()` returns `false`.
    #[test]
    fn unrecognized_chain_effect_classifies_as_irreversible() {
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            // Stand-in for an unclassified non-mana side effect; the classifier
            // is structural — any non-`Mana` effect in the chain triggers the
            // irreversible variant.
            Effect::Unimplemented {
                name: "regression fixture".to_string(),
                description: None,
            },
        ));

        let penalty = mana_ability_penalty(&ability);
        assert_eq!(penalty, ManaSourcePenalty::HasIrreversibleContinuation);
        assert!(!penalty.is_undoable());
        assert!(!penalty.is_free());
        assert_eq!(penalty.tier_byte(), 0);
        assert_eq!(penalty.expected_life_cost(), 0);
        assert!(
            penalty.priority_amount() > ManaSourcePenalty::None.priority_amount(),
            "irreversible-continuation must sort strictly worse than None"
        );
        assert!(
            penalty.priority_amount()
                < ManaSourcePenalty::DealsDamageOnResolution {
                    fixed_amount: Some(1),
                }
                .priority_amount(),
            "irreversible-continuation must sort strictly better than any painland"
        );
    }

    #[test]
    fn none_penalty_implies_pure_mana_chain() {
        // CR 605.3b: This sweep is the empirical guarantee that the type
        // system's promise — `ManaSourcePenalty::None` ⇒ pure-mana chain ⇒
        // `is_undoable()` — actually holds across every printed mana ability
        // the parser currently emits. Any chain shape that carries a non-mana
        // continuation must classify into `HasIrreversibleContinuation` (or a
        // more specific damage / pay-life / sacrifice variant); a `None`
        // result with a non-mana continuation in its sub-graph is a
        // classifier bug, not a parser bug.
        use crate::database::CardDatabase;
        use crate::game::mana_abilities::is_mana_ability;
        use std::path::Path;

        // Walk up from the crate root to find the repo's client/public
        // card-data.json. Tests may run from the workspace root or the
        // crate dir depending on the runner; try both.
        // allow-full-card-db: whole-corpus mana-classifier drift guard — must scan every printed card
        let candidates = [
            Path::new("client/public/card-data.json"),
            Path::new("../../client/public/card-data.json"),
        ];
        let Some(path) = candidates.iter().find(|p| p.exists()).copied() else {
            eprintln!(
                "SKIP none_penalty_implies_pure_mana_chain: card-data.json missing \
                 (primary CI lanes regenerate it; local runs without it skip)"
            );
            return;
        };
        let db = match CardDatabase::from_export(path) {
            Ok(db) => db,
            Err(err) => {
                eprintln!("SKIP none_penalty_implies_pure_mana_chain: load error: {err}");
                return;
            }
        };

        fn find_non_mana_side_effect(ability: &AbilityDefinition) -> Option<&Effect> {
            // `ability.effect` is the Mana production itself; only scan the
            // continuation chain.
            if let Some(sub) = ability.sub_ability.as_deref() {
                if let Some(bad) = walk(sub) {
                    return Some(bad);
                }
            }
            if let Some(other) = ability.else_ability.as_deref() {
                if let Some(bad) = walk(other) {
                    return Some(bad);
                }
            }
            return None;

            fn walk(ability: &AbilityDefinition) -> Option<&Effect> {
                if !matches!(*ability.effect, Effect::Mana { .. }) {
                    return Some(&ability.effect);
                }
                if let Some(sub) = ability.sub_ability.as_deref() {
                    if let Some(bad) = walk(sub) {
                        return Some(bad);
                    }
                }
                if let Some(other) = ability.else_ability.as_deref() {
                    if let Some(bad) = walk(other) {
                        return Some(bad);
                    }
                }
                None
            }
        }

        let mut offenders: Vec<(String, String)> = Vec::new();
        for (name, face) in db.face_iter() {
            for ability in &face.abilities {
                if !is_mana_ability(ability) {
                    continue;
                }
                if mana_ability_penalty(ability) != ManaSourcePenalty::None {
                    continue;
                }
                if let Some(bad) = find_non_mana_side_effect(ability) {
                    offenders.push((name.to_string(), format!("{bad:?}")));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "ManaSourcePenalty::None classification is leaking impure chains \
             — these cards should classify as HasIrreversibleContinuation \
             (or a more specific harm variant): {offenders:#?}"
        );
    }

    // ── Wild Growth / Fertile Ground autotap (#4265) ────────────────────────

    use crate::types::ability::TriggerDefinition;

    /// Build a Wild-Growth–style aura attached to `land_id`: a TapsForMana
    /// trigger on a separate Enchantment object that adds `bonus_color` when
    /// the land is tapped.
    fn attach_taps_for_mana_aura(
        state: &mut GameState,
        land_id: ObjectId,
        controller: PlayerId,
        bonus_color: ManaColor,
    ) -> ObjectId {
        let aura = create_object(
            state,
            CardId(99),
            controller,
            "Wild Growth".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.attached_to = Some(land_id.into());
        obj.entered_battlefield_turn = Some(1);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![bonus_color],
                            contribution: ManaContribution::Additional,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::AttachedTo),
        );
        aura
    }

    /// Issue #4265: `taps_for_mana_aura_bonus` returns `[Green]` for a Forest
    /// enchanted by Wild Growth.
    #[test]
    fn aura_bonus_detects_taps_for_mana_trigger_on_attached_aura() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        attach_taps_for_mana_aura(&mut state, forest, PlayerId(0), ManaColor::Green);

        // Fixed aura: one aura, one color choice.
        let bonus = taps_for_mana_aura_bonus(&state, forest, PlayerId(0));
        assert_eq!(bonus.len(), 1, "one aura");
        assert_eq!(bonus[0].1, vec![ManaType::Green]);
    }

    /// Issue #4265: A bare Forest (no aura) has no TapsForMana bonus.
    #[test]
    fn aura_bonus_empty_for_unenchanted_land() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        assert!(taps_for_mana_aura_bonus(&state, forest, PlayerId(0)).is_empty());
    }

    /// Item B (revert-failing perf): the board-global auto-tap sweep computes the
    /// TapsForMana trigger-source list ONCE before its per-land loop, not once
    /// per land. With K lands and one Wild-Growth aura, exactly one
    /// battlefield trigger-source scan runs. Pre-fix every land re-scanned the
    /// whole battlefield inside `land_mana_options` (K scans).
    #[test]
    fn auto_tap_sweep_scans_aura_sources_once() {
        const K: u64 = 5;
        let mut state = GameState::new_two_player(42);
        let mut forests = Vec::new();
        for i in 0..K {
            let forest = create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(0);
            forests.push(forest);
        }
        attach_taps_for_mana_aura(&mut state, forests[0], PlayerId(0), ManaColor::Green);

        let cost = crate::types::mana::ManaCost::Cost {
            shards: Vec::new(),
            generic: 2,
        };
        let mut events = Vec::new();
        crate::game::perf_counters::reset();
        crate::game::casting_costs::auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &cost,
            &mut events,
            None,
        );
        let snap = crate::game::perf_counters::snapshot();

        assert_eq!(
            snap.mana_aura_trigger_scans, 1,
            "one trigger-source scan for the whole sweep (revert-failing: pre-fix = K)"
        );
    }

    /// Item B byte-identical: the indexed arity over a precomputed source list
    /// detects exactly the same auras as the full wrapper, including the
    /// deliberate no-controller-filter (an opponent-controlled aura still folds
    /// in, Aura-Theft semantics) and two-auras-on-one-land.
    #[test]
    fn taps_for_mana_aura_bonus_indexed_matches_full_scan_with_two_auras() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        // One opponent-controlled aura (no controller filter) + one own aura.
        attach_taps_for_mana_aura(&mut state, forest, PlayerId(1), ManaColor::Green);
        attach_taps_for_mana_aura(&mut state, forest, PlayerId(0), ManaColor::Blue);

        let full = taps_for_mana_aura_bonus(&state, forest, PlayerId(0));
        let sources = taps_for_mana_trigger_sources(&state);
        let indexed = taps_for_mana_aura_bonus_indexed(&state, forest, PlayerId(0), &sources);

        assert_eq!(
            full.len(),
            2,
            "both auras fold in regardless of controller (no controller filter)"
        );
        assert_eq!(
            full, indexed,
            "indexed arity is byte-identical to the full wrapper"
        );
    }

    /// Issue #4265 regression: `auto_tap_land_mana_options` for a Forest
    /// enchanted by Wild Growth must return a single `atomic_combination`
    /// containing `[Green, Green]` so the autotap planner treats the whole
    /// {G}{G} output as one atomic tap — not two separate taps of the same
    /// land. Before the fix, the bonus {G} was invisible to the planner and
    /// Wild Growth lands were undervalued in mana coverage checks.
    #[test]
    fn land_mana_options_includes_wild_growth_aura_bonus_as_atomic_combination() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        let wild_growth =
            attach_taps_for_mana_aura(&mut state, forest, PlayerId(0), ManaColor::Green);

        let sources = taps_for_mana_trigger_sources(&state);
        let options = auto_tap_land_mana_options_indexed(&state, forest, PlayerId(0), &sources);
        assert_eq!(options.len(), 1, "one option per tap");
        let opt = &options[0];
        assert_eq!(opt.mana_type, ManaType::Green);
        assert_eq!(
            opt.atomic_combination,
            Some(vec![ManaType::Green, ManaType::Green]),
            "Wild Growth bonus must be folded into the atomic combination"
        );
        // Forest + Wild Growth = {G}{G}: same color, so the two-or-more-colors
        // flag must NOT be set (CR 106.1b counts distinct colors, not quantity).
        assert!(!opt.source_could_produce_two_or_more_colors);
        // Resolver override: Wild Growth's trigger must produce Green.
        assert_eq!(opt.taps_for_mana_overrides.len(), 1);
        assert_eq!(opt.taps_for_mana_overrides[0].0, wild_growth);
        assert_eq!(
            opt.taps_for_mana_overrides[0].1,
            ProductionOverride::SingleColor(ManaType::Green),
        );
    }

    /// Issue #4265: `max_mana_yield` counts the aura bonus so X-value
    /// choosers know the enchanted land produces 2, not 1.
    ///
    /// `max_mana_yield` uses `activatable_mana_options` for its subtype-only
    /// fallback, which does NOT include the basic-subtype synthesised option
    /// that `land_mana_options` adds. Production Forests always carry an
    /// explicit `{T}: Add {G}` ability from the parser, so the test mirrors
    /// that by adding `verge_ability(Green)` (a plain `{T}: Add {G}`).
    #[test]
    fn max_mana_yield_counts_wild_growth_aura_bonus() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
            // Mirror production: real Forests carry an explicit {T}: Add {G}.
            Arc::make_mut(&mut obj.abilities).push(verge_ability(ManaColor::Green));
        }

        assert_eq!(max_mana_yield(&state, forest, PlayerId(0)), 1, "baseline");
        attach_taps_for_mana_aura(&mut state, forest, PlayerId(0), ManaColor::Green);
        assert_eq!(
            max_mana_yield(&state, forest, PlayerId(0)),
            2,
            "Wild Growth adds 1 more mana"
        );
    }

    /// Issue #4265: `display_land_mana_pips` shows two green pips for a
    /// Forest enchanted by Wild Growth.
    #[test]
    fn display_pips_includes_wild_growth_aura_bonus() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        let pips_before = display_land_mana_pips(&state, forest, PlayerId(0));
        assert_eq!(pips_before, vec![ManaPip::Color(ManaColor::Green)]);

        attach_taps_for_mana_aura(&mut state, forest, PlayerId(0), ManaColor::Green);
        let pips_after = display_land_mana_pips(&state, forest, PlayerId(0));
        // Aura bonus pips bypass the deduplication closure so the UI frame
        // shows two {G} symbols: one from the land's own activated ability and
        // one from Wild Growth's trigger.
        assert_eq!(
            pips_after,
            vec![
                ManaPip::Color(ManaColor::Green),
                ManaPip::Color(ManaColor::Green)
            ],
            "Forest + Wild Growth must show two green pips: {pips_after:?}"
        );
    }

    // ── Fertile Ground (AnyOneColor bonus) ──────────────────────────────────

    /// Build a Fertile Ground–style aura: `TapsForMana` trigger that adds one
    /// mana of any color (`AnyOneColor` with all five colors as options).
    fn attach_any_color_aura(
        state: &mut GameState,
        land_id: ObjectId,
        controller: PlayerId,
    ) -> ObjectId {
        let aura = create_object(
            state,
            CardId(98),
            controller,
            "Fertile Ground".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.attached_to = Some(land_id.into());
        obj.entered_battlefield_turn = Some(1);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: ManaColor::ALL.to_vec(),
                            contribution: ManaContribution::Additional,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::AttachedTo),
        );
        aura
    }

    /// Issue #4265 / Fertile Ground regression: `taps_for_mana_aura_bonus`
    /// returns five choices (one per color) for an `AnyOneColor` aura so the
    /// planner can pick whichever color satisfies the pending cost.
    #[test]
    fn aura_bonus_any_color_returns_five_choices() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        attach_any_color_aura(&mut state, forest, PlayerId(0));

        let bonus = taps_for_mana_aura_bonus(&state, forest, PlayerId(0));
        assert_eq!(bonus.len(), 1, "one aura");
        // Each color option is a separate entry in the inner vec.
        assert_eq!(
            bonus[0].1.len(),
            5,
            "AnyOneColor aura must surface all five color choices"
        );
        for color in [
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ] {
            assert!(
                bonus[0].1.contains(&color),
                "missing {color:?} in aura bonus choices"
            );
        }
    }

    /// Issue #4265 / Fertile Ground regression: `land_mana_options` fans out
    /// into five options for Forest + Fertile Ground (one per bonus color) so
    /// the autotap planner can use the bonus to pay costs in any color.
    #[test]
    fn land_mana_options_fans_out_any_color_aura_bonus() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        let fertile_ground = attach_any_color_aura(&mut state, forest, PlayerId(0));

        let sources = taps_for_mana_trigger_sources(&state);
        let options = auto_tap_land_mana_options_indexed(&state, forest, PlayerId(0), &sources);
        // Forest subtype fallback = one base option {G}.
        // Fertile Ground fans out × 5 → five options.
        assert_eq!(options.len(), 5, "Forest + Fertile Ground = 5 options");
        // Every option starts with Green (the land's own output).
        for opt in &options {
            assert_eq!(opt.mana_type, ManaType::Green);
            let combo = opt
                .atomic_combination
                .as_ref()
                .expect("must have combination");
            assert_eq!(
                combo[0],
                ManaType::Green,
                "first type must be land's own {{G}}"
            );
            assert_eq!(combo.len(), 2, "two-element combination: land + aura");
            // Resolver override: one entry pointing to Fertile Ground.
            assert_eq!(opt.taps_for_mana_overrides.len(), 1);
            assert_eq!(opt.taps_for_mana_overrides[0].0, fertile_ground);
            // Override matches the bonus color for this option.
            assert_eq!(
                opt.taps_for_mana_overrides[0].1,
                ProductionOverride::SingleColor(combo[1]),
            );
        }
        // All five colors must appear as the second element across the options.
        let bonus_colors: Vec<ManaType> = options
            .iter()
            .map(|o| o.atomic_combination.as_ref().unwrap()[1])
            .collect();
        for color in [
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ] {
            assert!(
                bonus_colors.contains(&color),
                "bonus color {color:?} missing from options: {bonus_colors:?}"
            );
        }
        // Forest + Fertile Ground should flag as multi-color source because
        // the combined types can span two distinct colors (e.g., G + W).
        assert!(
            options
                .iter()
                .any(|o| o.source_could_produce_two_or_more_colors),
            "at least one option must flag two-or-more-colors"
        );
        // The {G}+{G} option must NOT flag two-or-more-colors (same color).
        let gg_opt = options
            .iter()
            .find(|o| o.atomic_combination.as_ref().unwrap()[1] == ManaType::Green);
        assert!(
            gg_opt.is_some_and(|o| !o.source_could_produce_two_or_more_colors),
            "{{G}}+{{G}} option must not flag two-or-more-colors"
        );
    }

    /// Issue #4265 / Fertile Ground regression: `display_land_mana_pips` emits
    /// a `OneOfColors` pip for an AnyOneColor aura bonus (like City of Brass),
    /// not five separate concrete pips.
    #[test]
    fn display_pips_any_color_aura_emits_one_of_colors_pip() {
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        attach_any_color_aura(&mut state, forest, PlayerId(0));

        let pips = display_land_mana_pips(&state, forest, PlayerId(0));
        // First pip: {G} from subtype fallback. Second: OneOfColors from Fertile Ground.
        assert_eq!(pips.len(), 2, "two pips: {{G}} plus OneOfColors");
        assert_eq!(pips[0], ManaPip::Color(ManaColor::Green));
        assert!(
            matches!(&pips[1], ManaPip::OneOfColors(colors) if colors.len() == 5),
            "second pip must be OneOfColors with 5 options, got: {pips:?}"
        );
    }

    // ── G1: beneficial mana-tap trigger classifier ──────────────────────────

    fn fixed(value: i32) -> QuantityExpr {
        QuantityExpr::Fixed { value }
    }

    fn opponent_scope() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![],
            controller: Some(ControllerRef::Opponent),
            properties: vec![],
        })
    }

    #[test]
    fn effect_benefits_classifier_covers_every_arm() {
        // Zhur-Taa Druid's live shape: opponent-scoped each-player damage → benefit.
        assert!(effect_benefits_trigger_controller(
            &Effect::DamageEachPlayer {
                amount: fixed(1),
                player_filter: PlayerFilter::Opponent,
            }
        ));
        // Self-scoped each-player damage (would hit the tapper) → pass.
        assert!(!effect_benefits_trigger_controller(
            &Effect::DamageEachPlayer {
                amount: fixed(1),
                player_filter: PlayerFilter::Controller,
            }
        ));
        // "Deals damage to target opponent" (opponent player scope) → benefit.
        assert!(effect_benefits_trigger_controller(&Effect::DealDamage {
            amount: fixed(1),
            target: opponent_scope(),
            damage_source: None,
            excess: None,
        }));
        // Manabarbs: damage to the TRIGGERING player (the tapper) → pass.
        assert!(!effect_benefits_trigger_controller(&Effect::DealDamage {
            amount: fixed(1),
            target: TargetFilter::TriggeringPlayer,
            damage_source: None,
            excess: None,
        }));
        // Controller-scoped damage (the tapper harms themselves) → pass.
        assert!(!effect_benefits_trigger_controller(&Effect::DealDamage {
            amount: fixed(1),
            target: TargetFilter::Controller,
            damage_source: None,
            excess: None,
        }));
        // Opponent-scoped life loss → benefit.
        assert!(effect_benefits_trigger_controller(&Effect::LoseLife {
            amount: fixed(1),
            target: Some(opponent_scope()),
        }));
        // Untargeted life loss (defaults to the resolving player) → pass.
        assert!(!effect_benefits_trigger_controller(&Effect::LoseLife {
            amount: fixed(1),
            target: None,
        }));
        // Controller life gain → benefit.
        assert!(effect_benefits_trigger_controller(&Effect::GainLife {
            amount: fixed(1),
            player: TargetFilter::Controller,
        }));
        // Opponent life gain → pass (helps the opponent, not the tapper).
        assert!(!effect_benefits_trigger_controller(&Effect::GainLife {
            amount: fixed(1),
            player: opponent_scope(),
        }));
        // Unknown / unmodeled shapes default-pass (documented catch-all).
        assert!(!effect_benefits_trigger_controller(
            &Effect::Unimplemented {
                name: "runaway growth".to_string(),
                description: None,
            }
        ));
        assert!(!effect_benefits_trigger_controller(
            &Effect::GenericEffect {
                static_abilities: vec![],
                duration: None,
                target: None,
            }
        ));
    }

    fn taps_for_mana_trigger_with(effect: Effect) -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::TapsForMana)
            .execute(AbilityDefinition::new(AbilityKind::Database, effect))
            .valid_card(TargetFilter::SelfRef)
    }

    #[test]
    fn trigger_chain_benefits_walks_sub_ability_and_requires_execute() {
        // No execute chain → benefits no one.
        assert!(!trigger_chain_benefits_controller(&TriggerDefinition::new(
            TriggerMode::TapsForMana
        )));
        // Top-level beneficial effect.
        assert!(trigger_chain_benefits_controller(
            &taps_for_mana_trigger_with(Effect::DamageEachPlayer {
                amount: fixed(1),
                player_filter: PlayerFilter::Opponent,
            })
        ));
        // Beneficial effect nested in a sub_ability link is still found.
        let mut nested = AbilityDefinition::new(
            AbilityKind::Database,
            Effect::Unimplemented {
                name: "noop".to_string(),
                description: None,
            },
        );
        nested.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::GainLife {
                amount: fixed(2),
                player: TargetFilter::Controller,
            },
        )));
        assert!(trigger_chain_benefits_controller(
            &TriggerDefinition::new(TriggerMode::TapsForMana).execute(nested)
        ));
    }

    #[test]
    fn is_non_mana_tap_trigger_excludes_pure_mana_and_wrong_mode() {
        // Non-mana TapsForMana (Zhur-Taa) → true.
        assert!(is_non_mana_tap_trigger(&taps_for_mana_trigger_with(
            Effect::DamageEachPlayer {
                amount: fixed(1),
                player_filter: PlayerFilter::Opponent,
            }
        )));
        // Pure-mana TapsForMana (Wild Growth aura) → false (it IS a mana ability).
        let pure_mana =
            TriggerDefinition::new(TriggerMode::TapsForMana).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Additional,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            ));
        assert!(!is_non_mana_tap_trigger(&pure_mana));
        // ManaAdded with a non-mana effect → true.
        let mana_added =
            TriggerDefinition::new(TriggerMode::ManaAdded).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: fixed(1),
                    player: TargetFilter::Controller,
                },
            ));
        assert!(is_non_mana_tap_trigger(&mana_added));
        // Wrong mode (a beneficial effect on a non-tap trigger) → false.
        let wrong_mode =
            TriggerDefinition::new(TriggerMode::ChangesZone).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DamageEachPlayer {
                    amount: fixed(1),
                    player_filter: PlayerFilter::Opponent,
                },
            ));
        assert!(!is_non_mana_tap_trigger(&wrong_mode));
    }

    #[test]
    fn beneficial_sources_scan_scopes_to_controller() {
        let mut state = GameState::new_two_player(42);
        // P0's Zhur-Taa-shaped dork.
        let dork = create_object(
            &mut state,
            CardId(800),
            PlayerId(0),
            "Beneficial Dork".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&dork)
            .unwrap()
            .trigger_definitions
            .push(taps_for_mana_trigger_with(Effect::DamageEachPlayer {
                amount: fixed(1),
                player_filter: PlayerFilter::Opponent,
            }));
        // An opponent-controlled copy of the same shape must NOT appear for P0.
        let opp_dork = create_object(
            &mut state,
            CardId(801),
            PlayerId(1),
            "Opponent Dork".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_dork)
            .unwrap()
            .trigger_definitions
            .push(taps_for_mana_trigger_with(Effect::DamageEachPlayer {
                amount: fixed(1),
                player_filter: PlayerFilter::Opponent,
            }));

        let sources = beneficial_non_mana_tap_trigger_sources(&state, PlayerId(0));
        assert_eq!(sources, vec![dork], "only P0's controlled source is listed");
    }

    /// CR 205.4b + CR 205.4g: `source_is_snow` reads the LAYERED (post-CR-613)
    /// supertype set, not the printed base set. A permanent whose Snow supertype
    /// was continuously granted (base lacks it, layered set contains it) is a
    /// snow source. This is the discriminating fixture for "reads layered, not
    /// base": a `base_card_types`-reading implementation would return `false`.
    #[test]
    fn source_is_snow_reads_layered_not_base_supertypes() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(910),
            PlayerId(0),
            "Continuously-Snowed Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        // Printed (base) supertypes deliberately LACK Snow ...
        obj.base_card_types = obj.card_types.clone();
        // ... while the layered result (post-CR-613 continuous grant) HAS Snow.
        obj.card_types.supertypes.push(Supertype::Snow);

        // Non-vacuity guard: the base set must genuinely lack Snow, so a
        // base-reading implementation would fail this test.
        assert!(
            !obj.base_card_types.supertypes.contains(&Supertype::Snow),
            "fixture invalid: base supertypes must NOT contain Snow for this to \
             discriminate layered vs. base reads",
        );
        assert!(
            source_is_snow(&state, id),
            "a permanent continuously granted the Snow supertype is a snow source \
             (CR 205.4g); source_is_snow must read the layered supertype set",
        );
    }

    /// A missing object / `ObjectId(0)` sentinel is not a snow source.
    #[test]
    fn source_is_snow_false_for_missing_object() {
        let state = GameState::new_two_player(42);
        assert!(
            !source_is_snow(&state, ObjectId(0)),
            "the ObjectId(0) sentinel (and any absent object) is not a snow source",
        );
    }
}
