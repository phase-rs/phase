//! Discard-matters feature — structural detection of a deck that discards its
//! OWN cards on purpose, as fuel for a payoff engine.
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `Effect::Discard { count, target, .. }` at
//!   `crates/engine/src/types/ability.rs:11384` — the rich enabler form
//!   (`QuantityExpr` count, `CardSelectionMode`, unless-filter).
//! - `Effect::DiscardCard { count, target }` at `ability.rs:10311` — the older
//!   simple enabler form (`u32` count). BOTH are live in the resolver
//!   (`game/effects/discard.rs` and `game/effects/mod.rs`), so a classifier
//!   that reads only one silently misses part of the class.
//! - `AbilityCost::Discard { count, .. }` at `ability.rs:8309` — the rummaging
//!   class (Wild Mongrel / Anje / flashback) spells its discard as a COST, not
//!   an effect, so the source predicate reads both axes. Nesting is resolved
//!   through the engine's `AbilityDefinition::cost_categories()` authority.
//! - `TriggerMode::Discarded` at `crates/engine/src/types/triggers.rs:321` and
//!   `TriggerMode::DiscardedAll` at `triggers.rs:322` (CR 701.9) — the payoffs.
//! - `TriggerDefinition.valid_target` — used to keep only YOUR-discard engines,
//!   excluding the "whenever an opponent discards" punisher shape.
//!
//! No parser remediation required — every axis is expressible over existing
//! typed AST.
//!
//! ## Why this axis exists
//!
//! CR 701.9: a deck built on Archfiend of Ifnir, Bone Miser, Waste Not or
//! Containment Construct *wants* to discard — each card pitched is a repeatable
//! value trigger. The AI has the opposite instinct: discarding is a cost, and
//! `card_advantage` scores the card leaving hand as a loss. Nothing values the
//! trigger it fires, so a deck whose entire engine is self-discard reads as
//! having no plan at all.
//!
//! ## Boundary with `hand_disruption`
//!
//! `hand_disruption` scores making an OPPONENT discard (`Effect::Discard` /
//! `Effect::DiscardCard` scoped to `TargetFilter::Opponent`) — stripping their
//! resources. This axis is the disjoint half: YOUR OWN discard
//! (`TargetFilter::Controller`) as fuel. The two never read the same card,
//! because the scope that qualifies here is exactly the one that disqualifies
//! there.
//!
//! ## Boundary with `cycling_discipline`
//!
//! `CyclingDisciplinePolicy` governs *when to pay a cycling cost* — patience
//! about spending a card for a replacement draw. That is a cost-discipline
//! question about one activation. This axis is a deck-composition question:
//! does a discard EVENT fire an engine on the battlefield? `TriggerMode::
//! CycledOrDiscarded` is deliberately NOT read here — a cycling trigger is
//! `cycling_discipline`'s subject, and counting it would double-score the same
//! card across two policies with different intents. Only the discard-specific
//! modes qualify.

use engine::game::ability_utils::ability_definition_supported;
use engine::game::quantity::resolve_quantity;
use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, CostCategory, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

use crate::ability_chain::collect_scoped_effects;
pub(crate) use crate::ability_chain::AbilityScope;
use crate::features::commitment;

/// Commitment at or above which self-discard is a deliberate engine rather than
/// incidental rummaging. Gates `DiscardPayoffPolicy::activation`.
pub const DISCARD_MATTERS_FLOOR: f32 = 0.35;

/// CR 701.9: per-deck discard-matters classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities` and `CardFace.triggers` — never by card name.
#[derive(Debug, Clone, Default)]
pub struct DiscardMattersFeature {
    /// Cards that discard YOU cards — the enablers that feed the payoff engine.
    pub source_count: u32,
    /// Permanents carrying a "whenever you discard a card" engine trigger
    /// (CR 701.9), controller-scoped and not self-referential — the payoffs that
    /// make pitching cards actively good.
    pub payoff_count: u32,
    /// `0.0..=1.0` — how central discarding-as-a-payoff is to this deck.
    /// Consumed by `DiscardPayoffPolicy::activation` as the single scaling knob.
    pub commitment: f32,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> DiscardMattersFeature {
    if deck.is_empty() {
        return DiscardMattersFeature::default();
    }

    let mut source_count = 0u32;
    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // Deck-time: a modal card whose discard lives in a branch still counts as
        // an enabler for the archetype, so scan the full potential tree.
        if is_discard_source_parts(
            &face.abilities,
            AbilityScope::Potential,
            &DiscardQuantity::Any,
        ) {
            source_count = source_count.saturating_add(entry.count);
        }
        if is_discard_payoff_parts(&face.triggers) {
            payoff_count = payoff_count.saturating_add(entry.count);
        }
    }

    let commitment = compute_commitment(source_count, payoff_count, total_nonland);

    DiscardMattersFeature {
        source_count,
        payoff_count,
        commitment,
    }
}

/// Whether a discard instruction's COUNT must be established positive.
///
/// CR 701.9 + CR 107.1b: "discard N cards" resolves its quantity at resolution,
/// so a count of zero moves no card and emits no discard event — it fires no
/// "whenever you discard" engine. Deck classification and live candidate scoring
/// want different answers about that, so the requirement is a parameter of the
/// one classifier rather than a second forked copy of it.
pub(crate) enum DiscardQuantity<'a> {
    /// Deck-time: any discard instruction marks the card regardless of count. A
    /// "discard X" or "discard your hand" card is still an enabler for archetype
    /// classification — its count is unknowable at deck-build time.
    Any,
    /// Live candidate: the count must resolve to at least one card *now*.
    ///
    /// Delegates to the engine's `resolve_quantity` authority rather than
    /// re-deriving quantity semantics, so this agrees with the resolver by
    /// construction. That also yields the correct conservative behavior for an
    /// unbound `X`: it reads `cost_x_paid` off the source and falls back to 0
    /// when X has not been announced yet, so an unbound dynamic discard stays
    /// neutral until it is known positive.
    ResolvesPositive {
        state: &'a GameState,
        controller: PlayerId,
        source: ObjectId,
    },
}

impl DiscardQuantity<'_> {
    /// CR 701.9: does this discard move at least one card under this requirement?
    fn expr_is_satisfied(&self, count: &QuantityExpr) -> bool {
        match self {
            DiscardQuantity::Any => true,
            DiscardQuantity::ResolvesPositive {
                state,
                controller,
                source,
            } => resolve_quantity(state, count, *controller, *source) >= 1,
        }
    }

    /// The `Effect::DiscardCard` sibling carries a plain `u32`, so its count is
    /// already concrete and needs no game state to settle.
    fn fixed_is_satisfied(&self, count: u32) -> bool {
        match self {
            DiscardQuantity::Any => true,
            DiscardQuantity::ResolvesPositive { .. } => count >= 1,
        }
    }
}

/// CR 701.9: the abilities discard YOU one or more cards — a repeatable enabler
/// for the payoff engine. Parts-based so it classifies both a deck-time
/// `CardFace.abilities` slice and the action's runtime effect chain.
///
/// The caller chooses the `scope`: `Potential` for deck-time (a modal discard
/// mode still marks the card), `Unconditional` for a live candidate before its
/// mode is selected (CR 700.2 — a modal "choose one — discard / …" must NOT be
/// credited a discard until that mode is actually chosen).
///
/// Both `Effect` spellings are read. They are separate variants with separate
/// resolver paths, and real cards use each, so classifying only the richer one
/// would drop half the class.
pub(crate) fn is_discard_source_parts<'a>(
    abilities: impl IntoIterator<Item = &'a AbilityDefinition>,
    scope: AbilityScope,
    quantity: &DiscardQuantity<'_>,
) -> bool {
    abilities.into_iter().any(|ability| {
        ability_cost_discards(ability, scope, quantity)
            || collect_scoped_effects(ability, scope)
                .iter()
                .any(|effect| match effect {
                    Effect::Discard { target, count, .. } => {
                        discards_controller(target) && quantity.expr_is_satisfied(count)
                    }
                    Effect::DiscardCard { target, count } => {
                        discards_controller(target) && quantity.fixed_is_satisfied(*count)
                    }
                    _ => false,
                })
    })
}

/// CR 118.3 + CR 601.2h: does PAYING this ability's cost discard a card?
///
/// The rummaging class this axis exists for — Wild Mongrel, Anje Falkenrath,
/// Faithless Looting's flashback — spells the discard as an `AbilityCost`, not as
/// an `Effect`. Reading only the effect chain classifies none of them, so the
/// shared source predicate has to look at both axes.
///
/// A discard COST has no target filter because it is always paid by the
/// activating player, i.e. by you — there is no opponent-scoped cost to exclude
/// the way there is for `Effect::Discard`.
fn ability_cost_discards(
    ability: &AbilityDefinition,
    scope: AbilityScope,
    quantity: &DiscardQuantity<'_>,
) -> bool {
    // Cheap gate through the engine's own cost-category authority, which already
    // flattens `Composite`/`OneOf` nesting. If it says no discard exists
    // anywhere in the tree, there is nothing to walk.
    if !ability.cost_categories().contains(&CostCategory::Discards) {
        return false;
    }
    ability
        .cost
        .as_ref()
        .is_some_and(|cost| cost_discards(cost, scope, quantity))
}

/// Walk a cost tree for a discard the payer will actually make.
///
/// The `scope` distinction is the same one the effect walk uses, applied to
/// costs: `Potential` asks "could paying this discard a card?", `Unconditional`
/// asks "will it?". That matters for `AbilityCost::OneOf` (CR 118.12a) — a
/// "discard a card OR pay 2 life" cost is a discard the deck can plan around,
/// but NOT one a live candidate is committed to, so crediting it at the live
/// seam would score a discard the player may never make.
fn cost_discards(cost: &AbilityCost, scope: AbilityScope, quantity: &DiscardQuantity<'_>) -> bool {
    match cost {
        AbilityCost::Discard { count, .. } => quantity.expr_is_satisfied(count),
        // CR 601.2h: every component of a composite cost is paid, so a discard
        // anywhere inside it is guaranteed.
        AbilityCost::Composite { costs } => costs
            .iter()
            .any(|inner| cost_discards(inner, scope, quantity)),
        // CR 118.12a: only one branch is chosen. A discard is guaranteed only
        // when every legal branch discards; otherwise it is merely possible.
        AbilityCost::OneOf { costs } => {
            !costs.is_empty()
                && match scope {
                    AbilityScope::Potential => costs
                        .iter()
                        .any(|inner| cost_discards(inner, scope, quantity)),
                    AbilityScope::Unconditional => costs
                        .iter()
                        .all(|inner| cost_discards(inner, scope, quantity)),
                }
        }
        AbilityCost::PerCounter { base, .. } => cost_discards(base, scope, quantity),
        // Every other cost form: defer to the engine's category authority rather
        // than enumerating here. That is deliberate — a newly added discarding
        // cost variant is picked up automatically instead of being silently
        // dropped by a stale match arm, and the count check it skips can only
        // make this UNDER-credit, never over-credit.
        other => other.categories().contains(&CostCategory::Discards),
    }
}

/// CR 701.9: the triggers carry a "whenever you discard a card" engine — a
/// repeatable payoff. Parts-based so it classifies both a deck-time
/// `CardFace.triggers` slice and a live `GameObject.trigger_definitions`
/// iterator (the runtime trigger authority).
pub(crate) fn is_discard_payoff_parts<'a>(
    triggers: impl IntoIterator<Item = &'a TriggerDefinition>,
) -> bool {
    triggers.into_iter().any(is_discard_payoff_trigger)
}

/// Single-trigger structural classifier (mode + scope), exposed so the policy
/// can pair it with live per-turn firing eligibility per trigger entry.
pub(crate) fn is_discard_payoff_trigger(t: &TriggerDefinition) -> bool {
    // 1. Mode fires on a discard event (CR 701.9). `DiscardedAll` is the
    //    "discards their hand" shape (Anje / Containment Construct class) and is
    //    the same event class. `CycledOrDiscarded` is deliberately excluded —
    //    see the module docs' `cycling_discipline` boundary.
    if !matches!(t.mode, TriggerMode::Discarded | TriggerMode::DiscardedAll) {
        return false;
    }
    // 2. Your-discard only: "whenever an opponent discards" is a punisher for a
    //    different deck, not a reason for YOU to pitch cards.
    if !matches!(&t.valid_target, None | Some(TargetFilter::Controller)) {
        return false;
    }
    // 3. Exclude a self-referential "when this card is discarded" trigger —
    //    madness and Obsidian-Charmaw-style recursion fire from the card being
    //    discarded itself, not from a battlefield engine. Those are the
    //    enabler's own payoff, already priced by the card, and counting them
    //    would let a pile of madness cards masquerade as an engine base.
    if matches!(&t.valid_card, Some(TargetFilter::SelfRef)) {
        return false;
    }
    // 4. The payoff must resolve to a real effect. A missing execute or an
    //    unsupported one (`TriggerNoExecute` / `Effect::Unimplemented`) produces
    //    no value, so it is not an engine — the same shared support authority the
    //    live fireability preflight consults.
    t.execute
        .as_deref()
        .is_some_and(ability_definition_supported)
}

/// True when the discard instruction makes the controller (you) discard, not an
/// opponent. `TargetFilter::Any` is the serde default on both variants and is
/// NOT treated as "you": an unscoped discard is exactly the ambiguous case, and
/// crediting it would pull `hand_disruption`'s opponent-facing cards into this
/// axis.
fn discards_controller(target: &TargetFilter) -> bool {
    matches!(target, TargetFilter::Controller)
}

/// Calibration (computed, not asserted from intuition — the anchors below are
/// the values `compute_commitment` actually returns):
/// - Realistic Rakdos pitch shell — 10 self-discard outlets + 4 engines
///   (Archfiend of Ifnir / Bone Miser / Waste Not) over 36 nonland → **0.878**.
/// - A lighter build, 6 outlets + 2 engines → 0.481: still a real plan, still
///   above `DISCARD_MATTERS_FLOOR`.
///
/// Anti-calibration:
/// - Incidental rummaging, 2 outlets + 1 engine over 36 nonland → **0.196**,
///   comfortably below the floor.
/// - Outlets with no engine, or an engine with no outlet → 0.0.
///
/// Geometric mean over (source, payoff): BOTH pillars are mandatory. Discard
/// with no engine is pure card disadvantage — the AI is RIGHT to avoid it, and
/// this axis must not push it to. An engine with no outlet only ever fires on a
/// cleanup-step discard.
fn compute_commitment(source_count: u32, payoff_count: u32, total_nonland: u32) -> f32 {
    // ~18 self-discard outlets per 60 nonland is a fully-committed pitch shell.
    let source_density = (commitment::density_per_60(source_count, total_nonland) / 18.0).min(1.0);
    // ~8 engine payoffs per 60 nonland is a fully-committed payoff base. Engines
    // are scarcer than outlets, but not as scarce as a draw engine: several are
    // cheap enchantments/creatures a pitch deck runs in multiples.
    let payoff_density = (commitment::density_per_60(payoff_count, total_nonland) / 8.0).min(1.0);
    commitment::geometric_mean(&[source_density, payoff_density])
}
