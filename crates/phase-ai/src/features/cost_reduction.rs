//! Cost-reduction feature — structural detection of a deck that discounts its
//! own spells (CR 601.2f).
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `StaticMode::ModifyCost { mode, amount, spell_filter, dynamic_count }` at
//!   `crates/engine/src/types/statics.rs:1017` — the reducer itself.
//! - `CostModifyMode::{Reduce, Raise, Minimum}` at `statics.rs:698`; only
//!   `Reduce` discounts (CR 601.2f), `Raise`/`Minimum` are the Thalia /
//!   Trinisphere tax shapes and are NOT this axis.
//! - `StaticDefinition.affected: Option<TargetFilter>` at
//!   `crates/engine/src/types/ability.rs:20740` — carries the caster scope
//!   (`TypedFilter.controller`), exactly as `collect_battlefield_cost_modifiers` reads it.
//! - `CardFace.static_abilities: Vec<StaticDefinition>` at `card.rs:162`;
//!   the runtime counterpart is `GameObject.static_definitions`.
//! - `ManaCost::Cost { generic, shards }` at
//!   `crates/engine/src/types/mana.rs:1714`.
//!
//! No parser remediation required — every axis is expressible over existing
//! typed AST. `features::mana_ramp` explicitly deferred this shape
//! ("`StaticMode::ModifyCost` is deliberately out of scope — cost reducers are
//! a follow-up feature"); this module is that follow-up.
//!
//! ## Why this axis exists
//!
//! A Goblin Electromancer / Baral / Foundry Inspector / Medallion effect is
//! acceleration that never taps for mana: every subsequent spell costs less for
//! as long as the permanent survives (CR 601.2f). The engine already *applies*
//! the discount when the AI casts, so the AI is never overcharged — but nothing
//! makes it *value deploying the reducer first*. `mana_ramp` only sees effects
//! that add mana (`Effect::Mana`, land-fetch, extra land drops), so a deck whose
//! entire acceleration plan is cost reduction reads as having no ramp at all.
//! This axis lets a policy see that plan.
//!
//! ## Boundary with `mana_ramp`
//!
//! `mana_ramp` measures mana *added* to the pool; this axis measures cost
//! *removed* from spells. The two are disjoint at the AST level (`Effect::Mana`
//! vs `StaticMode::ModifyCost`) and a card is never counted by both. A deck can
//! read high on both — Sol Ring plus Medallions is a real shell — and the axes
//! stay independent.

use engine::game::filter::{matches_target_filter_against_face_scoped, FaceControllerScope};
use engine::game::quantity::resolve_quantity;
use engine::game::DeckEntry;
use engine::types::ability::{QuantityExpr, StaticDefinition, TargetFilter};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::statics::CostModifyMode;

use crate::features::commitment;

/// Commitment at or above which discounting your own spells is a real plan for
/// this deck rather than one incidental Medallion. Gates
/// `CostReductionPolicy::activation`.
///
/// Calibrated so a shell with four two-mana reducers over ~36 nonland cards
/// (commitment ≈ 0.61) activates while a deck running two (≈ 0.43) does not —
/// see [`compute_commitment`].
pub const COST_REDUCTION_FLOOR: f32 = 0.45;

/// Reducer density (per 60 nonland) at which the engine pillar saturates. Ten
/// discount permanents per 60 nonland is a fully-committed cost-reduction base.
const REDUCER_SATURATION_PER_60: f32 = 10.0;

/// CR 601.2f: per-deck cost-reduction classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.static_abilities` — never by card name.
#[derive(Debug, Clone, Default)]
pub struct CostReductionFeature {
    /// Cards carrying a board-wide CR 601.2f reducer that applies to spells YOU
    /// cast — the engines. Excludes self-cost reductions ("this spell costs {1}
    /// less") and opponent-scoped taxes.
    pub reducer_count: u32,
    /// Summed generic-mana discount those reducers deliver per application.
    ///
    /// CR 118.7a: a generic cost reduction affects only the generic component of
    /// a cost, so the magnitude is the reduction's generic amount — not its full
    /// mana value.
    pub total_discount: u32,
    /// Nonland deck cards that at least one of those reducers actually discounts
    /// (its `spell_filter` admits them) — the spells the engines pay off on.
    pub discounted_count: u32,
    /// `0.0..=1.0` — how central discounting your own spells is to this deck.
    /// Consumed by `CostReductionPolicy::activation` as the single scaling knob.
    pub commitment: f32,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> CostReductionFeature {
    if deck.is_empty() {
        return CostReductionFeature::default();
    }

    let mut reducer_count = 0u32;
    let mut total_discount = 0u32;
    let mut total_nonland = 0u32;
    // One entry per reducing static, so coverage is measured against every
    // discount the deck can put on the battlefield.
    let mut spell_filters: Vec<Option<TargetFilter>> = Vec::new();

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        let discount = your_spell_discount_parts(&face.static_abilities);
        if discount == 0 {
            continue;
        }
        reducer_count = reducer_count.saturating_add(entry.count);
        total_discount = total_discount.saturating_add(discount.saturating_mul(entry.count));
        // Hoisted out of any per-copy loop: the filter set is about WHAT the
        // deck discounts, so each unique face contributes its filters once.
        for def in &face.static_abilities {
            if let Some(filter) = your_spell_discount_filter(def) {
                spell_filters.push(filter);
            }
        }
    }

    // Second pass: how much of the deck do those filters actually admit? A
    // reducer whose filter matches nothing this deck plays is not an engine.
    let mut discounted_count = 0u32;
    if !spell_filters.is_empty() {
        for entry in deck {
            let face = &entry.card;
            if face.card_type.core_types.contains(&CoreType::Land) {
                continue;
            }
            if spell_filters
                .iter()
                .any(|filter| filter_admits_face(filter.as_ref(), face))
            {
                discounted_count = discounted_count.saturating_add(entry.count);
            }
        }
    }

    let commitment = compute_commitment(reducer_count, discounted_count, total_nonland);

    CostReductionFeature {
        reducer_count,
        total_discount,
        discounted_count,
        commitment,
    }
}

/// CR 601.2f: total per-application generic discount these statics give to
/// spells YOU cast. `0` means "not a cost-reduction engine".
///
/// Parts-based so it classifies both a deck-time `CardFace.static_abilities`
/// slice and a live `GameObject.static_definitions` slice — the two carry the
/// same `StaticDefinition` shape under different field names.
pub(crate) fn your_spell_discount_parts<'a>(
    statics: impl IntoIterator<Item = &'a StaticDefinition>,
) -> u32 {
    statics
        .into_iter()
        .filter_map(your_spell_discount)
        .fold(0u32, u32::saturating_add)
}

/// CR 601.2f: the generic discount this one static gives to spells you cast, or
/// `None` when it is not a board-wide reduction of your own spells.
///
/// Eligibility is NOT re-derived here. `StaticDefinition::board_wide_cost_modifier`
/// is the engine's single structural authority — the same one
/// `casting::collect_battlefield_cost_modifiers` consumes at cast time — so
/// `Minimum`, `SelfRef` (CR 113.6) and the caster scope are settled in one place
/// and cannot drift between deck analysis and the resolver.
fn your_spell_discount(def: &StaticDefinition) -> Option<u32> {
    let modifier = def.board_wide_cost_modifier()?;

    // CR 601.2f: `Raise` (Thalia) is a tax, not a discount. `Minimum`
    // (Trinisphere) never reaches here — the authority rejects it.
    if !matches!(modifier.mode, CostModifyMode::Reduce) {
        return None;
    }

    // CR 601.2f: a modifier scoped to opponents' spells is a discount handed to
    // the other side, not this deck's engine.
    if !modifier.caster_scope.admits_own_controller() {
        return None;
    }

    generic_discount(modifier.amount)
}

/// CR 118.7a: only the generic component of a cost can be reduced by a generic
/// reduction, so the magnitude is `generic`. A purely colored `amount` moves no
/// generic cost and is not an engine.
fn generic_discount(amount: &ManaCost) -> Option<u32> {
    let ManaCost::Cost { generic, .. } = amount else {
        return None;
    };
    (*generic > 0).then_some(*generic)
}

/// CR 601.2f: one discount a static would currently apply to its controller's
/// spells, paired with the spells it applies to.
pub(crate) struct LiveDiscount {
    /// Generic mana removed per application, `dynamic_count` already resolved.
    pub generic: u32,
    /// `None` discounts every spell its controller casts.
    pub spell_filter: Option<TargetFilter>,
}

/// CR 601.2f: the discounts `statics` would apply to their controller's spells
/// **right now**, with every state-dependent term of the casting authority's
/// eligibility resolved rather than assumed.
///
/// The deck-time [`your_spell_discount`] deliberately answers only the
/// structural half, because a deck list has no game state. This is the live
/// half, and it exists because the casting authority gates each modifier on two
/// things a structural read cannot see (`casting::collect_battlefield_cost_modifiers`):
///
/// * `condition` — an "as long as" / "during your turn" gate. A candidate still
///   in hand has not entered the battlefield, so a source-relative condition has
///   no truthful answer yet; per CR 601.2f the modifier simply would not apply
///   when it is false. This **fails off**: a conditional reducer earns no
///   deployment credit rather than credit the AI cannot bank on.
/// * `dynamic_count` — "for each [thing]" multiplier, resolved through the
///   engine's `resolve_quantity` authority so this agrees with the resolver by
///   construction. A multiplier of zero means the reducer currently discounts
///   nothing and is skipped entirely.
pub(crate) fn live_your_spell_discounts<'a>(
    state: &GameState,
    source: ObjectId,
    controller: PlayerId,
    statics: impl IntoIterator<Item = &'a StaticDefinition>,
) -> Vec<LiveDiscount> {
    statics
        .into_iter()
        .filter_map(|def| {
            let per_application = your_spell_discount(def)?;
            let modifier = def.board_wide_cost_modifier()?;
            // CR 601.2f: an unevaluable gate fails off (see above).
            if modifier.condition.is_some() {
                return None;
            }
            let multiplier = match modifier.dynamic_count {
                None => 1,
                Some(qty) => {
                    let expr = QuantityExpr::Ref { qty: qty.clone() };
                    u32::try_from(resolve_quantity(state, &expr, controller, source).max(0))
                        .unwrap_or(0)
                }
            };
            let generic = per_application.saturating_mul(multiplier);
            (generic > 0).then(|| LiveDiscount {
                generic,
                spell_filter: modifier.spell_filter.cloned(),
            })
        })
        .collect()
}

/// The `spell_filter` of a qualifying reducer — `Some(None)` for "discounts
/// every spell you cast", `None` when this static is not a qualifying reducer.
///
/// Separate from [`your_spell_discount`] because coverage is keyed on WHAT is
/// discounted while magnitude is keyed on HOW MUCH; folding both into one
/// return type would force every caller to destructure a tuple it half-ignores.
fn your_spell_discount_filter(def: &StaticDefinition) -> Option<Option<TargetFilter>> {
    your_spell_discount(def)?;
    Some(def.board_wide_cost_modifier()?.spell_filter.cloned())
}

/// CR 601.2f: would this reducer's `spell_filter` admit `face` as a discounted
/// spell? `None` is an unfiltered reducer — it discounts everything you cast.
///
/// Delegates wholly to the engine's context-free `CardFace` authority
/// (`matches_target_filter_against_face_scoped`), which owns CR 205 type
/// semantics AND every context-free `FilterProp` (mana value, color, keyword,
/// supertype) — so a color-, mana-value- or keyword-scoped reducer is matched
/// rather than silently discarded, and a property that needs live state fails
/// closed inside that authority instead of here.
///
/// `FaceControllerScope::AssumeOwn` because the caster axis was already settled
/// by `CostModifierCasterScope` in [`your_spell_discount`] — deck analysis asks
/// only about the analyzing player's own list, which is exactly how
/// `casting::collect_battlefield_cost_modifiers` splits the two checks.
fn filter_admits_face(filter: Option<&TargetFilter>, face: &CardFace) -> bool {
    match filter {
        None => true,
        Some(filter) => {
            matches_target_filter_against_face_scoped(face, filter, FaceControllerScope::AssumeOwn)
        }
    }
}

/// Calibration: an Izzet spells shell with four two-mana reducers (Goblin
/// Electromancer / Baral, "instant and sorcery spells you cast cost {1} less")
/// over ~36 nonland cards, ~20 of which are instants or sorceries →
/// reducer density 6.67/60 → 0.667, coverage 20/36 → 0.556, commitment ≈ 0.61.
/// A full artifact shell (eight reducers, ~29 of 36 nonland discounted) → ≈ 0.94.
///
/// Anti-calibration: two reducers over the same 36 nonland → ≈ 0.43, below
/// [`COST_REDUCTION_FLOOR`]; a deck with no CR 601.2f reducer → 0.0; a reducer
/// whose filter admits nothing the deck plays (a lone Semblance Anvil in a
/// creature-less shell) → coverage 0.0 → 0.0.
///
/// Geometric mean over (reducer, coverage): BOTH pillars are mandatory. Reducers
/// that discount nothing this deck casts are blanks, and spells with no reducer
/// are just spells — neither alone is a cost-reduction plan.
fn compute_commitment(reducer_count: u32, discounted_count: u32, total_nonland: u32) -> f32 {
    let reducer_density = (commitment::density_per_60(reducer_count, total_nonland)
        / REDUCER_SATURATION_PER_60)
        .min(1.0);
    // Coverage is already a fraction of the deck, so it is its own density.
    let coverage = if total_nonland == 0 {
        0.0
    } else {
        (discounted_count as f32 / total_nonland as f32).min(1.0)
    };
    commitment::geometric_mean(&[reducer_density, coverage])
}
