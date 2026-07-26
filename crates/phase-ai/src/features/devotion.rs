//! Devotion feature — mono-color pip-density payoff detection (CR 700.5).
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `StaticCondition::DevotionGE { colors: Vec<ManaColor>, threshold: u32 }` at
//!   `crates/engine/src/types/ability.rs:7070` — the Theros gods and
//!   devotion-gated statics ("as long as your devotion to black is less than
//!   five, Erebos isn't a creature", parsed as `RemoveType{Creature}` gated on
//!   `Not{DevotionGE}`).
//! - `QuantityRef::Devotion { colors: DevotionColors }` at
//!   `crates/engine/src/types/ability.rs:5574` — scaling payoffs (Gray
//!   Merchant's drain, Anax's power) and Nykthos-style mana ramp (`Effect::Mana`
//!   whose `ManaProduction` count is devotion).
//! - `DevotionColors::{Fixed(Vec<ManaColor>), ChosenColor}` at
//!   `crates/engine/src/types/ability.rs:1818`.
//! - Pip density reuses `ManaCost::count_colored_pips` (`types/mana.rs:1746`),
//!   the single CR 700.5 counting authority (hybrid `{G/W}{G/W}` counts as 2).
//!
//! No parser remediation required.
//!
//! ## Why this axis exists
//!
//! CR 700.5: devotion to a color is the number of that color's mana symbols
//! among the mana costs of permanents you control. It is the payoff currency
//! for the Theros gods (which are not creatures below their threshold), Gray
//! Merchant-style drains, and Nykthos-style ramp — 43 cards in the corpus read
//! it.
//! The AI's evaluation models mana value and board presence but not pip
//! density, so it will not prefer a double-pip permanent over an off-color one,
//! nor see that a god is one pip from turning on.
//!
//! ## Boundary with `tribal` / `mana_ramp`
//!
//! A mono-color devotion deck often looks tribal or ramp-flavoured, but the
//! resource is distinct: devotion counts *colored pips*, not creatures of a
//! type or mana sources. A five-Forest ramp deck has high `mana_ramp` and zero
//! devotion; a {B}{B}-heavy Gray Merchant deck is the reverse.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, DevotionColors, Effect, QuantityExpr, QuantityRef, StaticCondition,
    StaticDefinition,
};
use engine::types::card_type::CoreType;
use engine::types::mana::ManaColor;

use crate::ability_chain::{collect_scoped_effects, AbilityScope};
use crate::features::commitment;

/// Commitment at or above which the deck is genuinely a devotion payoff deck
/// rather than an incidental mono-color splash. Gates `DevotionPolicy::activation`.
pub const DEVOTION_FLOOR: f32 = 0.35;

/// CR 700.5 + CR 205.2: per-deck devotion classification.
///
/// Detection is structural over `CardFace.static_abilities`, `.triggers`,
/// `.abilities` and `.mana_cost` — never by card name.
#[derive(Debug, Clone, Default)]
pub struct DevotionFeature {
    /// Cards that pay off devotion — a `DevotionGE` gate (gods) or a
    /// `QuantityRef::Devotion` read (drains, ramp, X-scaling).
    pub payoff_count: u32,
    /// The color SET the deck is most devoted to among the sets its payoffs
    /// read — one color for a mono god (Erebos), two for a combined god
    /// (Athreos W+B, Xenagos R+G). Empty when the deck has no devotion payoff.
    /// The policy scores pip contributions against this set, counting a hybrid
    /// pip once (CR 700.5).
    pub primary_colors: Vec<ManaColor>,
    /// Colored-pip count toward `primary_colors` across the deck's permanent
    /// faces (CR 700.5 counts permanents only). Drives commitment.
    pub pip_count: u32,
    /// Every DISTINCT `DevotionGE` gate, each keyed on its EXACT color set — a
    /// two-color god's threshold is against combined devotion to both colors
    /// (CR 700.5), not either component. Each god turns on independently, so the
    /// policy rewards a cast that crosses ANY gate. Empty when the deck has no
    /// threshold payoff, so a scaling-only deck is never handed a fabricated gate.
    pub gates: Vec<DevotionGate>,
    /// `0.0..=1.0` — how central devotion is. Consumed by
    /// `DevotionPolicy::activation` as the single scaling knob.
    pub commitment: f32,
    /// Names of detected payoffs. NOT used for classification — that already
    /// happened against the AST. Identity lookup only.
    pub payoff_names: Vec<String>,
}

/// CR 700.5: a `DevotionGE` gate — a god that becomes a creature once devotion
/// to `colors` (combined, hybrids once) reaches `threshold`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevotionGate {
    pub colors: Vec<ManaColor>,
    pub threshold: u32,
}

/// CR 700.5: colored pips in `cost` that count toward devotion to `colors` —
/// each mana symbol counted once even when it is hybrid across two of the
/// colors (`{W/B}` counts once for W+B devotion). The single-cost analogue of
/// `engine::game::devotion::count_devotion` (which sums over the battlefield).
pub(crate) fn cost_devotion_pips(
    cost: &engine::types::mana::ManaCost,
    colors: &[ManaColor],
) -> u32 {
    let engine::types::mana::ManaCost::Cost { shards, .. } = cost else {
        return 0;
    };
    shards
        .iter()
        .filter(|shard| colors.iter().any(|c| shard.contributes_to(*c)))
        .count() as u32
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> DevotionFeature {
    if deck.is_empty() {
        return DevotionFeature::default();
    }

    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();
    // Permanent-face mana costs (with copy counts) so candidate-set totals are
    // computed SET-WISE below — a hybrid `{W/B}` shard counts once for a W+B set
    // (CR 700.5), never once per color as a sum of per-color buckets would.
    let mut permanent_costs: Vec<(&engine::types::mana::ManaCost, u32)> = Vec::new();
    // Every color SET a payoff demands, kept whole (a two-color god demands the
    // pair, not each color), plus whether a `ChosenColor` payoff makes every
    // color eligible (Nykthos).
    let mut demanded_sets: Vec<Vec<ManaColor>> = Vec::new();
    let mut any_chosen = false;
    let mut gates: Vec<DevotionGate> = Vec::new();

    let mut demand = |set: Vec<ManaColor>| {
        let set = normalize_colors(set);
        if !set.is_empty() && !demanded_sets.contains(&set) {
            demanded_sets.push(set);
        }
    };

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // CR 700.5 + CR 110.4: only permanents contribute devotion pips. Keep the
        // whole cost so each candidate set is scored set-wise (hybrids once).
        if face
            .card_type
            .core_types
            .iter()
            .any(|t| t.is_permanent_type())
        {
            permanent_costs.push((&face.mana_cost, entry.count));
        }

        let gate = highest_devotion_gate(face);
        let scales = reads_devotion(face);
        if let Some((colors, threshold)) = &gate {
            let colors = normalize_colors(colors.clone());
            demand(colors.clone());
            let candidate = DevotionGate {
                colors,
                threshold: *threshold,
            };
            if !gates.contains(&candidate) {
                gates.push(candidate);
            }
        }
        for colors in scaling_payoff_colors(face) {
            match colors {
                DevotionColors::Fixed(cols) => demand(cols),
                DevotionColors::ChosenColor => any_chosen = true,
            }
        }

        if gate.is_some() || scales {
            payoff_count = payoff_count.saturating_add(entry.count);
            payoff_names.push(face.name.clone());
        }
    }

    // Candidate primary sets: every demanded set, plus each single color when a
    // `ChosenColor` payoff (Nykthos) makes any color eligible. The primary is the
    // set the deck is most devoted to.
    let mut candidates = demanded_sets.clone();
    if any_chosen {
        for color in ManaColor::ALL {
            let single = vec![color];
            if !candidates.contains(&single) {
                candidates.push(single);
            }
        }
    }
    // CR 700.5 set-wise: a candidate set's deck devotion is the pips each
    // permanent face contributes to the SET as a whole (a hybrid symbol once),
    // summed over copies. This is the same counting `count_devotion` performs at
    // runtime — never a sum of independent per-color buckets, which would count a
    // `{W/B}` shard twice toward a W+B set and inflate commitment.
    let set_total = |set: &[ManaColor]| -> u32 {
        permanent_costs
            .iter()
            .map(|&(cost, count)| cost_devotion_pips(cost, set).saturating_mul(count))
            .sum()
    };
    let primary_colors = candidates
        .into_iter()
        .max_by_key(|set| set_total(set))
        .unwrap_or_default();
    let pip_count = set_total(&primary_colors);

    let commitment = compute_commitment(payoff_count, pip_count, total_nonland);

    DevotionFeature {
        payoff_count,
        primary_colors,
        pip_count,
        gates,
        commitment,
        payoff_names,
    }
}

/// CR 105.1: put a color set in canonical WUBRG order and drop duplicates so
/// sets and gates dedup reliably.
fn normalize_colors(mut colors: Vec<ManaColor>) -> Vec<ManaColor> {
    let order = |c: &ManaColor| {
        ManaColor::ALL
            .iter()
            .position(|x| x == c)
            .unwrap_or(usize::MAX)
    };
    colors.sort_by_key(order);
    colors.dedup();
    colors
}

/// Calibration: Mono-Black Devotion (Gray Merchant ×4, Erebos, ~30 permanents
/// averaging ~1.3 black pips → ~40 pips over 37 nonland) → commitment ≈ 0.90.
/// Anti-calibration: a two-color midrange deck with one off-color god and few
/// pips in its color → below `DEVOTION_FLOOR`; UW control → 0.0.
///
/// Geometric mean over (payoff, pip): BOTH pillars are mandatory. Pips with no
/// payoff is just a mono-color deck; a payoff with no pips never turns on.
fn compute_commitment(payoff_count: u32, pip_count: u32, total_nonland: u32) -> f32 {
    let payoff_density = (commitment::density_per_60(payoff_count, total_nonland) / 6.0).min(1.0);
    // ~30 pips per 60 nonland is a fully-committed mono-color devotion deck.
    let pip_density = (commitment::density_per_60(pip_count, total_nonland) / 30.0).min(1.0);
    commitment::geometric_mean(&[payoff_density, pip_density])
}

/// The highest `DevotionGE` gate on the face (the god threshold), with the
/// colors it reads. Walks the static-condition tree so a gate nested under
/// `Not` (Erebos: "isn't a creature unless devotion ≥ 5") is found. Gods carry
/// the gate on a static, never a trigger, so only statics are scanned.
fn highest_devotion_gate(face: &engine::types::card::CardFace) -> Option<(Vec<ManaColor>, u32)> {
    face.static_abilities
        .iter()
        .filter_map(|def| def.condition.as_ref())
        .filter_map(static_devotion_gate)
        .max_by_key(|(_, threshold)| *threshold)
}

fn static_devotion_gate(condition: &StaticCondition) -> Option<(Vec<ManaColor>, u32)> {
    match condition {
        StaticCondition::DevotionGE { colors, threshold } => Some((colors.clone(), *threshold)),
        StaticCondition::Not { condition } => static_devotion_gate(condition),
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => conditions
            .iter()
            .filter_map(static_devotion_gate)
            .max_by_key(|(_, threshold)| *threshold),
        _ => None,
    }
}

/// True when the face reads `QuantityRef::Devotion` anywhere in an ability,
/// trigger, or static magnitude (Gray Merchant, Nykthos, Anax, cost reducers).
fn reads_devotion(face: &engine::types::card::CardFace) -> bool {
    !scaling_payoff_colors(face).is_empty()
}

/// Every `DevotionColors` demand read by a `QuantityRef::Devotion` on the face.
fn scaling_payoff_colors(face: &engine::types::card::CardFace) -> Vec<DevotionColors> {
    let mut out = Vec::new();
    for ability in &face.abilities {
        collect_devotion_colors_in_ability(ability, &mut out);
    }
    for trigger in &face.triggers {
        if let Some(execute) = &trigger.execute {
            collect_devotion_colors_in_ability(execute, &mut out);
        }
    }
    for def in &face.static_abilities {
        collect_devotion_colors_in_static(def, &mut out);
    }
    out
}

fn collect_devotion_colors_in_ability(ability: &AbilityDefinition, out: &mut Vec<DevotionColors>) {
    // CR 700.5 deck-time classification: `AbilityScope::Potential` walks the
    // `else_ability`/modal branches too, so a card whose devotion payoff lives
    // only in one mode (a modal "choose one — ... equal to your devotion") is
    // still detected. Mirrors the poison feature's deck-time scan.
    for effect in collect_scoped_effects(ability, AbilityScope::Potential) {
        collect_devotion_colors_in_effect(effect, out);
    }
}

fn collect_devotion_colors_in_static(def: &StaticDefinition, out: &mut Vec<DevotionColors>) {
    for modification in &def.modifications {
        if let Some(expr) = continuous_modification_quantity(modification) {
            collect_devotion_colors_in_expr(expr, out);
        }
    }
}

fn collect_devotion_colors_in_effect(effect: &Effect, out: &mut Vec<DevotionColors>) {
    // `Effect::count_expr` is the engine's exhaustive authority for an effect's
    // magnitude `QuantityExpr` (drain amount, damage, token count, draw count,
    // …), so every count/amount-bearing payoff is covered without hand-listing
    // effect variants.
    if let Some(expr) = effect.count_expr() {
        collect_devotion_colors_in_expr(expr, out);
    }
    // CR 605.1a: `count_expr` deliberately returns `None` for `Effect::Mana`
    // because a mana effect has no count/amount magnitude — its `QuantityExpr`
    // lives inside the `ManaProduction`. That is the Nykthos / Nyx Lotus /
    // Karametra's Acolyte carrier ("add mana equal to your devotion"), so it is
    // dug out explicitly here.
    if let Effect::Mana { produced, .. } = effect {
        if let Some(expr) = mana_production_count(produced) {
            collect_devotion_colors_in_expr(expr, out);
        }
    }
}

/// CR 605.1a: the `QuantityExpr` count a mana-production carries, if any.
/// Enumerated without a wildcard so a new `ManaProduction` variant forces this
/// devotion scan to be reconsidered. `Fixed` / `Mixed` produce a statically
/// sized bundle and carry no dynamic count.
fn mana_production_count(
    production: &engine::types::ability::ManaProduction,
) -> Option<&QuantityExpr> {
    use engine::types::ability::ManaProduction as MP;
    match production {
        MP::Colorless { count }
        | MP::AnyOneColor { count, .. }
        | MP::AnyCombination { count, .. }
        | MP::ChosenColor { count, .. }
        | MP::OpponentLandColors { count }
        | MP::AnyCombinationOfObjectColors { count, .. }
        | MP::AnyTypeProduceableBy { count, .. }
        | MP::AnyInCommandersColorIdentity { count, .. }
        | MP::AnyOneColorAmongPermanents { count, .. } => Some(count),
        // No dynamic count: fixed/statically-sized bundles and choice-set forms.
        MP::Fixed { .. }
        | MP::Mixed { .. }
        | MP::ChoiceAmongExiledColors { .. }
        | MP::ChoiceAmongCombinations { .. }
        | MP::DistinctColorsAmongPermanents { .. }
        | MP::TriggerEventManaType => None,
    }
}

fn collect_devotion_colors_in_expr(expr: &QuantityExpr, out: &mut Vec<DevotionColors>) {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::Devotion { colors },
        } => out.push(colors.clone()),
        QuantityExpr::Ref { .. } | QuantityExpr::Fixed { .. } => {}
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => collect_devotion_colors_in_expr(inner, out),
        QuantityExpr::UpTo { max } => collect_devotion_colors_in_expr(max, out),
        QuantityExpr::Power { exponent, .. } => collect_devotion_colors_in_expr(exponent, out),
        QuantityExpr::Difference { left, right } => {
            collect_devotion_colors_in_expr(left, out);
            collect_devotion_colors_in_expr(right, out);
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for e in exprs {
                collect_devotion_colors_in_expr(e, out);
            }
        }
    }
}

/// The dynamic magnitude carried by a continuous modification, if any. Mirrors
/// `game::quantity::continuous_modification_dynamic_quantity` — enumerated
/// without a wildcard so a new `QuantityExpr`-carrying variant forces this
/// devotion scan to be reconsidered alongside the engine authority.
fn continuous_modification_quantity(
    m: &engine::types::ability::ContinuousModification,
) -> Option<&QuantityExpr> {
    use engine::types::ability::ContinuousModification as CM;
    match m {
        CM::SetDynamicPower { value }
        | CM::SetDynamicToughness { value }
        | CM::SetPowerDynamic { value }
        | CM::SetToughnessDynamic { value }
        | CM::AddDynamicPower { value }
        | CM::AddDynamicToughness { value }
        | CM::AddDynamicKeyword { value, .. } => Some(value),
        CM::AddCounterOnEnter { .. }
        | CM::SetStartingLoyalty { .. }
        | CM::CopyValues { .. }
        | CM::CopyChosen
        | CM::SetName { .. }
        | CM::SetTextName { .. }
        | CM::AddPower { .. }
        | CM::AddToughness { .. }
        | CM::SetPower { .. }
        | CM::SetToughness { .. }
        | CM::AddKeyword { .. }
        | CM::AddKeywordWithDerivedCost { .. }
        | CM::RemoveKeyword { .. }
        | CM::GrantAbility { .. }
        | CM::GrantAllActivatedAbilitiesOf { .. }
        | CM::GrantAllTriggeredAbilitiesOf { .. }
        | CM::GrantTrigger { .. }
        | CM::GrantReplacement { .. }
        | CM::RemoveAllAbilities
        | CM::AddType { .. }
        | CM::RemoveType { .. }
        | CM::AddSubtype { .. }
        | CM::RemoveSubtype { .. }
        | CM::SetCardTypes { .. }
        | CM::RemoveAllSubtypes { .. }
        | CM::AddAllCreatureTypes
        | CM::AddAllBasicLandTypes
        | CM::AddAllLandTypes
        | CM::AddChosenSubtype { .. }
        | CM::AddChosenColor { .. }
        | CM::RemoveChosenKeyword
        | CM::AddChosenKeyword
        | CM::SetColor { .. }
        | CM::AddColor { .. }
        | CM::AddStaticMode { .. }
        | CM::GrantStaticAbility { .. }
        | CM::SwitchPowerToughness
        | CM::AssignDamageFromToughness
        | CM::AssignDamageAsThoughUnblocked
        | CM::AssignNoCombatDamage
        | CM::ChangeController
        | CM::SetBasicLandType { .. }
        | CM::SetChosenBasicLandType
        | CM::SetChosenName
        | CM::RetainPrintedTriggerFromSource { .. }
        | CM::RetainPrintedAbilityFromSource { .. }
        | CM::RetainAllOtherAbilitiesFromSource
        | CM::AddSupertype { .. }
        | CM::RemoveSupertype { .. }
        | CM::RemoveManaCost => None,
    }
}
