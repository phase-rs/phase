//! Unit tests for `features::mill` — structural detection + calibration
//! anchors. No `#[cfg(test)]` in SOURCE files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::mill::{detect, effect_is_opponent_mill, is_mill_enabler, COMMITMENT_FLOOR};

fn face(core: Vec<CoreType>) -> CardFace {
    CardFace {
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: core,
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn spell(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, effect)
}

fn mill_spell_face(target: TargetFilter, count: u32) -> CardFace {
    let mut f = face(vec![CoreType::Sorcery]);
    f.abilities.push(spell(Effect::Mill {
        count: QuantityExpr::Fixed {
            value: count as i32,
        },
        target,
        destination: Zone::Graveyard,
    }));
    f
}

fn draw_spell_face() -> CardFace {
    let mut f = face(vec![CoreType::Instant]);
    f.abilities.push(spell(Effect::Draw {
        count: QuantityExpr::Fixed { value: 2 },
        target: TargetFilter::Controller,
    }));
    f
}

fn triggered_mill_creature(target: TargetFilter) -> CardFace {
    let mut f = face(vec![CoreType::Creature]);
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Mill {
            count: QuantityExpr::Fixed { value: 2 },
            target,
            destination: Zone::Graveyard,
        },
    );
    f.triggers.push(
        engine::types::ability::TriggerDefinition::new(TriggerMode::ChangesZone).execute(execute),
    );
    f
}

fn land_face() -> CardFace {
    face(vec![CoreType::Land])
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

// ─── effect_is_opponent_mill ───────────────────────────────────────────────

#[test]
fn player_target_mill_is_opponent_mill() {
    let e = Effect::Mill {
        count: QuantityExpr::Fixed { value: 10 },
        target: TargetFilter::Player,
        destination: Zone::Graveyard,
    };
    assert!(effect_is_opponent_mill(&e));
}

#[test]
fn opponent_filter_mill_is_opponent_mill() {
    let e = Effect::Mill {
        count: QuantityExpr::Fixed { value: 3 },
        target: TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::Opponent)),
        destination: Zone::Graveyard,
    };
    assert!(effect_is_opponent_mill(&e));
}

#[test]
fn controller_target_mill_excluded() {
    let e = Effect::Mill {
        count: QuantityExpr::Fixed { value: 13 },
        target: TargetFilter::Controller,
        destination: Zone::Graveyard,
    };
    assert!(!effect_is_opponent_mill(&e));
}

#[test]
fn any_target_mill_excluded() {
    let e = Effect::Mill {
        count: QuantityExpr::Fixed { value: 6 },
        target: TargetFilter::Any,
        destination: Zone::Graveyard,
    };
    assert!(!effect_is_opponent_mill(&e));
}

// ─── is_mill_enabler ──────────────────────────────────────────────────────

#[test]
fn spell_with_player_target_is_enabler() {
    let f = mill_spell_face(TargetFilter::Player, 10);
    assert!(is_mill_enabler(&f));
}

#[test]
fn spell_with_opponent_target_is_enabler() {
    let f = mill_spell_face(
        TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::Opponent)),
        7,
    );
    assert!(is_mill_enabler(&f));
}

#[test]
fn spell_with_controller_mill_is_not_enabler() {
    let f = mill_spell_face(TargetFilter::Controller, 4);
    assert!(!is_mill_enabler(&f));
}

#[test]
fn trigger_with_player_mill_is_enabler() {
    let f = triggered_mill_creature(TargetFilter::Player);
    assert!(is_mill_enabler(&f));
}

#[test]
fn trigger_with_controller_mill_is_not_enabler() {
    let f = triggered_mill_creature(TargetFilter::Controller);
    assert!(!is_mill_enabler(&f));
}

/// Cross-chain guard: one ability draws, a second mills self — flat-merging
/// would see a self-mill effect but no opponent-mill. Per-chain isolation
/// confirms neither ability individually crosses the threshold.
#[test]
fn cross_ability_draw_and_self_mill_is_not_enabler() {
    let mut f = face(vec![CoreType::Sorcery]);
    f.abilities.push(spell(Effect::Draw {
        count: QuantityExpr::Fixed { value: 2 },
        target: TargetFilter::Controller,
    }));
    f.abilities.push(spell(Effect::Mill {
        count: QuantityExpr::Fixed { value: 3 },
        target: TargetFilter::Controller,
        destination: Zone::Graveyard,
    }));
    assert!(!is_mill_enabler(&f));
}

// ─── detect + calibration ─────────────────────────────────────────────────

#[test]
fn empty_deck_produces_zero_commitment() {
    let f = detect(&[]);
    assert_eq!(f.mill_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn all_lands_deck_produces_zero_commitment() {
    let deck: Vec<DeckEntry> = (0..24).map(|_| entry(land_face(), 1)).collect();
    let f = detect(&deck);
    assert_eq!(f.mill_count, 0);
    assert_eq!(f.commitment, 0.0);
}

/// Calibration anchor — a dedicated mill deck (30 opponent-mill spells in 60
/// cards) must clear `COMMITMENT_FLOOR` so `MillPayoffPolicy` activates.
#[test]
fn dedicated_mill_deck_clears_commitment_floor() {
    let mill = mill_spell_face(TargetFilter::Player, 10);
    let deck = vec![entry(mill, 30), entry(land_face(), 30)];
    let f = detect(&deck);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "dedicated mill deck (30/30 nonland) must clear COMMITMENT_FLOOR {COMMITMENT_FLOOR}; \
         got commitment={}",
        f.commitment
    );
    assert_eq!(f.mill_count, 30);
}

/// Anti-calibration — a splash (4 mill in a 36-nonland deck) stays below floor.
#[test]
fn splash_mill_four_of_stays_below_floor() {
    // 4 mill + 32 draw + 24 land → density ≈ 6.7 per 60 → commitment ≈ 0.33
    let deck = vec![
        entry(mill_spell_face(TargetFilter::Player, 10), 4),
        entry(draw_spell_face(), 32),
        entry(land_face(), 24),
    ];
    let f = detect(&deck);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "splash mill (4/36 nonland) must stay below COMMITMENT_FLOOR {COMMITMENT_FLOOR}; \
         got commitment={}",
        f.commitment
    );
}

/// Commitment clamps at 1.0 even with extreme density.
#[test]
fn commitment_clamps_at_one() {
    let f = detect(&[entry(mill_spell_face(TargetFilter::Player, 10), 60)]);
    assert_eq!(f.commitment, 1.0);
}

/// Self-mill only deck (Fractured Sanity / Fraying Sanity pattern) must score
/// zero — it belongs to the reanimator axis, not the mill win-con axis.
#[test]
fn self_mill_only_deck_has_zero_commitment() {
    let f = detect(&[
        entry(mill_spell_face(TargetFilter::Controller, 13), 20),
        entry(land_face(), 20),
    ]);
    assert_eq!(
        f.mill_count, 0,
        "self-mill must not register as mill enabler"
    );
    assert_eq!(f.commitment, 0.0);
}
