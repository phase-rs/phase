//! Control feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `Effect::Counter { .. }` — counterspell detection
//!   (`crates/engine/src/types/ability.rs:2118`). CR 701.6.
//! - `Effect::Destroy { .. }` — destroy removal
//!   (`ability.rs:2106`). CR 701.8.
//! - `Effect::Bounce { destination: None | Some(Hand) }` — bounce removal
//!   (`ability.rs:2358-2363`).
//! - `Effect::ChangeZone { destination: Zone::Exile | Zone::Graveyard, .. }` —
//!   exile/graveyard removal (`ability.rs:2271`). CR 701.13.
//! - `Effect::DealDamage { .. }` — damage removal (`ability.rs:2085`). CR 120.3.
//! - `Effect::DestroyAll { .. }` (`ability.rs:2264`), `Effect::DamageAll { .. }`
//!   (`ability.rs:2252`), `Effect::ChangeZoneAll { .. }` (`ability.rs:2301`) —
//!   sweepers. These are **distinct** variants from spot removal — the parser
//!   emits them separately, so sweeper-vs-spot classification is unambiguous.
//! - `Effect::Draw { count }` (`ability.rs:2094`), `Effect::Dig { .. }`
//!   (`ability.rs:2310`) — card advantage variants. CR 120.1.
//! - `CoreType::Instant` / `CoreType::Sorcery` (`card_type.rs:74-77`).
//!   CR 117.1a + CR 304.1: instants can be cast any time the player has priority.
//! - `AbilityKind::Spell` (`ability.rs:3749`) — distinguishes spell effects from
//!   activated/triggered abilities.
//!
//! No parser remediation required — sweeper vs. spot removal is discriminable
//! via distinct `Effect` enum variants.
//!
//! No mulligan policy: control hands vary between "hold up interaction" and
//! "deploy finisher" — no single hand-shape signal is reliable enough to warrant
//! a mulligan policy analogous to ramp or landfall.

use engine::analysis::deck_signals::{self, DeckCard};
use engine::game::DeckEntry;

use crate::features::commitment;

pub use engine::analysis::deck_signals::control::{
    is_card_draw_parts, is_counterspell_parts, is_spot_removal_parts, is_sweeper_parts,
};

/// Per-deck control classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.abilities` — never by card name. Two orthogonal axes:
///
/// - `commitment`: overall control density (interaction + draw); drives
///   `SweeperTimingPolicy` and plan-layer tempo classification.
/// - `reactive_tempo`: fraction of interaction that is instant-speed; drives
///   `HoldManaUpForInteractionPolicy` independently of commitment. A
///   sorcery-heavy sweeper deck scores high commitment but near-zero
///   reactive_tempo — it should NOT hold mana up on the opponent's turn.
///
/// Counterspells are tautologically on instants (CR 117.1a + CR 304.1), so no
/// parallel `counterspell_instant_count` field is needed.
#[derive(Debug, Clone, Default)]
pub struct ControlFeature {
    /// Spell abilities with `Effect::Counter` — hard/soft counterspells.
    /// CR 701.6: countering cancels a spell without it resolving.
    pub counterspell_count: u32,
    /// Spells that destroy, bounce, exile, or deal fatal damage to a specific
    /// permanent — spot removal. Mutually exclusive with `sweeper_count`.
    pub spot_removal_count: u32,
    /// Spells with `DestroyAll`, `DamageAll`, or `ChangeZoneAll` — board wipes.
    /// Weighted 2× in the commitment formula because they answer more threats.
    pub sweeper_count: u32,
    /// Spells with `Effect::Draw` or `Effect::Dig` on an `AbilityKind::Spell`
    /// ability, excluding lands. CR 120.1: drawing cards replenishes hand.
    pub card_draw_count: u32,
    /// Non-land cards with `CoreType::Instant`. Flash creatures are NOT counted
    /// (flash is a keyword, not a core type — keyword detection is out of scope).
    pub instant_count: u32,
    /// Spot-removal cards that also have `CoreType::Instant` — tracked
    /// separately because `reactive_tempo` cares about instant-speed interaction
    /// specifically, not total removal density.
    pub spot_removal_instant_count: u32,
    /// Sweeper cards that also have `CoreType::Instant`.
    pub sweeper_instant_count: u32,
    /// `instant_count / max(total_nonland, 1)` — how reactive the deck is.
    pub reactive_instant_ratio: f32,
    /// `0.0..=1.0` overall control commitment. Tactical opt-in gate for
    /// `SweeperTimingPolicy`; also drives plan-layer `TempoClass::Control`.
    /// `COMMITMENT_FLOOR = 0.25` — midrange decks with incidental removal
    /// cross 0.1 easily, so the floor is stricter than landfall/ramp.
    pub commitment: f32,
    /// `0.0..=1.0` instant-speed interaction density. Drives
    /// `HoldManaUpForInteractionPolicy` independently of `commitment`.
    /// Counterspells are included in full (tautologically instant-speed).
    pub reactive_tempo: f32,
}

/// Commitment floor — below this level the deck is not meaningfully a control
/// deck. Stricter than landfall/ramp (0.1) because any deck has some removal.
pub const COMMITMENT_FLOOR: f32 = 0.25;

/// Reactive-tempo floor used by `HoldManaUpForInteractionPolicy`. Numerically
/// equal to `COMMITMENT_FLOOR` but conceptually independent — a sorcery-heavy
/// control deck has high commitment but near-zero reactive_tempo, and the
/// hold-mana-up bias only fires when reactive_tempo crosses this floor.
pub const REACTIVE_TEMPO_FLOOR: f32 = 0.25;

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and
/// classifies cards across control axes.
///
/// Planeswalker activated abilities are deliberately excluded from removal
/// counts — they are `AbilityKind::Activated`, not `AbilityKind::Spell`. A
/// planeswalker's `-N: Destroy target creature` is a loyalty activation, not a
/// control spell.
pub fn detect(deck: &[DeckEntry]) -> ControlFeature {
    let cards = DeckCard::from_deck_entries(deck);
    let signals = deck_signals::control::detect(&cards);
    let total_nonland = deck_signals::nonland_card_count(&cards);
    let reactive_instant_ratio = signals.instant_count as f32 / f32::max(total_nonland as f32, 1.0);

    // Commitment formula calibrated against competitive control archetypes.
    // Azorius Control baseline: ~4 counters, ~6 spot removal, ~2-3 sweepers,
    // ~6 draw spells, ~14 instants in a 60-card deck (~40 nonlands).
    // interaction_density at baseline: (4 + 6 + 6) / 40 = 0.40 → × 2.0 = 0.80.
    // draw_density at baseline: 6 / 40 = 0.15 → × 1.0 = 0.15.
    // commitment = clamp01(0.80 + 0.15) ≈ 0.95. ✓
    //
    // CR 701.6: counter. CR 701.8: destroy. CR 701.13: exile.
    let interaction_density = (signals.counterspell_count as f32
        + signals.spot_removal_count as f32
        + 2.0 * signals.sweeper_count as f32)
        / f32::max(total_nonland as f32, 1.0);
    let draw_density = signals.card_draw_count as f32 / f32::max(total_nonland as f32, 1.0);

    // Instant-only variant for reactive_tempo.
    // Counterspells are tautologically instant-speed (CR 117.1a + CR 304.1).
    let interaction_density_instant_only = (signals.counterspell_count as f32
        + signals.spot_removal_instant_count as f32
        + 2.0 * signals.sweeper_instant_count as f32)
        / f32::max(total_nonland as f32, 1.0);

    let commitment = commitment::weighted_sum(&[
        (2.0 / 60.0, interaction_density * 60.0),
        (1.0 / 60.0, draw_density * 60.0),
    ]);
    // reactive_tempo intentionally sums two overlapping signals:
    //   1. instant-only interaction density (counts each interaction spell once)
    //   2. raw instant ratio (counts every instant in the deck — interaction OR not)
    // The overlap is by design — an instant-speed counterspell legitimately
    // contributes to both the "I have interaction I can hold up" signal and the
    // "this is an instant-heavy deck" signal. Calibration absorbs the overlap.
    let reactive_tempo =
        clamp01(1.5 * interaction_density_instant_only + 1.0 * reactive_instant_ratio);

    ControlFeature {
        counterspell_count: signals.counterspell_count,
        spot_removal_count: signals.spot_removal_count,
        sweeper_count: signals.sweeper_count,
        card_draw_count: signals.card_draw_count,
        instant_count: signals.instant_count,
        spot_removal_instant_count: signals.spot_removal_instant_count,
        sweeper_instant_count: signals.sweeper_instant_count,
        reactive_instant_ratio,
        commitment,
        reactive_tempo,
    }
}

#[inline]
fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};

    fn face(name: &str, core_type: CoreType, effect: Effect) -> CardFace {
        CardFace {
            name: name.into(),
            card_type: CardType {
                core_types: vec![core_type],
                ..Default::default()
            },
            abilities: vec![AbilityDefinition::new(AbilityKind::Spell, effect)],
            ..Default::default()
        }
    }

    #[test]
    fn control_feature_fields_match_engine_signals() {
        let deck = vec![
            DeckEntry {
                card: face(
                    "Counter",
                    CoreType::Instant,
                    Effect::Counter {
                        target: TargetFilter::Any,
                        source_rider: None,
                        countered_spell_zone: None,
                    },
                ),
                count: 2,
            },
            DeckEntry {
                card: face(
                    "Removal",
                    CoreType::Instant,
                    Effect::Destroy {
                        target: TargetFilter::Any,
                        cant_regenerate: false,
                    },
                ),
                count: 3,
            },
            DeckEntry {
                card: face(
                    "Sweeper",
                    CoreType::Sorcery,
                    Effect::DestroyAll {
                        target: TargetFilter::Any,
                        cant_regenerate: false,
                    },
                ),
                count: 4,
            },
            DeckEntry {
                card: face(
                    "Draw",
                    CoreType::Sorcery,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 2 },
                        target: TargetFilter::Controller,
                    },
                ),
                count: 5,
            },
        ];
        let cards = DeckCard::from_deck_entries(&deck);
        let signals = deck_signals::control::detect(&cards);
        let total_nonland = deck_signals::nonland_card_count(&cards);
        let feature = detect(&deck);

        assert_eq!(feature.counterspell_count, signals.counterspell_count);
        assert_eq!(feature.spot_removal_count, signals.spot_removal_count);
        assert_eq!(feature.sweeper_count, signals.sweeper_count);
        assert_eq!(feature.card_draw_count, signals.card_draw_count);
        assert_eq!(feature.instant_count, signals.instant_count);
        assert_eq!(
            feature.spot_removal_instant_count,
            signals.spot_removal_instant_count
        );
        assert_eq!(feature.sweeper_instant_count, signals.sweeper_instant_count);

        let reactive_instant_ratio =
            signals.instant_count as f32 / f32::max(total_nonland as f32, 1.0);
        let interaction_density = (signals.counterspell_count as f32
            + signals.spot_removal_count as f32
            + 2.0 * signals.sweeper_count as f32)
            / f32::max(total_nonland as f32, 1.0);
        let draw_density = signals.card_draw_count as f32 / f32::max(total_nonland as f32, 1.0);
        let interaction_density_instant_only = (signals.counterspell_count as f32
            + signals.spot_removal_instant_count as f32
            + 2.0 * signals.sweeper_instant_count as f32)
            / f32::max(total_nonland as f32, 1.0);
        let commitment = commitment::weighted_sum(&[
            (2.0 / 60.0, interaction_density * 60.0),
            (1.0 / 60.0, draw_density * 60.0),
        ]);
        let reactive_tempo =
            clamp01(1.5 * interaction_density_instant_only + 1.0 * reactive_instant_ratio);

        assert_eq!(feature.reactive_instant_ratio, reactive_instant_ratio);
        assert_eq!(feature.commitment, commitment);
        assert_eq!(feature.reactive_tempo, reactive_tempo);
    }
}
