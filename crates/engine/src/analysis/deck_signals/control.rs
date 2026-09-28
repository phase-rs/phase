//! Control feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `Effect::Counter { .. }` — counterspell detection
//!   (`crate::types::ability.rs:14678`). CR 701.6.
//! - `Effect::Destroy { .. }` — destroy removal
//!   (`ability.rs:14656`). CR 701.8.
//! - `Effect::Bounce { destination: None | Some(Hand) }` — bounce removal
//!   (`ability.rs:15132`).
//! - `Effect::ChangeZone { destination: Zone::Exile | Zone::Graveyard, .. }` —
//!   exile/graveyard removal (`ability.rs:14904`). CR 701.13.
//! - `Effect::DealDamage { .. }` — damage removal (`ability.rs:14526`). CR 120.3.
//! - `Effect::DestroyAll { .. }` (`ability.rs:14897`), `Effect::DamageAll { .. }`
//!   (`ability.rs:14869`), `Effect::ChangeZoneAll { .. }` (`ability.rs:14965`) —
//!   sweepers. These are **distinct** variants from spot removal — the parser
//!   emits them separately, so sweeper-vs-spot classification is unambiguous.
//! - `Effect::Draw { count }` (`ability.rs:14636`), `Effect::Dig { .. }`
//!   (`ability.rs:15014`) — card advantage variants. CR 120.1.
//! - `CoreType::Instant` / `CoreType::Sorcery` (`card_type.rs:49`).
//!   CR 117.1a + CR 304.1: instants can be cast any time the player has priority.
//! - `AbilityKind::Spell` (`ability.rs:22774`) — distinguishes spell effects from
//!   activated/triggered abilities.
//!
//! No parser remediation required — sweeper vs. spot removal is discriminable
//! via distinct `Effect` enum variants.
//!
//! No mulligan policy: control hands vary between "hold up interaction" and
//! "deploy finisher" — no single hand-shape signal is reliable enough to warrant
//! a mulligan policy analogous to ramp or landfall.

use crate::analysis::deck_signals::ability_chain::collect_chain_effects;
use crate::analysis::deck_signals::DeckCard;
use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::zones::Zone;

/// Integer control signals derived structurally from a deck's parsed AST.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlSignals {
    pub counterspell_count: u32,
    pub spot_removal_count: u32,
    pub sweeper_count: u32,
    pub card_draw_count: u32,
    pub instant_count: u32,
    pub spot_removal_instant_count: u32,
    pub sweeper_instant_count: u32,
}

/// Structural detection over resolved deck cards, weighted by copy count.
///
/// Planeswalker activated abilities are deliberately excluded from removal
/// counts — they are `AbilityKind::Activated`, not `AbilityKind::Spell`. A
/// planeswalker's `-N: Destroy target creature` is a loyalty activation, not a
/// control spell.
pub fn detect(cards: &[DeckCard<'_>]) -> ControlSignals {
    let mut counterspell_count = 0u32;
    let mut spot_removal_count = 0u32;
    let mut sweeper_count = 0u32;
    let mut card_draw_count = 0u32;
    let mut instant_count = 0u32;
    let mut spot_removal_instant_count = 0u32;
    let mut sweeper_instant_count = 0u32;
    for card in cards {
        let face = card.face;
        let is_land = face.card_type.core_types.contains(&CoreType::Land);

        // Per-axis bool sentinels: a face contributes at most once per axis
        // even if multiple abilities match (e.g., a modal spell).
        let is_cs = is_counterspell(face);
        let is_sw = is_sweeper_parts(&face.abilities);
        // Spot removal is mutually exclusive with sweeper.
        let is_sr = !is_sw && is_spot_removal_parts(&face.abilities);
        let is_cd = !is_land && is_card_draw(face);
        let is_instant = !is_land && face.card_type.core_types.contains(&CoreType::Instant);

        if is_cs {
            counterspell_count = counterspell_count.saturating_add(card.count);
        }
        if is_sw {
            sweeper_count = sweeper_count.saturating_add(card.count);
            if is_instant {
                sweeper_instant_count = sweeper_instant_count.saturating_add(card.count);
            }
        }
        if is_sr {
            spot_removal_count = spot_removal_count.saturating_add(card.count);
            if is_instant {
                spot_removal_instant_count = spot_removal_instant_count.saturating_add(card.count);
            }
        }
        if is_cd {
            card_draw_count = card_draw_count.saturating_add(card.count);
        }
        if is_instant {
            instant_count = instant_count.saturating_add(card.count);
        }
    }

    ControlSignals {
        counterspell_count,
        spot_removal_count,
        sweeper_count,
        card_draw_count,
        instant_count,
        spot_removal_instant_count,
        sweeper_instant_count,
    }
}

/// A counterspell has an `AbilityKind::Spell` ability whose effect chain
/// contains `Effect::Counter`.
///
/// CR 701.6: countering removes the spell from the stack without it resolving.
/// Only `AbilityKind::Spell` is checked — activated tap-for-counter abilities
/// (e.g., Ertai) have `AbilityKind::Activated` and are excluded.
pub fn is_counterspell_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Spell
            && collect_chain_effects(ability)
                .iter()
                .any(|e| matches!(e, Effect::Counter { .. }))
    })
}

fn is_counterspell(face: &CardFace) -> bool {
    is_counterspell_parts(&face.abilities)
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
pub fn is_sweeper_parts(abilities: &[AbilityDefinition]) -> bool {
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
pub fn is_spot_removal_parts(abilities: &[AbilityDefinition]) -> bool {
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
pub fn is_card_draw_parts(abilities: &[AbilityDefinition]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::deck_loading::DeckEntry;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, BounceSelection, DigSource, Effect, QuantityExpr,
        TargetFilter,
    };
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::zones::Zone;

    fn detect(deck: &[DeckEntry]) -> ControlSignals {
        let cards = DeckCard::from_deck_entries(deck);
        super::detect(&cards)
    }

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
                source_rider: None,
                countered_spell_zone: None,
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
                selection: BounceSelection::Targeted,
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
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                damage_source: None,
            },
        )
    }

    fn draw_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: crate::types::ability::TargetFilter::Controller,
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
                excess: None,
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
    }

    #[test]
    fn detects_spot_destroy_removal() {
        let mut face = card_face_with_types("Doom Blade", vec![CoreType::Instant]);
        face.abilities.push(destroy_ability());
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.spot_removal_count, 4);
        assert_eq!(feature.sweeper_count, 0);
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
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 3 },
                destination: None,
                keep_count: Some(3),
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                rest_split_top_count: None,
                rest_order: crate::types::ability::DigRestOrder::Preserve,
                reveal: false,
                enter_tapped: false,
                enters_attacking: false,
                source: DigSource::Library,
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
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                destination: Some(Zone::Exile),
                keep_count: Some(1),
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                rest_split_top_count: None,
                rest_order: crate::types::ability::DigRestOrder::Preserve,
                reveal: false,
                enter_tapped: false,
                enters_attacking: false,
                source: DigSource::Library,
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
                target: crate::types::ability::TargetFilter::Controller,
            },
        ));
        let deck = vec![entry(face, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.card_draw_count, 0);
    }

    #[test]
    fn instant_count_is_quantity_weighted() {
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
        assert_eq!(feature.instant_count, 2);
    }

    #[test]
    fn sorcery_sweepers_are_not_instant_sweepers() {
        let mut sweeper = card_face_with_types("Sorcery Wrath", vec![CoreType::Sorcery]);
        sweeper.abilities.push(destroy_all_ability());
        let deck = vec![entry(sweeper, 8)];

        let feature = detect(&deck);
        assert_eq!(feature.sweeper_count, 8);
        assert_eq!(feature.instant_count, 0);
        assert_eq!(feature.sweeper_instant_count, 0);
    }

    #[test]
    fn instant_control_tracks_instant_removal_separately() {
        let mut counter = card_face_with_types("Counterspell", vec![CoreType::Instant]);
        counter.abilities.push(counter_ability());
        let mut removal = card_face_with_types("Instant Removal", vec![CoreType::Instant]);
        removal.abilities.push(destroy_ability());
        let deck = vec![entry(counter, 4), entry(removal, 4)];

        let feature = detect(&deck);
        assert_eq!(feature.counterspell_count, 4);
        assert_eq!(feature.spot_removal_count, 4);
        assert_eq!(feature.spot_removal_instant_count, 4);
        assert_eq!(feature.instant_count, 8);
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
    }

    #[test]
    fn empty_deck_produces_defaults() {
        let feature = detect(&[]);
        assert_eq!(feature.counterspell_count, 0);
        assert_eq!(feature.spot_removal_count, 0);
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
