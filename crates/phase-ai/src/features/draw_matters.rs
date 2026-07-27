//! Draw-matters feature — structural detection of a "whenever you draw" engine
//! deck.
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `Effect::Draw { count, target }` at `crates/engine/src/types/ability.rs:10108`
//!   — the card-draw enablers (scoped to `TargetFilter::Controller`, i.e. "you
//!   draw").
//! - `TriggerMode::Drawn` at `crates/engine/src/types/triggers.rs:319`
//!   (CR 121.1: a card was drawn) — the payoffs.
//! - `TriggerDefinition.valid_target` (`Option<TargetFilter>`) at `ability.rs`
//!   — used to keep only YOUR-draw engines (`None`/`Controller`), excluding the
//!   "whenever an opponent draws" punisher shape.
//!
//! No parser remediation required — every axis is expressible over existing
//! typed AST.
//!
//! ## Why this axis exists
//!
//! A deck built around a "whenever you draw a card" engine — The Locust God
//! (make an Insect), Psychosis Crawler / Niv-Mizzet (ping), Chulane — turns
//! every extra draw into a repeatable value trigger (CR 121.1). `card_advantage`
//! values *having* cards, but nothing values *triggering* the engine, so the AI
//! will not lean into extra draws when it has a payoff on the battlefield. This
//! axis lets a policy see that engine.
//!
//! ## Boundary with `card_advantage` / `spellslinger_prowess`
//!
//! `card_advantage` scores the card itself (~1 card-equivalent per draw);
//! this axis adds the *extra* value a draw carries when it also fires an engine
//! — the same split as `CyclingDisciplinePolicy` (patience) vs a payoff policy.
//! `spellslinger_prowess` counts spell-cast triggers; a draw event (CR 121.1) is
//! a disjoint trigger. A card can read on both axes — the overlap is intentional
//! and the axes stay independent.

use engine::game::DeckEntry;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter, TriggerDefinition};
use engine::types::card_type::CoreType;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_scoped_effects;
pub(crate) use crate::ability_chain::AbilityScope;
use crate::features::commitment;

/// Commitment at or above which "drawing matters" is a real plan for this deck
/// rather than incidental card advantage. Gates `DrawPayoffPolicy::activation`.
pub const DRAW_MATTERS_FLOOR: f32 = 0.35;

/// CR 121.1: per-deck draw-matters classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities` and `CardFace.triggers` — never by card name.
#[derive(Debug, Clone, Default)]
pub struct DrawMattersFeature {
    /// Cards that draw you extra cards (an `Effect::Draw` scoped to the
    /// controller) — the enablers that feed the payoff engine.
    pub source_count: u32,
    /// Permanents carrying a "whenever you draw a card" engine trigger
    /// (CR 121.1), controller-scoped and not self-referential — the payoffs that
    /// make extra draws actively good.
    pub payoff_count: u32,
    /// `0.0..=1.0` — how central drawing-as-a-payoff is to this deck. Consumed by
    /// `DrawPayoffPolicy::activation` as the single scaling knob.
    pub commitment: f32,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> DrawMattersFeature {
    if deck.is_empty() {
        return DrawMattersFeature::default();
    }

    let mut source_count = 0u32;
    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // Deck-time: a modal card whose draw lives in a branch still counts as a
        // draw enabler for the archetype, so scan the full potential tree — plus
        // ETB "cantrip" triggers (Elvish Visionary), which the live policy also
        // credits via `CastFacts::immediate_etb_triggers`.
        if is_draw_source_parts(&face.abilities, AbilityScope::Potential)
            || is_etb_draw_source(&face.triggers)
        {
            source_count = source_count.saturating_add(entry.count);
        }
        if is_draw_payoff_parts(&face.triggers) {
            payoff_count = payoff_count.saturating_add(entry.count);
        }
    }

    let commitment = compute_commitment(source_count, payoff_count, total_nonland);

    DrawMattersFeature {
        source_count,
        payoff_count,
        commitment,
    }
}

/// CR 121.1: the abilities draw YOU one or more cards — a repeatable enabler for
/// the payoff engine. Parts-based so it classifies both a deck-time
/// `CardFace.abilities` slice and the action's runtime effect chain
/// (`CastFacts::primary_effects` / the activated ability). The caller chooses the
/// `scope`: `Potential` for deck-time (a modal draw mode still marks the card),
/// `Unconditional` for a live candidate before its mode is selected (CR 700.2 —
/// a modal "choose one — draw / …" must NOT be credited a draw until the draw
/// mode is actually chosen).
pub(crate) fn is_draw_source_parts<'a>(
    abilities: impl IntoIterator<Item = &'a AbilityDefinition>,
    scope: AbilityScope,
) -> bool {
    abilities.into_iter().any(|ability| {
        collect_scoped_effects(ability, scope)
            .iter()
            .any(|effect| matches!(effect, Effect::Draw { target, .. } if draws_controller(target)))
    })
}

/// CR 121.1: the triggers carry a "whenever you draw a card" engine — a
/// repeatable payoff. Parts-based so it classifies both a deck-time
/// `CardFace.triggers` slice and a live `GameObject.trigger_definitions` iterator
/// (the runtime trigger authority).
pub(crate) fn is_draw_payoff_parts<'a>(
    triggers: impl IntoIterator<Item = &'a TriggerDefinition>,
) -> bool {
    triggers.into_iter().any(is_draw_payoff_trigger)
}

/// Single-trigger structural classifier (mode + scope), exposed so the policy
/// can pair it with live per-turn firing eligibility per trigger entry.
pub(crate) fn is_draw_payoff_trigger(t: &TriggerDefinition) -> bool {
    // 1. Mode fires on a draw event (CR 121.1).
    if !matches!(t.mode, TriggerMode::Drawn) {
        return false;
    }
    // 2. Your-draw only: "whenever an opponent draws" is a punisher for a
    //    different deck, not a reason for YOU to draw more.
    if !matches!(&t.valid_target, None | Some(TargetFilter::Controller)) {
        return false;
    }
    // 3. Exclude a self-referential "when this is drawn" trigger — that fires
    //    from hand on the card itself, not a battlefield engine.
    !matches!(&t.valid_card, Some(TargetFilter::SelfRef))
}

/// True when the draw effect draws the controller cards (you), not an opponent.
fn draws_controller(target: &TargetFilter) -> bool {
    matches!(target, TargetFilter::Controller)
}

/// CR 603.6a: the face carries a self-ETB "when this enters, draw a card"
/// trigger (Elvish Visionary) — the live policy credits these via
/// `CastFacts::immediate_etb_triggers`, so deck-time detection must count them
/// as draw sources too, or an ETB-cantrip deck is undercounted.
fn is_etb_draw_source(triggers: &[TriggerDefinition]) -> bool {
    triggers.iter().any(|t| {
        t.mode == TriggerMode::ChangesZone
            && t.destination == Some(Zone::Battlefield)
            && matches!(t.valid_card, Some(TargetFilter::SelfRef))
            && t.execute.as_deref().is_some_and(|execute| {
                collect_scoped_effects(execute, AbilityScope::Potential)
                    .iter()
                    .any(|e| matches!(e, Effect::Draw { target, .. } if draws_controller(target)))
            })
    })
}

/// Calibration: a dedicated draw engine deck (e.g. Izzet "draw-two": ~20 card-
/// draw sources + ~5 engines like The Locust God / Niv-Mizzet over ~36 nonland)
/// → commitment ≈ 0.85. Anti-calibration: a blue midrange deck that runs card
/// draw but no engine → below `DRAW_MATTERS_FLOOR`; an engine with no extra draw,
/// or draw with no engine → 0.0.
///
/// Geometric mean over (source, payoff): BOTH pillars are mandatory. Card draw
/// with no engine is just card advantage (`card_advantage` governs it); an engine
/// with no way to draw extra only triggers on the natural draw for turn.
fn compute_commitment(source_count: u32, payoff_count: u32, total_nonland: u32) -> f32 {
    // ~20 draw sources per 60 nonland is a fully-committed draw shell (card draw
    // is common, so this pillar saturates later than a keyword pillar).
    let source_density = (commitment::density_per_60(source_count, total_nonland) / 20.0).min(1.0);
    // ~5 engine payoffs per 60 nonland is a fully-committed payoff base.
    let payoff_density = (commitment::density_per_60(payoff_count, total_nonland) / 5.0).min(1.0);
    commitment::geometric_mean(&[source_density, payoff_density])
}
