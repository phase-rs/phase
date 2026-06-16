//! Lifegain-matters feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `Keyword::Lifelink` at `keywords.rs:487` (CR 702.15a): a lifelink source
//!   makes its controller gain life on damage.
//! - `Effect::GainLife { amount, player }` at `ability.rs` (CR 119.3): "you gain
//!   N life" when `player` resolves to the controller.
//! - `TriggerMode::LifeGained` at `triggers.rs:313` (CR 603.6a): "whenever you
//!   gain life, …" — the lifegain *payoff*.
//!
//! No parser remediation required — lifegain-matters cards classify structurally
//! using the existing typed AST; never by card name.
//!
//! Why this is not redundant with existing handling: `eval.rs::lifelink_mult`
//! gives a flat per-creature lifelink bonus and `redundancy_avoidance` actively
//! *penalizes* gaining life at a high life total — neither recognizes a deck
//! whose payoffs convert each life-gain event into advantage. This axis fills
//! that gap; the companion policy is payoff-gated so non-lifegain decks (and the
//! redundancy penalty) are unaffected.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, TargetFilter, TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::triggers::TriggerMode;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment floor below which `LifegainPayoffPolicy` opts out.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// CR 119 / CR 702.15a: Per-deck lifegain-matters classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.keywords`, `CardFace.abilities`, and `CardFace.triggers` — never by
/// card name. The companion `LifegainPayoffPolicy` consumes this to value
/// casting lifegain sources when the deck contains payoffs.
#[derive(Debug, Clone, Default)]
pub struct LifegainFeature {
    /// Cards that gain *you* life — lifelink, or a "you gain N life" effect.
    /// CR 702.15a / CR 119.3. The fuel a lifegain-matters deck runs on.
    pub source_count: u32,
    /// Cards that REWARD gaining life — a `LifeGained` trigger ("whenever you
    /// gain life, …"). CR 603.6a. The intent signal: a pile of incidental
    /// lifegain without payoffs is not a lifegain-MATTERS deck.
    pub payoff_count: u32,
    /// `0.0..=1.0` — how central the lifegain-matters plan is to this deck.
    /// Driven primarily by payoff density; sources are supporting fuel. Consumed
    /// by `LifegainPayoffPolicy::activation` as the scaling knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// lifegain sources and lifegain payoffs.
pub fn detect(deck: &[DeckEntry]) -> LifegainFeature {
    if deck.is_empty() {
        return LifegainFeature::default();
    }

    let mut source_count = 0u32;
    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }
        if is_lifegain_source(face) {
            source_count = source_count.saturating_add(entry.count);
        }
        if is_lifegain_payoff(face) {
            payoff_count = payoff_count.saturating_add(entry.count);
        }
    }

    // Payoffs are the intent signal; sources are fuel. Weights mirror the
    // calibrated artifacts axis: a *single* incidental payoff in a ~40–60-nonland
    // deck stays below `COMMITMENT_FLOOR` (≈0.12–0.18) — it takes roughly three
    // payoff-equivalents to activate. Sources contribute little on their own
    // (incidental lifelink/gain-life is common in non-lifegain decks), and the
    // policy is additionally payoff-gated so sources alone never activate it.
    let commitment = commitment::weighted_sum(&[
        (
            0.12,
            commitment::density_per_60(payoff_count, total_nonland),
        ),
        (
            0.03,
            commitment::density_per_60(source_count, total_nonland),
        ),
    ]);

    LifegainFeature {
        source_count,
        payoff_count,
        commitment,
    }
}

/// A lifegain *source*: lifelink, or an effect (in an ability or trigger chain)
/// that makes you gain life. CR 702.15a / CR 119.3.
fn is_lifegain_source(face: &CardFace) -> bool {
    if is_lifegain_source_parts(&face.keywords, &chain_effects(&face.abilities)) {
        return true;
    }
    // "At the beginning of your upkeep, you gain 1 life" and similar trigger-borne
    // sources. CR 603.6a.
    face.triggers.iter().any(is_lifegain_source_trigger)
}

/// A lifegain *payoff*: a `LifeGained` trigger ("whenever you gain life, …").
/// CR 603.6a.
fn is_lifegain_payoff(face: &CardFace) -> bool {
    face.triggers.iter().any(trigger_rewards_your_lifegain)
}

/// Single authority for the "these *parts* make a lifegain source" check —
/// lifelink (CR 702.15a) or a controller-scoped `GainLife` effect (CR 119.3).
/// Shared by deck-time `CardFace` detection ([`is_lifegain_source`]) and the
/// live-game `LifegainPayoffPolicy` so the two never drift. Operates on a
/// keyword slice + an effect slice so it applies equally to a `CardFace` and a
/// `GameObject`.
pub(crate) fn is_lifegain_source_parts(keywords: &[Keyword], effects: &[&Effect]) -> bool {
    keywords.contains(&Keyword::Lifelink) || effects.iter().any(effect_gains_you_life)
}

/// True when a trigger's executed effect chain makes the controller gain life.
/// Shared with the live policy so trigger-borne sources such as Soul Warden
/// variants are valued the same way they are detected at deck-analysis time.
pub(crate) fn is_lifegain_source_trigger(trigger: &TriggerDefinition) -> bool {
    trigger.execute.as_ref().is_some_and(|exec| {
        collect_chain_effects(exec)
            .iter()
            .any(effect_gains_you_life)
    })
}

/// True when a `LifeGained` trigger rewards the source controller's lifegain.
/// Opponent-only life-gain triggers punish or react to opponents; they are not
/// payoffs for the AI's own lifegain plan.
fn trigger_rewards_your_lifegain(trigger: &TriggerDefinition) -> bool {
    trigger.mode == TriggerMode::LifeGained
        && !filter_is_opponent_scoped(trigger.valid_target.as_ref())
}

fn filter_is_opponent_scoped(filter: Option<&TargetFilter>) -> bool {
    let Some(filter) = filter else {
        return false;
    };
    match filter {
        TargetFilter::Typed(typed) => matches!(typed.controller, Some(ControllerRef::Opponent)),
        TargetFilter::Or { filters } => filters.iter().all(|f| filter_is_opponent_scoped(Some(f))),
        TargetFilter::And { filters } => filters.iter().any(|f| filter_is_opponent_scoped(Some(f))),
        _ => false,
    }
}

/// True if the effect makes you (the controller) gain life. CR 119.3. Opponent-
/// scoped gain ("target opponent gains 2 life") does not count as your source.
fn effect_gains_you_life(effect: &&Effect) -> bool {
    matches!(
        effect,
        Effect::GainLife { player, .. } if target_filter_is_you(player)
    )
}

/// CR 119.3: "you gain N life" — the controller-scoped gain. A targeted/other
/// player gain ("target player/opponent gains life") is not a reliable source
/// of *your* lifegain, so only `Controller` counts.
fn target_filter_is_you(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::Controller)
}

/// Flatten a slice of abilities into the effects reachable through their
/// sub-ability chains (the effect set the source predicate inspects).
fn chain_effects(abilities: &[AbilityDefinition]) -> Vec<&Effect> {
    abilities.iter().flat_map(collect_chain_effects).collect()
}
