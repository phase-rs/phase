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

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr, TargetFilter,
};
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaPip, ManaType};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use crate::types::TriggerMode;

use super::combat;
use super::mana_abilities;
use super::mana_payment;
use super::restrictions;

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
    /// (`QuantityExpr::Ref` / `HalfRounded` / etc.) amount — treated as
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

    /// CR 605.3b: Cost contains `AbilityCost::Sacrifice { target: SelfRef }`.
    /// The source leaves the battlefield on activation, so the activation is
    /// never rewindable.
    ///
    /// Examples: Treasure, Gold, Lotus Petal.
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
    /// CR 605.3b — classification of this activation's penalty axis.
    /// Single source of truth for auto-tap prioritization, `max_x_value`
    /// free-producer gating, AI life-cost scoring, and `UntapLandForMana`
    /// undoability. Constructed by `scan_mana_abilities` via
    /// `mana_ability_penalty`.
    pub penalty: ManaSourcePenalty,
    /// CR 605.3b + CR 106.1a: Complete pre-chosen multi-mana sequence for
    /// `ManaProduction::ChoiceAmongCombinations` sources (Shadowmoor/Eventide
    /// filter lands). `None` for all other sources. When `Some`, a single
    /// activation of this ability produces **every** mana type listed here,
    /// atomically — the shard assigner must treat all combos sharing the same
    /// `(object_id, ability_index)` as alternatives (pick at most one).
    pub atomic_combination: Option<Vec<ManaType>>,
}

/// Check whether an ability cost includes a tap component.
/// Matches both `AbilityCost::Tap` and `Composite` costs containing `Tap`.
pub(crate) fn has_tap_component(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Tap) => true,
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|c| matches!(c, AbilityCost::Tap))
        }
        _ => false,
    }
}

/// Check whether an ability cost requires sacrificing the source permanent.
fn cost_requires_sacrifice(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Composite { costs }) => costs.iter().any(|c| {
            matches!(
                c,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
        }),
        _ => false,
    }
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
/// non-`Fixed` shape (Ref / HalfRounded / Offset / Multiply / …) returns
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
    if cost_requires_sacrifice(&ability.cost) {
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
    land_mana_options(state, object_id, controller, true)
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
            // CR 106.1a: Display the chosen color when available; otherwise
            // surface as a generic "any color" choice across WUBRG so the
            // frame still renders something meaningful.
            ManaProduction::ChosenColor { .. } => match obj.chosen_color() {
                Some(color) => push(&mut pips, ManaPip::Color(color)),
                None => push(&mut pips, ManaPip::OneOfColors(ManaColor::ALL.to_vec())),
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
                    state, None, controller, object_id, filter,
                );
                if !colors.is_empty() {
                    push(&mut pips, ManaPip::CombinationOfColors(colors));
                }
            }
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
    // CR 602.5a + CR 302.6: Creatures with summoning sickness cannot activate tap abilities.
    if combat::has_summoning_sickness(obj) {
        return Vec::new();
    }
    scan_mana_abilities(state, obj, object_id, controller, true)
}

fn land_mana_options(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
    require_untapped: bool,
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
    // cannot activate tap abilities the turn they enter the battlefield.
    if require_untapped && combat::has_summoning_sickness(obj) {
        return Vec::new();
    }

    let mut options = scan_mana_abilities(state, obj, object_id, controller, require_untapped);

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
                penalty: ManaSourcePenalty::None,
                atomic_combination: None,
            });
        }
    }

    options
}

/// Scan an object's abilities for activated mana abilities with a tap cost component.
/// Type-agnostic — works for lands, creatures, artifacts, etc.
fn scan_mana_abilities(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    object_id: ObjectId,
    controller: PlayerId,
    require_current_payability: bool,
) -> Vec<ManaSourceOption> {
    let mut options = Vec::new();
    for (ability_index, ability) in obj.abilities.iter().enumerate() {
        if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
            continue;
        }
        if require_current_payability
            && !mana_abilities::can_activate_mana_ability_now(
                state,
                controller,
                object_id,
                ability_index,
                ability,
            )
        {
            continue;
        }
        if !has_tap_component(&ability.cost) {
            continue;
        }
        if !activation_condition_satisfied(state, controller, object_id, ability_index, ability) {
            continue;
        }

        let penalty = mana_ability_penalty(ability);
        for row in emit_source_rows(state, controller, object_id, ability_index, ability) {
            let option = ManaSourceOption {
                object_id,
                ability_index: Some(ability_index),
                mana_type: row.mana_type,
                penalty,
                atomic_combination: row.atomic_combination,
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
}

fn emit_source_rows(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    _ability_index: usize,
    ability: &AbilityDefinition,
) -> Vec<SourceRow> {
    let Effect::Mana { produced, .. } = &*ability.effect else {
        return Vec::new();
    };
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
                })
            })
            .collect(),
        _ => mana_options_from_production(state, controller, object_id, produced)
            .into_iter()
            .map(|mana_type| SourceRow {
                mana_type,
                atomic_combination: None,
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
        // CR 106.1a: Resolve the source permanent's chosen color. If the
        // permanent has no chosen color yet (e.g., still in library / sideboard
        // preview), return empty so it isn't offered as a mana source.
        ManaProduction::ChosenColor { .. } => state
            .objects
            .get(&object_id)
            .and_then(|obj| obj.chosen_color())
            .map(|color| vec![mana_color_to_type(&color)])
            .unwrap_or_default(),
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
            super::effects::mana::distinct_colors_among_permanents(
                state, None, controller, object_id, filter,
            )
            .iter()
            .map(mana_color_to_type)
            .collect()
        }
        // CR 603.7c + CR 106.3: "add one mana of any type that land produced"
        // resolves only inside a triggered ability (TapsForMana). For the mana
        // source enumeration path (cost-payment auto-tap, direct activation),
        // there is no trigger context — this variant has no pre-resolution
        // option set and contributes nothing to mana-source analysis.
        ManaProduction::TriggerEventManaType => Vec::new(),
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
    for (object_id, obj) in state.objects.iter() {
        if obj.zone != Zone::Battlefield {
            continue;
        }
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
/// Reuses `match_taps_for_mana`'s `valid_card_matches` predicate — the same
/// single-authority match used at trigger-firing time — so the refund and
/// firing paths cannot drift.
pub(crate) fn aura_taps_for_mana_sources_for_land(
    state: &GameState,
    land_id: ObjectId,
    controller: PlayerId,
) -> Vec<ObjectId> {
    let mut sources = Vec::new();
    for (&object_id, obj) in state.objects.iter() {
        if obj.zone != Zone::Battlefield {
            continue;
        }
        if obj.controller != controller {
            continue;
        }
        for trigger in obj.trigger_definitions.iter_all() {
            if trigger.mode != TriggerMode::TapsForMana {
                continue;
            }
            // CR 605.1b: A `TapsForMana` trigger fires when its `valid_card`
            // resolves to the land that produced mana. `AttachedTo` is the
            // canonical aura-on-land shape; `SelfRef` covers the
            // self-tapping land case (already refunded by the land's own
            // `source_id`, but kept here for completeness).
            let synthetic_event = crate::types::events::GameEvent::ManaAdded {
                player_id: controller,
                mana_type: ManaType::Colorless,
                source_id: land_id,
                tapped_for_mana: true,
            };
            if super::trigger_matchers::match_taps_for_mana(
                &synthetic_event,
                trigger,
                object_id,
                state,
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
    for (object_id, obj) in state.objects.iter() {
        if obj.zone != Zone::Battlefield {
            continue;
        }
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
    use crate::types::ability::{AbilityDefinition, AbilityKind, ManaContribution, QuantityExpr};
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
        });
        assert_eq!(chosen, vec![ManaPip::OneOfColors(ManaColor::ALL.to_vec())]);

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
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
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
            qty: crate::types::ability::QuantityRef::HandSize,
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
}
