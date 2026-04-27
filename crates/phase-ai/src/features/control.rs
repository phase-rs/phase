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

use engine::game::DeckEntry;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;

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
    if deck.is_empty() {
        return ControlFeature::default();
    }

    let mut counterspell_count = 0u32;
    let mut spot_removal_count = 0u32;
    let mut sweeper_count = 0u32;
    let mut card_draw_count = 0u32;
    let mut instant_count = 0u32;
    let mut spot_removal_instant_count = 0u32;
    let mut sweeper_instant_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        let is_land = face.card_type.core_types.contains(&CoreType::Land);
        if !is_land {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // Per-axis bool sentinels: a face contributes at most once per axis
        // even if multiple abilities match (e.g., a modal spell).
        let is_cs = is_counterspell(face);
        let is_sw = is_sweeper_parts(&face.abilities);
        // Spot removal is mutually exclusive with sweeper.
        let is_sr = !is_sw && is_spot_removal_parts(&face.abilities);
        let is_cd = !is_land && is_card_draw(face);
        let is_instant = !is_land && face.card_type.core_types.contains(&CoreType::Instant);

        if is_cs {
            counterspell_count = counterspell_count.saturating_add(entry.count);
        }
        if is_sw {
            sweeper_count = sweeper_count.saturating_add(entry.count);
            if is_instant {
                sweeper_instant_count = sweeper_instant_count.saturating_add(entry.count);
            }
        }
        if is_sr {
            spot_removal_count = spot_removal_count.saturating_add(entry.count);
            if is_instant {
                spot_removal_instant_count = spot_removal_instant_count.saturating_add(entry.count);
            }
        }
        if is_cd {
            card_draw_count = card_draw_count.saturating_add(entry.count);
        }
        if is_instant {
            instant_count = instant_count.saturating_add(entry.count);
        }
    }

    let reactive_instant_ratio = instant_count as f32 / f32::max(total_nonland as f32, 1.0);

    // Commitment formula calibrated against competitive control archetypes.
    // Azorius Control baseline: ~4 counters, ~6 spot removal, ~2-3 sweepers,
    // ~6 draw spells, ~14 instants in a 60-card deck (~40 nonlands).
    // interaction_density at baseline: (4 + 6 + 6) / 40 = 0.40 → × 2.0 = 0.80.
    // draw_density at baseline: 6 / 40 = 0.15 → × 1.0 = 0.15.
    // commitment = clamp01(0.80 + 0.15) ≈ 0.95. ✓
    //
    // CR 701.6: counter. CR 701.8: destroy. CR 701.13: exile.
    let interaction_density =
        (counterspell_count as f32 + spot_removal_count as f32 + 2.0 * sweeper_count as f32)
            / f32::max(total_nonland as f32, 1.0);
    let draw_density = card_draw_count as f32 / f32::max(total_nonland as f32, 1.0);

    // Instant-only variant for reactive_tempo.
    // Counterspells are tautologically instant-speed (CR 117.1a + CR 304.1).
    let interaction_density_instant_only = (counterspell_count as f32
        + spot_removal_instant_count as f32
        + 2.0 * sweeper_instant_count as f32)
        / f32::max(total_nonland as f32, 1.0);

    let commitment = clamp01(2.0 * interaction_density + 1.0 * draw_density);
    // reactive_tempo intentionally sums two overlapping signals:
    //   1. instant-only interaction density (counts each interaction spell once)
    //   2. raw instant ratio (counts every instant in the deck — interaction OR not)
    // The overlap is by design — an instant-speed counterspell legitimately
    // contributes to both the "I have interaction I can hold up" signal and the
    // "this is an instant-heavy deck" signal. Calibration absorbs the overlap.
    let reactive_tempo =
        clamp01(1.5 * interaction_density_instant_only + 1.0 * reactive_instant_ratio);

    ControlFeature {
        counterspell_count,
        spot_removal_count,
        sweeper_count,
        card_draw_count,
        instant_count,
        spot_removal_instant_count,
        sweeper_instant_count,
        reactive_instant_ratio,
        commitment,
        reactive_tempo,
    }
}

/// A counterspell has an `AbilityKind::Spell` ability whose effect chain
/// contains `Effect::Counter`.
///
/// CR 701.6: countering removes the spell from the stack without it resolving.
/// Only `AbilityKind::Spell` is checked — activated tap-for-counter abilities
/// (e.g., Ertai) have `AbilityKind::Activated` and are excluded.
fn is_counterspell(face: &CardFace) -> bool {
    face.abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability)
                .iter()
                .any(|e| matches!(e, Effect::Counter { .. }))
    })
}

/// Parts-based sweeper classifier. Exported `pub(crate)` so `SweeperTimingPolicy`
/// can call it against live `GameObject` fields without re-implementing the logic.
///
/// A sweeper is a card whose effect chain contains `DestroyAll`, `DamageAll`,
/// or `ChangeZoneAll`. Distinct variants from spot removal — no overlap possible.
/// CR 701.8 (destroy), CR 120.3 (damage), CR 701.13 (exile).
///
/// Takes only `abilities` because sweeper detection is purely effect-shape
/// based. The `_parts` suffix is retained for policy parity with other
/// classifiers — callers pass `&obj.abilities` directly.
pub(crate) fn is_sweeper_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        collect_chain_effects(ability).iter().any(|e| {
            matches!(
                e,
                Effect::DestroyAll { .. } | Effect::DamageAll { .. } | Effect::ChangeZoneAll { .. }
            )
        })
    })
}

/// Parts-based spot removal classifier. Exported `pub(crate)` for policy reuse.
///
/// Spot removal is a spell (`AbilityKind::Spell`) whose effect chain contains:
/// - `Effect::Destroy { .. }` — CR 701.8
/// - `Effect::Bounce { destination: None | Some(Hand) }` — return to hand
/// - `Effect::ChangeZone { destination: Exile | Graveyard, .. }` — CR 701.13
/// - `Effect::DealDamage { .. }` — direct damage removal (CR 120.3)
///
/// Must NOT already be classified as a sweeper (callers enforce mutual exclusivity).
/// Planeswalker loyalty activations are excluded — only `AbilityKind::Spell` counts.
pub(crate) fn is_spot_removal_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability)
                .iter()
                .any(is_spot_removal_effect)
    })
}

fn is_spot_removal_effect(e: &&Effect) -> bool {
    match e {
        Effect::Destroy { .. } => true,
        Effect::Bounce { destination, .. } => {
            // Bounce to hand (or unspecified, defaulting to hand) is removal.
            // Bounce to library or other zones is a different effect.
            matches!(destination, None | Some(Zone::Hand))
        }
        Effect::ChangeZone { destination, .. } => {
            matches!(destination, Zone::Exile | Zone::Graveyard)
        }
        Effect::DealDamage { .. } => true,
        _ => false,
    }
}

/// Parts-based card draw classifier. Exported `pub(crate)` so `spellslinger_prowess`
/// can call it for cantrip detection without re-implementing the impulse-cast
/// carve-out logic.
///
/// CR 121.1: drawing cards moves them to hand. An `Effect::Dig` whose kept-card
/// destination is `Exile` is impulse-cast (e.g., Outpost Siege variant) — it
/// doesn't move cards to hand and doesn't satisfy CR 121.1, so it is excluded.
/// Dig variants whose destination is `None` (defaults to Hand) or explicitly
/// `Hand` are accepted. Only `AbilityKind::Spell` abilities count.
pub(crate) fn is_card_draw_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability).iter().any(|e| match e {
                // Any non-zero draw counts. `Fixed { value: 0 }` is a no-op
                // draw (rare modal corner case); variable / Ref quantities
                // ("draw X cards") are accepted as net-positive draws since
                // X is normally ≥ 1 at resolution.
                Effect::Draw { count, .. } => !matches!(count, QuantityExpr::Fixed { value: 0 }),
                // Impulse-to-exile is tempo, not card advantage — excluded.
                Effect::Dig { destination, .. } => !matches!(destination, Some(Zone::Exile)),
                _ => false,
            })
    })
}

/// Thin wrapper around `is_card_draw_parts` for callers that have a full
/// `CardFace`. Lands are excluded by the caller's `!is_land` gate.
fn is_card_draw(face: &CardFace) -> bool {
    is_card_draw_parts(&face.abilities)
}

#[inline]
fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::DeckEntry;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::zones::Zone;

    fn card_face_with_types(name: &str, core_types: Vec<CoreType>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: Vec::new(),
                core_types,
                subtypes: Vec::new(),
            },
            ..Default::default()
        }
    }

    fn entry(card: CardFace, count: u32) -> DeckEntry {
        DeckEntry { card, count }
    }

    fn counter_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            },
        )
    }

    fn destroy_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        )
    }

    fn bounce_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
        )
    }

    fn exile_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )
    }

    fn destroy_all_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DestroyAll {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        )
    }

    fn damage_all_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                player_filter: None,
            },
        )
    }

    fn draw_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        )
    }

    fn damage_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )
    }

    #[test]
    fn detects_hard_counter() {
        let mut face = card_face_with_types("Counterspell", vec![CoreType::Instant]);
        face.abilities.push(counter_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.counterspell_count, 4);
        assert_eq!(feature.spot_removal_count, 0);
        assert_eq!(feature.sweeper_count, 0);
        assert!(feature.commitment > 0.0);
    }

    #[test]
    fn detects_spot_destroy_removal() {
        let mut face = card_face_with_types("Doom Blade", vec![CoreType::Instant]);
        face.abilities.push(destroy_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.spot_removal_count, 4);
        assert_eq!(feature.sweeper_count, 0);
        assert!(feature.commitment > 0.0);
    }

    #[test]
    fn detects_spot_bounce_removal() {
        let mut face = card_face_with_types("Unsummon", vec![CoreType::Instant]);
        face.abilities.push(bounce_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.spot_removal_count, 4);
        assert_eq!(feature.sweeper_count, 0);
    }

    #[test]
    fn detects_spot_exile_removal() {
        let mut face = card_face_with_types("Path to Exile", vec![CoreType::Instant]);
        face.abilities.push(exile_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.spot_removal_count, 4);
        assert_eq!(feature.sweeper_count, 0);
    }

    #[test]
    fn detects_sweeper_not_as_spot_removal() {
        // DestroyAll must NOT also register as spot removal.
        let mut face = card_face_with_types("Wrath of God", vec![CoreType::Sorcery]);
        face.abilities.push(destroy_all_ability());
        let deck = vec![entry(face, 3)];

        let feature = detect(&deck);
        assert_eq!(feature.sweeper_count, 3);
        assert_eq!(feature.spot_removal_count, 0);
    }

    #[test]
    fn detects_card_draw() {
        let mut face = card_face_with_types("Divination", vec![CoreType::Sorcery]);
        face.abilities.push(draw_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.card_draw_count, 4);
    }

    #[test]
    fn detects_dig_to_hand_as_card_draw() {
        // Dig with destination = None defaults to Hand → counts as card draw.
        let mut face = card_face_with_types("Brainstorm-shape", vec![CoreType::Instant]);
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Dig {
                count: QuantityExpr::Fixed { value: 3 },
                destination: None,
                keep_count: Some(3),
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
            },
        ));
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.card_draw_count, 4);
    }

    #[test]
    fn impulse_dig_to_exile_excluded_from_card_draw() {
        // CR 120.1: card draw moves cards to hand. A Dig whose kept-card
        // destination is Exile is impulse-cast (e.g., Outpost Siege variant) —
        // it must NOT count as card draw.
        let mut face = card_face_with_types("Outpost-shape", vec![CoreType::Sorcery]);
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Dig {
                count: QuantityExpr::Fixed { value: 1 },
                destination: Some(Zone::Exile),
                keep_count: Some(1),
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
            },
        ));
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.card_draw_count, 0);
    }

    #[test]
    fn detects_modal_counter_plus_damage_once_each() {
        // A modal spell with one Counter mode and one DealDamage mode:
        // counterspell_count += 1, spot_removal_count += 1.
        let mut face = card_face_with_types("Modal Spell", vec![CoreType::Instant]);
        face.abilities.push(counter_ability());
        face.abilities.push(damage_ability());
        let deck = vec![entry(face, 1)];

        let feature = detect(&deck);
        assert_eq!(feature.counterspell_count, 1);
        assert_eq!(feature.spot_removal_count, 1);
    }

    #[test]
    fn flash_creature_not_counted_as_instant() {
        // A creature with flash has CoreType::Creature, NOT CoreType::Instant.
        // It must NOT count toward instant_count.
        let mut face = card_face_with_types("Flash Creature", vec![CoreType::Creature]);
        face.abilities.push(destroy_ability());
        let deck = vec![entry(face, 2)];

        let feature = detect(&deck);
        assert_eq!(feature.instant_count, 0);
        // Spot removal is still counted (it's a spell-kind destroy ability).
        assert_eq!(feature.spot_removal_count, 2);
        assert_eq!(feature.spot_removal_instant_count, 0);
    }

    #[test]
    fn ignores_cantrip_on_land() {
        // A land with an activated draw ability — activated, not Spell-kind,
        // so is_card_draw returns false. Also gated by is_land.
        let mut face = card_face_with_types("Cantrip Land", vec![CoreType::Land]);
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        ));
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.card_draw_count, 0);
    }

    #[test]
    fn instant_ratio_computed_correctly() {
        // 2 instants out of 4 nonland cards → ratio = 0.5.
        let mut instant_a = card_face_with_types("Counter A", vec![CoreType::Instant]);
        instant_a.abilities.push(counter_ability());
        let mut instant_b = card_face_with_types("Bolt", vec![CoreType::Instant]);
        instant_b.abilities.push(damage_ability());
        let sorcery = card_face_with_types("Divination", vec![CoreType::Sorcery]);
        let creature = card_face_with_types("Bear", vec![CoreType::Creature]);
        let deck = vec![
            entry(instant_a, 1),
            entry(instant_b, 1),
            entry(sorcery, 1),
            entry(creature, 1),
        ];

        let feature = detect(&deck);
        let expected = 2.0 / 4.0;
        assert!(
            (feature.reactive_instant_ratio - expected).abs() < 1e-5,
            "expected ratio {expected}, got {}",
            feature.reactive_instant_ratio
        );
    }

    #[test]
    fn reactive_tempo_zero_for_sorcery_only_control() {
        // 8 sorcery sweepers: high commitment (sweepers × 2× weight), but
        // zero instant_count → reactive_tempo should be near zero.
        let mut sweeper = card_face_with_types("Sorcery Wrath", vec![CoreType::Sorcery]);
        sweeper.abilities.push(destroy_all_ability());
        let deck = vec![entry(sweeper, 8)];

        let feature = detect(&deck);
        assert!(
            feature.commitment > 0.25,
            "commitment should be above floor: {}",
            feature.commitment
        );
        assert_eq!(feature.instant_count, 0);
        assert_eq!(feature.sweeper_instant_count, 0);
        // reactive_tempo: instant_only_density = 0, reactive_instant_ratio = 0.
        assert!(
            feature.reactive_tempo < 0.05,
            "reactive_tempo should be near zero for sorcery-only control: {}",
            feature.reactive_tempo
        );
    }

    #[test]
    fn reactive_tempo_high_for_instant_control() {
        // 4 counterspells + 4 instant-speed removal → high reactive_tempo.
        let mut counter = card_face_with_types("Counterspell", vec![CoreType::Instant]);
        counter.abilities.push(counter_ability());
        let mut removal = card_face_with_types("Instant Removal", vec![CoreType::Instant]);
        removal.abilities.push(destroy_ability());
        let deck = vec![entry(counter, 4), entry(removal, 4)];

        let feature = detect(&deck);
        assert!(
            feature.reactive_tempo > 0.5,
            "reactive_tempo should be high for instant-heavy control: {}",
            feature.reactive_tempo
        );
    }

    #[test]
    fn vanilla_creature_not_registered() {
        let face = card_face_with_types("Grizzly Bears", vec![CoreType::Creature]);
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.counterspell_count, 0);
        assert_eq!(feature.spot_removal_count, 0);
        assert_eq!(feature.sweeper_count, 0);
        assert_eq!(feature.card_draw_count, 0);
        assert_eq!(feature.commitment, 0.0);
    }

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert_eq!(feature.counterspell_count, 0);
        assert_eq!(feature.spot_removal_count, 0);
        assert_eq!(feature.commitment, 0.0);
        assert_eq!(feature.reactive_tempo, 0.0);
    }

    #[test]
    fn commitment_clamps_to_one() {
        // Enough interaction to overflow → must be clamped to 1.0.
        let mut face = card_face_with_types("Counter", vec![CoreType::Instant]);
        face.abilities.push(counter_ability());
        let deck = vec![entry(face, 60)];

        let feature = detect(&deck);
        assert!(
            (feature.commitment - 1.0).abs() < 1e-5,
            "commitment should clamp to 1.0, got {}",
            feature.commitment
        );
    }

    #[test]
    fn damage_all_sweeper_detected() {
        let mut face = card_face_with_types("Pyroclasm", vec![CoreType::Sorcery]);
        face.abilities.push(damage_all_ability());
        let deck = vec![entry(face, 2)];

        let feature = detect(&deck);
        assert_eq!(feature.sweeper_count, 2);
        assert_eq!(feature.spot_removal_count, 0);
    }
}
