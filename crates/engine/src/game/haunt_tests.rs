//! Tests for Haunt (CR 702.55). Declared from `game/mod.rs` so `haunt.rs`
//! (runtime) and `database/haunt.rs` (synthesis) stay implementation-only.

use std::sync::Arc;

use super::haunt::{haunted_creature, match_haunted_creature_dies, resolve};
use super::triggers::process_triggers;
use super::zones::{create_object, move_to_zone};
use crate::database::haunt::synthesize_haunt;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ResolvedAbility, TargetFilter, TargetRef,
    TriggerDefinition, TypeFilter, TypedFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState, StackEntryKind};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// Whether an event is a creature dying (battlefield -> graveyard).
fn is_dies_event(e: &GameEvent) -> bool {
    matches!(
        e,
        GameEvent::ZoneChanged {
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            ..
        }
    )
}

// ---------------------------------------------------------------------------
// Synthesis-shape tests (building-block level)
// ---------------------------------------------------------------------------

/// A creature face carrying Haunt plus an ETB self-trigger (the parsed "enters
/// or the creature it haunts dies" payoff effect).
fn haunt_creature_face() -> CardFace {
    let mut face = CardFace::default();
    face.card_type.core_types.push(CoreType::Creature);
    face.keywords.push(Keyword::Haunt);
    // The ETB self-trigger whose effect is the haunt payoff.
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
                    cant_regenerate: false,
                },
            )),
    );
    face
}

fn haunt_spell_face() -> CardFace {
    let mut face = CardFace::default();
    face.card_type.core_types.push(CoreType::Instant);
    face.keywords.push(Keyword::Haunt);
    face
}

fn exile_haunting_trigger(face: &CardFace) -> &TriggerDefinition {
    face.triggers
        .iter()
        .find(|t| {
            t.execute
                .as_ref()
                .is_some_and(|a| matches!(a.effect.as_ref(), Effect::ExileHaunting { .. }))
        })
        .expect("an ExileHaunting trigger must be synthesized")
}

/// CR 702.55a + CR 702.55c (creature): the dies haunt ability + a payoff clone.
#[test]
fn synthesize_haunt_creature_builds_dies_ability_and_payoff_clone() {
    let mut face = haunt_creature_face();
    synthesize_haunt(&mut face);

    // CR 702.55a: dies trigger (Battlefield -> Graveyard) -> ExileHaunting target creature.
    let haunt = exile_haunting_trigger(&face);
    assert!(matches!(haunt.mode, TriggerMode::ChangesZone));
    assert_eq!(haunt.origin, Some(Zone::Battlefield));
    assert_eq!(haunt.destination, Some(Zone::Graveyard));
    assert_eq!(haunt.valid_card, Some(TargetFilter::SelfRef));

    // CR 702.55c: payoff clone — HauntedCreatureDies in the exile zone, same
    // effect as the ETB trigger (Destroy an enchantment).
    let payoff = face
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::HauntedCreatureDies))
        .expect("a HauntedCreatureDies payoff must be synthesized");
    assert_eq!(payoff.trigger_zones, vec![Zone::Exile]);
    assert!(payoff
        .execute
        .as_ref()
        .is_some_and(|a| matches!(a.effect.as_ref(), Effect::Destroy { .. })));
}

/// CR 702.55a (spell): the haunt ability fires when the spell is put into the
/// graveyard from the stack, scanned from the graveyard.
#[test]
fn synthesize_haunt_spell_builds_stack_to_graveyard_ability() {
    let mut face = haunt_spell_face();
    synthesize_haunt(&mut face);

    let haunt = exile_haunting_trigger(&face);
    assert_eq!(haunt.origin, Some(Zone::Stack));
    assert_eq!(haunt.destination, Some(Zone::Graveyard));
    assert_eq!(haunt.trigger_zones, vec![Zone::Graveyard]);
}

#[test]
fn synthesize_haunt_is_noop_without_keyword() {
    let mut face = CardFace::default();
    face.card_type.core_types.push(CoreType::Creature);
    synthesize_haunt(&mut face);
    assert!(face.triggers.is_empty());
}

#[test]
fn synthesize_haunt_is_idempotent() {
    let mut face = haunt_creature_face();
    synthesize_haunt(&mut face);
    let after_first = face.triggers.len();
    synthesize_haunt(&mut face);
    assert_eq!(face.triggers.len(), after_first);
}

// ---------------------------------------------------------------------------
// Parser (spell-form payoff condition)
// ---------------------------------------------------------------------------

/// CR 702.55c: "When the creature this card haunts dies, …" parses to a
/// HauntedCreatureDies trigger in the exile zone, with its effect intact.
#[test]
fn parser_recognizes_spell_haunt_payoff_condition() {
    let def = crate::parser::oracle_trigger::parse_trigger_line(
        "When the creature this card haunts dies, destroy target creature.",
        "Test Haunt Spell",
    );
    assert!(matches!(def.mode, TriggerMode::HauntedCreatureDies));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.trigger_zones, vec![Zone::Exile]);
    assert!(def
        .execute
        .as_ref()
        .is_some_and(|a| matches!(a.effect.as_ref(), Effect::Destroy { .. })));
}

/// CR 119.3 + CR 102.1: a spell haunt card whose effect is "gain 1 life for each
/// player" (Benediction of Moons) parses that quantity dynamically — no clause
/// is silently swallowed once the card is no longer masked by an `Unknown`
/// trigger.
#[test]
fn benediction_of_moons_gains_life_per_player_without_swallow() {
    use crate::types::ability::{QuantityExpr, QuantityRef};
    let oracle = "You gain 1 life for each player.\n\
         Haunt (When this spell card is put into a graveyard after resolving, exile it haunting \
         target creature.)\n\
         When the creature this card haunts dies, you gain 1 life for each player.";
    let parsed = crate::parser::oracle::parse_oracle_text(
        oracle,
        "Benediction of Moons",
        &["Haunt".to_string()],
        &["Enchantment".to_string()],
        &[],
    );
    // No swallowed-clause diagnostic (the "for each player" multiplier is kept).
    assert!(
        !parsed.parse_warnings.iter().any(|w| matches!(
            w,
            crate::parser::oracle_ir::diagnostic::OracleDiagnostic::SwallowedClause { .. }
        )),
        "gain-life-for-each-player must not swallow a clause: {:?}",
        parsed.parse_warnings
    );
    // The main effect's amount is the dynamic player count, not Fixed(1).
    assert!(
        parsed.abilities.iter().any(|a| matches!(
            a.effect.as_ref(),
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount { .. }
                },
                ..
            }
        )),
        "gain life amount must be a dynamic player count"
    );
}

// ---------------------------------------------------------------------------
// Runtime — matcher, resolver, link lifetime
// ---------------------------------------------------------------------------

fn creature(state: &mut GameState, card: u64, name: &str, zone: Zone) -> ObjectId {
    let id = create_object(state, CardId(card), PlayerId(0), name.to_string(), zone);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types.core_types.push(CoreType::Creature);
    id
}

fn add_haunt_link(state: &mut GameState, card: ObjectId, creature: ObjectId) {
    state.exile_links.push(ExileLink {
        exiled_id: card,
        source_id: creature,
        kind: ExileLinkKind::Haunt,
    });
}

/// CR 702.55c: the matcher fires only for the death of the exact creature the
/// card haunts.
#[test]
fn match_haunted_creature_dies_only_for_the_haunted_creature() {
    let mut state = GameState::new_two_player(1);
    let haunting_card = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Ghost".into(),
        Zone::Exile,
    );
    let haunted = creature(&mut state, 2, "Haunted", Zone::Battlefield);
    let other = creature(&mut state, 3, "Other", Zone::Battlefield);
    add_haunt_link(&mut state, haunting_card, haunted);

    let mut events = Vec::new();
    move_to_zone(&mut state, haunted, Zone::Graveyard, &mut events);
    let dies_event = events
        .iter()
        .find(|e| is_dies_event(e))
        .cloned()
        .expect("haunted creature's ZoneChanged event");
    let dummy = TriggerDefinition::new(TriggerMode::HauntedCreatureDies);
    assert!(match_haunted_creature_dies(
        &dies_event,
        &dummy,
        haunting_card,
        &state
    ));

    // A different creature dying must not match.
    let mut events2 = Vec::new();
    move_to_zone(&mut state, other, Zone::Graveyard, &mut events2);
    let other_event = events2
        .into_iter()
        .find(is_dies_event)
        .expect("other creature's ZoneChanged event");
    assert!(!match_haunted_creature_dies(
        &other_event,
        &dummy,
        haunting_card,
        &state
    ));
}

/// CR 603.10a + CR 700.4: the dies check uses last-known information from the
/// `ZoneChanged` record, not the graveyard object — an animated land /
/// creature-land that died as a creature has shed its granted creature type by
/// the time the trigger fires, but must still trigger the haunt payoff.
#[test]
fn match_haunted_creature_dies_uses_last_known_information() {
    let mut state = GameState::new_two_player(1);
    let haunting_card = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Ghost".into(),
        Zone::Exile,
    );
    let haunted = creature(&mut state, 2, "Animated Land", Zone::Battlefield);
    add_haunt_link(&mut state, haunting_card, haunted);

    let mut events = Vec::new();
    move_to_zone(&mut state, haunted, Zone::Graveyard, &mut events);
    // The record snapshots its battlefield (creature) types; now strip the
    // granted creature type from the graveyard object (de-animation).
    {
        let obj = state.objects.get_mut(&haunted).unwrap();
        obj.card_types.core_types.clear();
        obj.base_card_types.core_types.clear();
    }
    let dies_event = events
        .into_iter()
        .find(is_dies_event)
        .expect("haunted creature's ZoneChanged event");
    let dummy = TriggerDefinition::new(TriggerMode::HauntedCreatureDies);
    assert!(
        match_haunted_creature_dies(&dies_event, &dummy, haunting_card, &state),
        "the dies check must read the record's LKI, not the graveyard object's current types"
    );
}

/// CR 702.55a–b: resolving ExileHaunting moves the (graveyard) card to exile and
/// links it to the targeted creature.
#[test]
fn resolve_exile_haunting_exiles_card_and_links_it() {
    let mut state = GameState::new_two_player(1);
    let card = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Thrull".into(),
        Zone::Graveyard,
    );
    let target = creature(&mut state, 2, "Victim", Zone::Battlefield);

    let ability = ResolvedAbility::new(
        Effect::ExileHaunting {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
        },
        vec![TargetRef::Object(target)],
        card,
        PlayerId(0),
    );
    let mut events = Vec::new();
    resolve(&mut state, &ability, &mut events).unwrap();

    assert_eq!(state.objects[&card].zone, Zone::Exile);
    assert_eq!(haunted_creature(&state, card), Some(target));
}

/// CR 702.55b/c: the Haunt link survives the haunted creature leaving the
/// battlefield (its death) — that is exactly when the payoff reads it.
#[test]
fn haunt_link_survives_the_haunted_creature_dying() {
    let mut state = GameState::new_two_player(1);
    let card = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Ghost".into(),
        Zone::Exile,
    );
    let haunted = creature(&mut state, 2, "Haunted", Zone::Battlefield);
    add_haunt_link(&mut state, card, haunted);

    let mut events = Vec::new();
    move_to_zone(&mut state, haunted, Zone::Graveyard, &mut events);
    assert_eq!(
        haunted_creature(&state, card),
        Some(haunted),
        "the link must persist through the haunted creature's death"
    );
}

// ---------------------------------------------------------------------------
// Integration — triggers actually fire
// ---------------------------------------------------------------------------

fn attach_synth(state: &mut GameState, id: ObjectId, face: &CardFace) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.trigger_definitions = face.triggers.clone().into();
    obj.base_trigger_definitions = Arc::new(face.triggers.clone());
}

fn has_triggered_ability_on_stack(state: &GameState) -> bool {
    state
        .stack
        .iter()
        .any(|e| matches!(&e.kind, StackEntryKind::TriggeredAbility { .. }))
}

/// CR 702.55a (creature): the haunt ability fires when the permanent dies.
#[test]
fn haunt_ability_fires_when_creature_dies() {
    let mut face = haunt_creature_face();
    synthesize_haunt(&mut face);
    let mut state = GameState::new_two_player(1);
    state.active_player = PlayerId(0);
    state.phase = crate::types::phase::Phase::PostCombatMain;
    let _victim = creature(&mut state, 9, "Victim", Zone::Battlefield);
    let card = creature(&mut state, 1, "Haunter", Zone::Battlefield);
    attach_synth(&mut state, card, &face);

    let mut events = Vec::new();
    move_to_zone(&mut state, card, Zone::Graveyard, &mut events);
    process_triggers(&mut state, &events);

    assert!(
        has_triggered_ability_on_stack(&state),
        "the haunt (exile-haunting) ability must trigger on death"
    );
}

/// CR 702.55a (spell): the haunt ability fires when the spell resolves to the
/// graveyard from the stack — the key spell-form firing assumption.
#[test]
fn haunt_ability_fires_when_spell_resolves_to_graveyard() {
    let mut face = haunt_spell_face();
    synthesize_haunt(&mut face);
    let mut state = GameState::new_two_player(1);
    state.active_player = PlayerId(0);
    state.phase = crate::types::phase::Phase::PostCombatMain;
    let _victim = creature(&mut state, 9, "Victim", Zone::Battlefield);
    let card = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Haunt Spell".into(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&card).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.base_card_types.core_types.push(CoreType::Instant);
    }
    attach_synth(&mut state, card, &face);

    let mut events = Vec::new();
    move_to_zone(&mut state, card, Zone::Graveyard, &mut events);
    process_triggers(&mut state, &events);

    assert!(
        has_triggered_ability_on_stack(&state),
        "the spell-form haunt ability must trigger when it resolves to the graveyard"
    );
}

/// CR 702.55c: the haunt-payoff fires from exile when the haunted creature dies.
#[test]
fn haunt_payoff_fires_from_exile_when_haunted_creature_dies() {
    let mut face = haunt_creature_face();
    synthesize_haunt(&mut face);
    let mut state = GameState::new_two_player(1);
    state.active_player = PlayerId(0);
    state.phase = crate::types::phase::Phase::PostCombatMain;
    // CR 603.3c: the payoff destroys target enchantment — give it a legal target
    // so the trigger is put on the stack.
    let enchantment = create_object(
        &mut state,
        CardId(8),
        PlayerId(0),
        "Some Aura".into(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&enchantment).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types.core_types.push(CoreType::Enchantment);
    }

    // The haunting card sits in exile carrying its synthesized payoff trigger.
    let card = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Ghost".into(),
        Zone::Exile,
    );
    attach_synth(&mut state, card, &face);
    let haunted = creature(&mut state, 2, "Haunted", Zone::Battlefield);
    add_haunt_link(&mut state, card, haunted);

    let mut events = Vec::new();
    move_to_zone(&mut state, haunted, Zone::Graveyard, &mut events);
    process_triggers(&mut state, &events);

    assert!(
        has_triggered_ability_on_stack(&state),
        "the haunt payoff must trigger from exile when the haunted creature dies"
    );
}

// ---------------------------------------------------------------------------
// Real-pipeline integration (MTGJSON -> parse -> synthesize)
// ---------------------------------------------------------------------------

use crate::database::mtgjson::{AtomicCard, AtomicIdentifiers};

fn atomic(name: &str, types: &[&str], type_line: &str, oracle: &str) -> AtomicCard {
    AtomicCard {
        name: name.to_string(),
        mana_cost: Some("{2}{W}{B}".to_string()),
        colors: vec!["W".to_string(), "B".to_string()],
        color_identity: vec!["W".to_string(), "B".to_string()],
        text: Some(oracle.to_string()),
        power: types.contains(&"Creature").then(|| "2".to_string()),
        toughness: types.contains(&"Creature").then(|| "2".to_string()),
        loyalty: None,
        defense: None,
        layout: "normal".to_string(),
        type_line: Some(type_line.to_string()),
        types: types.iter().map(|s| s.to_string()).collect(),
        subtypes: Vec::new(),
        supertypes: Vec::new(),
        keywords: Some(vec!["Haunt".to_string()]),
        side: None,
        face_name: None,
        mana_value: 4.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some(format!("{name}-oracle")),
            scryfall_id: Some(format!("{name}-face")),
        },
        foreign_data: Vec::new(),
        related_cards: crate::database::mtgjson::SetRelatedCards::default(),
    }
}

/// Real creature-form haunt card (Absolver Thrull shape).
#[test]
fn real_creature_haunt_card_synthesizes_ability_and_payoff() {
    let card = atomic(
        "Absolver Thrull",
        &["Creature"],
        "Creature — Thrull Cleric",
        "Haunt (When this creature dies, exile it haunting target creature.)\n\
         When this creature enters or the creature it haunts dies, destroy target enchantment.",
    );
    let face = crate::database::synthesis::build_oracle_face(&card, None);
    assert!(face.keywords.iter().any(|k| matches!(k, Keyword::Haunt)));
    assert!(
        face.triggers.iter().any(|t| t
            .execute
            .as_ref()
            .is_some_and(|a| matches!(a.effect.as_ref(), Effect::ExileHaunting { .. }))),
        "creature haunt must synthesize the exile-haunting ability"
    );
    assert!(
        face.triggers
            .iter()
            .any(|t| matches!(t.mode, TriggerMode::HauntedCreatureDies)),
        "creature haunt must synthesize the payoff trigger"
    );
    assert!(!crate::game::coverage::card_face_has_unimplemented_parts(
        &face
    ));
}

/// Real spell-form haunt card (Cry of Contrition shape).
#[test]
fn real_spell_haunt_card_synthesizes_ability_and_payoff() {
    let card = atomic(
        "Cry of Contrition",
        &["Sorcery"],
        "Sorcery",
        "Target player discards a card.\n\
         Haunt (When this spell card is put into a graveyard after resolving, exile it haunting \
         target creature.)\n\
         When the creature this card haunts dies, target player discards a card.",
    );
    let face = crate::database::synthesis::build_oracle_face(&card, None);
    assert!(face.keywords.iter().any(|k| matches!(k, Keyword::Haunt)));
    let haunt = face.triggers.iter().find(|t| {
        t.execute
            .as_ref()
            .is_some_and(|a| matches!(a.effect.as_ref(), Effect::ExileHaunting { .. }))
    });
    let haunt = haunt.expect("spell haunt must synthesize the exile-haunting ability");
    assert_eq!(haunt.origin, Some(Zone::Stack));
    let payoff = face
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::HauntedCreatureDies))
        .expect("spell haunt must parse the payoff trigger");
    assert_eq!(payoff.trigger_zones, vec![Zone::Exile]);
}
