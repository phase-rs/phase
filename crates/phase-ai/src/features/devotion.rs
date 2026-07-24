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
//!   Merchant's drain, Anax's power), Nykthos ramp, and X-cost reductions.
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
//! Merchant-style drains, and Nykthos ramp — 43 cards in the corpus read it.
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

use crate::ability_chain::collect_chain_effects;
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
    /// The color the deck is most devoted to among the colors its payoffs care
    /// about. `None` when the deck has no devotion payoff. The single color the
    /// policy scores pip contributions in.
    pub primary_color: Option<ManaColor>,
    /// Raw colored-pip count in `primary_color` across the deck's permanent
    /// faces (CR 700.5 counts permanents only).
    pub pip_count: u32,
    /// The highest `DevotionGE` threshold any god/gate reads in `primary_color`,
    /// or `None` when no threshold payoff reads it. Drives the policy's
    /// "this cast turns the god on" spike. Absence is modelled distinctly from a
    /// fabricated ceiling, mirroring `graveyard_types`.
    pub highest_threshold: Option<u32>,
    /// `0.0..=1.0` — how central devotion is. Consumed by
    /// `DevotionPolicy::activation` as the single scaling knob.
    pub commitment: f32,
    /// Names of detected payoffs. NOT used for classification — that already
    /// happened against the AST. Identity lookup only.
    pub payoff_names: Vec<String>,
}

/// A payoff's color demand: a fixed color set, or "whatever color you are most
/// devoted to" (Nykthos' `ChosenColor`), which makes every color relevant.
struct PayoffColors {
    fixed: Vec<ManaColor>,
    any_chosen: bool,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> DevotionFeature {
    if deck.is_empty() {
        return DevotionFeature::default();
    }

    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();
    // Per-color deck pip totals across permanent faces (index by ManaColor).
    let mut pip_totals = ColorTotals::default();
    // Which colors any payoff cares about, and the god thresholds per color.
    let mut demanded = PayoffColors {
        fixed: Vec::new(),
        any_chosen: false,
    };
    let mut thresholds = ColorThresholds::default();

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // CR 700.5: only permanents contribute devotion pips.
        if is_permanent_face(&face.card_type.core_types) {
            for color in ManaColor::ALL {
                let pips = face.mana_cost.count_colored_pips(Some(color)).max(0) as u32;
                pip_totals.add(color, pips.saturating_mul(entry.count));
            }
        }

        let gate = highest_devotion_gate(face);
        let scales = reads_devotion(face);
        if let Some((colors, threshold)) = &gate {
            for color in colors {
                thresholds.raise(*color, *threshold);
                demanded.fixed.push(*color);
            }
        }
        for colors in scaling_payoff_colors(face) {
            match colors {
                DevotionColors::Fixed(cols) => demanded.fixed.extend(cols),
                DevotionColors::ChosenColor => demanded.any_chosen = true,
            }
        }

        if gate.is_some() || scales {
            payoff_count = payoff_count.saturating_add(entry.count);
            payoff_names.push(face.name.clone());
        }
    }

    // The primary color is the deck's most-devoted color among the colors its
    // payoffs read. A `ChosenColor` payoff (Nykthos) makes every color eligible,
    // so the deck's own densest color wins.
    let primary_color = ManaColor::ALL
        .iter()
        .copied()
        .filter(|color| demanded.any_chosen || demanded.fixed.contains(color))
        .max_by_key(|color| pip_totals.get(*color));

    let (pip_count, highest_threshold) = match primary_color {
        Some(color) => (pip_totals.get(color), thresholds.get(color)),
        None => (0, None),
    };

    let commitment = compute_commitment(payoff_count, pip_count, total_nonland);

    DevotionFeature {
        payoff_count,
        primary_color,
        pip_count,
        highest_threshold,
        commitment,
        payoff_names,
    }
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

/// CR 205.2a: a face that can enter the battlefield contributes devotion.
/// Instants and sorceries never do (their pips are not "permanents you control").
fn is_permanent_face(core_types: &[CoreType]) -> bool {
    core_types.iter().any(|t| {
        matches!(
            t,
            CoreType::Creature
                | CoreType::Artifact
                | CoreType::Enchantment
                | CoreType::Planeswalker
                | CoreType::Land
                | CoreType::Battle
        )
    })
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
    for effect in collect_chain_effects(ability) {
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
    // effect variants. Mana-production reads (Nykthos ramp) and cost-reduction
    // self-discounts are intentionally NOT reached — see the module limitation.
    if let Some(expr) = effect.count_expr() {
        collect_devotion_colors_in_expr(expr, out);
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
/// `game::quantity::continuous_modification_dynamic_quantity`.
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
        _ => None,
    }
}

/// Fixed-size per-color accumulator — avoids a `HashMap` in a hot deck scan.
#[derive(Default)]
struct ColorTotals {
    white: u32,
    blue: u32,
    black: u32,
    red: u32,
    green: u32,
}

impl ColorTotals {
    fn add(&mut self, color: ManaColor, n: u32) {
        let slot = self.slot(color);
        *slot = slot.saturating_add(n);
    }
    fn get(&self, color: ManaColor) -> u32 {
        match color {
            ManaColor::White => self.white,
            ManaColor::Blue => self.blue,
            ManaColor::Black => self.black,
            ManaColor::Red => self.red,
            ManaColor::Green => self.green,
        }
    }
    fn slot(&mut self, color: ManaColor) -> &mut u32 {
        match color {
            ManaColor::White => &mut self.white,
            ManaColor::Blue => &mut self.blue,
            ManaColor::Black => &mut self.black,
            ManaColor::Red => &mut self.red,
            ManaColor::Green => &mut self.green,
        }
    }
}

/// Per-color god-threshold maxima. `None` slots distinguish "no gate in this
/// color" from a real threshold, so a scaling-only deck is never handed a
/// fabricated ceiling.
#[derive(Default)]
struct ColorThresholds {
    totals: ColorTotals,
    seen: ColorSeen,
}

#[derive(Default)]
struct ColorSeen {
    white: bool,
    blue: bool,
    black: bool,
    red: bool,
    green: bool,
}

impl ColorThresholds {
    fn raise(&mut self, color: ManaColor, threshold: u32) {
        let current = self.totals.get(color);
        *self.totals.slot(color) = current.max(threshold);
        match color {
            ManaColor::White => self.seen.white = true,
            ManaColor::Blue => self.seen.blue = true,
            ManaColor::Black => self.seen.black = true,
            ManaColor::Red => self.seen.red = true,
            ManaColor::Green => self.seen.green = true,
        }
    }
    fn get(&self, color: ManaColor) -> Option<u32> {
        let seen = match color {
            ManaColor::White => self.seen.white,
            ManaColor::Blue => self.seen.blue,
            ManaColor::Black => self.seen.black,
            ManaColor::Red => self.seen.red,
            ManaColor::Green => self.seen.green,
        };
        seen.then(|| self.totals.get(color))
    }
}
