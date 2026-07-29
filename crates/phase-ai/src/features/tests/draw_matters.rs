//! Unit tests for `features::draw_matters` — CR 121.1 "whenever you draw"
//! detection. No `#[cfg(test)]` in SOURCE files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::draw_matters::*;

fn face(name: &str, core: CoreType) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![core],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

/// A card-draw enabler: a spell that draws YOU cards (CR 121.1).
fn draw_source(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Sorcery);
    f.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
    )];
    f
}

fn drawn_trigger(
    mode: TriggerMode,
    valid_card: Option<TargetFilter>,
    valid_target: Option<TargetFilter>,
) -> TriggerDefinition {
    let mut t = TriggerDefinition::new(mode);
    if let Some(vc) = valid_card {
        t = t.valid_card(vc);
    }
    if let Some(vt) = valid_target {
        t = t.valid_target(vt);
    }
    t.execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Opponent,
            damage_source: None,
            excess: None,
        },
    ))
}

/// The Locust God / Niv-Mizzet shape: a "whenever you draw a card" engine on a
/// permanent, controller-scoped and broad.
fn engine(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Creature);
    f.triggers = vec![drawn_trigger(TriggerMode::Drawn, None, None)];
    f
}

#[test]
fn empty_deck_produces_defaults() {
    let f = detect(&[]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn vanilla_deck_not_registered() {
    let f = detect(&[entry(face("Bear", CoreType::Creature), 20)]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn detects_draw_source() {
    let f = detect(&[entry(draw_source("Divination"), 4)]);
    assert_eq!(f.source_count, 4);
}

/// An ETB "cantrip" creature (Elvish Visionary) — "when this enters, draw a card"
/// — has no `Effect::Draw` in `abilities`, only a self-ETB trigger. The live
/// policy credits these via `CastFacts::immediate_etb_triggers`, so deck-time
/// detection must count them as draw sources too (CR 603.6a), or an ETB-cantrip
/// shell is undercounted.
fn etb_draw_source(name: &str, drawn: TargetFilter) -> CardFace {
    let mut f = face(name, CoreType::Creature);
    let mut t = TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: drawn,
            },
        ));
    t.destination = Some(Zone::Battlefield);
    f.triggers = vec![t];
    f
}

#[test]
fn etb_cantrip_counts_as_a_draw_source() {
    let f = detect(&[entry(
        etb_draw_source("Elvish Visionary", TargetFilter::Controller),
        4,
    )]);
    assert_eq!(f.source_count, 4);
}

/// Control: an ETB that draws an OPPONENT a card is not an enabler for your engine.
#[test]
fn etb_opponent_draw_is_not_a_source() {
    let f = detect(&[entry(
        etb_draw_source("Opponent Cantrip", TargetFilter::Opponent),
        4,
    )]);
    assert_eq!(f.source_count, 0);
}

/// A draw effect that draws an OPPONENT is not an enabler for your engine.
#[test]
fn opponent_draw_effect_is_not_a_source() {
    let mut f = face("Opponent Draws", CoreType::Sorcery);
    f.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Opponent,
        },
    )];
    assert_eq!(detect(&[entry(f, 4)]).source_count, 0);
}

#[test]
fn detects_engine_payoff() {
    let f = detect(&[entry(engine("The Locust God"), 3)]);
    assert_eq!(f.payoff_count, 3);
}

/// A "whenever you draw" trigger with NO execute is a `TriggerNoExecute` no-op —
/// it produces no value, so deck detection must not count it as an engine (else
/// commitment is inflated for an unsupported payoff).
#[test]
fn payoff_without_execute_is_not_counted() {
    let mut f = face("No-op Engine", CoreType::Creature);
    f.triggers = vec![TriggerDefinition::new(TriggerMode::Drawn)]; // no execute
    assert_eq!(detect(&[entry(f, 3)]).payoff_count, 0);
}

/// A "whenever you draw" trigger whose execute is an unsupported
/// (`Effect::Unimplemented`) gap node likewise produces no value and is not
/// counted as an engine.
#[test]
fn payoff_with_unsupported_execute_is_not_counted() {
    let mut f = face("Unsupported Engine", CoreType::Creature);
    f.triggers = vec![
        TriggerDefinition::new(TriggerMode::Drawn).execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("draw_payoff_test_gap", "unsupported payoff"),
        )),
    ];
    assert_eq!(detect(&[entry(f, 3)]).payoff_count, 0);
}

/// Deck-time uses `AbilityScope::Potential`: a modal "choose one — burn / draw"
/// card whose draw lives in the `else` branch still marks the card as a draw
/// enabler for the archetype (the policy is the one that must be stricter live).
#[test]
fn modal_draw_mode_still_counts_as_a_deck_source() {
    let mut modal = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    );
    modal.else_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )));
    let mut f = face("Modal Burn-or-Draw", CoreType::Instant);
    f.abilities = vec![modal];
    assert_eq!(detect(&[entry(f, 4)]).source_count, 4);
}

/// An opponent-scoped "whenever an opponent draws" punisher is not your payoff.
#[test]
fn opponent_scoped_trigger_ignored() {
    let mut f = face("Notion Thief", CoreType::Creature);
    f.triggers = vec![drawn_trigger(
        TriggerMode::Drawn,
        None,
        Some(TargetFilter::Opponent),
    )];
    assert_eq!(detect(&[entry(f, 2)]).payoff_count, 0);
}

/// A self-referential "when this card is drawn" trigger fires from hand on the
/// card itself, not a battlefield engine — not a payoff.
#[test]
fn self_ref_drawn_trigger_is_not_a_payoff() {
    let mut f = face("Drawn Trigger Card", CoreType::Instant);
    f.triggers = vec![drawn_trigger(
        TriggerMode::Drawn,
        Some(TargetFilter::SelfRef),
        None,
    )];
    assert_eq!(detect(&[entry(f, 4)]).payoff_count, 0);
}

/// Calibration: a dedicated draw-engine shell clears the floor.
#[test]
fn committed_draw_deck_hits_floor() {
    let deck = vec![
        entry(draw_source("Cantrip A"), 12),
        entry(draw_source("Cantrip B"), 8),
        entry(engine("The Locust God"), 3),
        entry(engine("Niv-Mizzet"), 2),
        entry(face("Island", CoreType::Land), 24),
    ];
    let f = detect(&deck);
    assert!(
        f.commitment > 0.6,
        "committed draw deck must clear 0.6, got {}",
        f.commitment
    );
}

/// Both pillars are mandatory: card draw with no engine is just card advantage.
#[test]
fn sources_without_engine_collapse() {
    let deck = vec![
        entry(draw_source("Cantrip"), 20),
        entry(face("Island", CoreType::Land), 24),
    ];
    assert_eq!(detect(&deck).commitment, 0.0);
}

/// An engine with no extra draw only triggers on the natural draw for turn.
#[test]
fn engine_without_sources_collapses() {
    let deck = vec![
        entry(engine("The Locust God"), 3),
        entry(face("Island", CoreType::Land), 24),
    ];
    assert_eq!(detect(&deck).commitment, 0.0);
}

#[test]
fn commitment_clamps_to_one() {
    let deck = vec![
        entry(draw_source("Cantrip"), 40),
        entry(engine("The Locust God"), 20),
    ];
    assert!(detect(&deck).commitment <= 1.0);
}

/// Boundary: a non-empty all-land deck has `total_nonland == 0`;
/// `density_per_60` guards that to `0.0`, so commitment is a clean `0.0`, never
/// `NaN` (which would slip past the activation floor).
#[test]
fn all_land_deck_is_zero_not_nan() {
    let deck = vec![
        entry(face("Island", CoreType::Land), 20),
        entry(face("Mountain", CoreType::Land), 20),
    ];
    let commitment = detect(&deck).commitment;
    assert!(!commitment.is_nan());
    assert_eq!(commitment, 0.0);
}
