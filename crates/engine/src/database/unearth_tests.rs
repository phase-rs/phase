//! Tests for Unearth (CR 702.84) synthesis and runtime. Declared from
//! `database/mod.rs` so the implementation module (`unearth.rs`) stays free of
//! inline test scaffolding.

use std::sync::Arc;

use super::unearth::synthesize_unearth;
use crate::database::mtgjson::{AtomicCard, AtomicIdentifiers};
use crate::game::casting::{can_activate_ability_now, handle_activate_ability};
use crate::game::keywords::has_haste;
use crate::game::layers::evaluate_layers;
use crate::game::stack::resolve_top;
use crate::game::triggers::check_delayed_triggers;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, DelayedTriggerCondition, Effect,
    TargetFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaCostShard};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

// ---------------------------------------------------------------------------
// Synthesis-shape tests (building-block level)
// ---------------------------------------------------------------------------

fn face_with(keyword: Keyword) -> CardFace {
    let mut face = CardFace::default();
    face.keywords.push(keyword);
    face
}

/// Walk the `sub_ability` continuation chain into a flat list of effects, in
/// resolution order (primary first).
fn chain_effects(def: &AbilityDefinition) -> Vec<&Effect> {
    let mut effects = vec![def.effect.as_ref()];
    let mut node = def.sub_ability.as_deref();
    while let Some(step) = node {
        effects.push(step.effect.as_ref());
        node = step.sub_ability.as_deref();
    }
    effects
}

fn static_grants_haste_to_self(effect: &Effect) -> bool {
    let Effect::GenericEffect {
        static_abilities, ..
    } = effect
    else {
        return false;
    };
    static_abilities.iter().any(|s| {
        s.affected == Some(TargetFilter::SelfRef)
            && s.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste
                    }
                )
            })
    })
}

/// CR 702.84a: a synthesized Unearth ability is a sorcery-speed
/// graveyard-activated mana-cost reanimation chain: return self to the
/// battlefield, gain haste, exile at the next end step, and an "exile if it
/// would leave the battlefield" replacement — all bound to `SelfRef`.
#[test]
fn synthesize_unearth_builds_graveyard_activated_sorcery_chain() {
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 2,
    };
    let mut face = face_with(Keyword::Unearth(cost.clone()));
    synthesize_unearth(&mut face);

    assert_eq!(face.abilities.len(), 1, "exactly one synthesized ability");
    let def = &face.abilities[0];

    // CR 702.84a: activate only as a sorcery, only from the graveyard.
    assert_eq!(def.activation_zone, Some(Zone::Graveyard));
    assert!(def.is_sorcery_speed(), "activate only as a sorcery");

    // CR 602.1a: the activation cost is the keyword mana cost (no exile cost —
    // Unearth returns the card as its effect, not as a cost).
    assert!(
        matches!(def.cost.as_ref(), Some(AbilityCost::Mana { cost: c }) if c == &cost),
        "cost must be exactly the keyword mana cost, got {:?}",
        def.cost
    );

    let effects = chain_effects(def);
    assert_eq!(effects.len(), 4, "primary + three continuation steps");

    // (1) CR 702.84a: return this card from graveyard to the battlefield.
    assert!(
        matches!(
            effects[0],
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                enters_under: None,
                ..
            }
        ),
        "primary effect must return SelfRef graveyard->battlefield, got {:?}",
        effects[0]
    );

    // (2) CR 702.84a: it gains haste.
    assert!(
        static_grants_haste_to_self(effects[1]),
        "second step must grant haste to SelfRef, got {:?}",
        effects[1]
    );

    // (3) CR 702.84a: exile it at the beginning of the next end step.
    match effects[2] {
        Effect::CreateDelayedTrigger {
            condition,
            effect,
            uses_tracked_set,
        } => {
            assert_eq!(
                *condition,
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
            );
            assert!(!uses_tracked_set);
            assert!(
                matches!(
                    effect.effect.as_ref(),
                    Effect::ChangeZone {
                        origin: Some(Zone::Battlefield),
                        destination: Zone::Exile,
                        target: TargetFilter::SelfRef,
                        ..
                    }
                ),
                "delayed trigger must exile SelfRef from the battlefield, got {:?}",
                effect.effect
            );
        }
        other => panic!("third step must be a delayed exile trigger, got {other:?}"),
    }

    // (4) CR 702.84a: if it would leave the battlefield, exile it instead.
    match effects[3] {
        Effect::AddTargetReplacement {
            replacement,
            target,
        } => {
            assert_eq!(
                *target,
                TargetFilter::SelfRef,
                "install on the returned card"
            );
            assert_eq!(replacement.event, ReplacementEvent::Moved);
            assert_eq!(replacement.valid_card, Some(TargetFilter::SelfRef));
            // No destination restriction — ANY battlefield exit is redirected.
            assert_eq!(replacement.destination_zone, None);
            let execute = replacement
                .execute
                .as_deref()
                .expect("replacement carries a redirect effect");
            assert!(
                matches!(
                    execute.effect.as_ref(),
                    Effect::ChangeZone {
                        origin: Some(Zone::Battlefield),
                        destination: Zone::Exile,
                        target: TargetFilter::SelfRef,
                        ..
                    }
                ),
                "replacement must redirect SelfRef to exile, got {:?}",
                execute.effect
            );
        }
        other => panic!("fourth step must add the leaves-battlefield replacement, got {other:?}"),
    }
}

/// Cards without Unearth are untouched.
#[test]
fn synthesize_unearth_is_noop_without_keyword() {
    let mut face = face_with(Keyword::Flying);
    synthesize_unearth(&mut face);
    assert!(face.abilities.is_empty());
}

/// CR 113.2c: each printed Unearth keyword yields its own ability.
#[test]
fn synthesize_unearth_emits_one_ability_per_keyword() {
    let mut face = CardFace::default();
    face.keywords.push(Keyword::Unearth(ManaCost::generic(1)));
    face.keywords.push(Keyword::Unearth(ManaCost::generic(3)));
    synthesize_unearth(&mut face);
    assert_eq!(face.abilities.len(), 2);
}

// ---------------------------------------------------------------------------
// Runtime tests (end-to-end activation)
// ---------------------------------------------------------------------------

/// Put a creature carrying a free-cost (synthesized) Unearth ability into the
/// owner's graveyard, ready to activate.
fn setup_graveyard_creature(state: &mut GameState) -> ObjectId {
    let source = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Unearthed Beast".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = state.objects.get_mut(&source).unwrap();
        obj.power = Some(3);
        obj.base_power = Some(3);
        obj.toughness = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
    }
    let mut face = CardFace::default();
    face.keywords.push(Keyword::Unearth(ManaCost::default()));
    synthesize_unearth(&mut face);
    Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).extend(face.abilities);
    source
}

fn main_phase_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.phase = Phase::PreCombatMain;
    state
}

/// Activate the source's Unearth ability and resolve it off the stack.
fn activate_and_resolve(state: &mut GameState, source: ObjectId) {
    let mut events = Vec::new();
    handle_activate_ability(state, PlayerId(0), source, 0, &mut events)
        .expect("Unearth activation must succeed");
    assert!(!state.stack.is_empty(), "the ability is on the stack");
    resolve_top(state, &mut Vec::new());
}

/// CR 702.84a: Unearth is activatable from the graveyard in the sorcery window.
#[test]
fn unearth_activatable_from_graveyard_at_sorcery_speed() {
    let mut state = main_phase_state();
    let source = setup_graveyard_creature(&mut state);
    assert!(
        can_activate_ability_now(&state, PlayerId(0), source, 0),
        "Unearth must be activatable from the graveyard at sorcery speed"
    );
}

/// CR 702.84a: "Activate only as a sorcery" — rejected with the stack non-empty
/// / outside the main phase.
#[test]
fn unearth_rejects_instant_speed() {
    let mut state = main_phase_state();
    state.phase = Phase::Upkeep;
    let source = setup_graveyard_creature(&mut state);
    assert!(
        !can_activate_ability_now(&state, PlayerId(0), source, 0),
        "Unearth must reject activation outside the sorcery-speed window"
    );
}

/// CR 702.84a: activating Unearth returns the card to the battlefield (same
/// `ObjectId`) under its owner's control, and it gains haste.
#[test]
fn unearth_returns_creature_to_battlefield_with_haste() {
    let mut state = main_phase_state();
    let source = setup_graveyard_creature(&mut state);

    activate_and_resolve(&mut state, source);

    // CR 400.7: storage identity persists, so the returned permanent is `source`.
    assert_eq!(state.objects[&source].zone, Zone::Battlefield);
    assert!(state.battlefield.contains(&source));
    assert_eq!(state.objects[&source].controller, PlayerId(0));

    // CR 702.84a: it gains haste (Layer 6 continuous grant).
    evaluate_layers(&mut state);
    assert!(
        has_haste(&state.objects[&source]),
        "the unearthed creature must have haste"
    );
}

/// CR 702.84a: "Exile it at the beginning of the next end step." Firing the
/// end-step `PhaseChanged` event resolves the delayed exile.
#[test]
fn unearth_creature_exiled_at_next_end_step() {
    let mut state = main_phase_state();
    let source = setup_graveyard_creature(&mut state);
    activate_and_resolve(&mut state, source);
    assert_eq!(state.objects[&source].zone, Zone::Battlefield);

    // The end step begins: fire the delayed "exile this" trigger and resolve it.
    state.phase = Phase::End;
    let stacked =
        check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
    assert!(
        !stacked.is_empty(),
        "the delayed exile trigger must fire at the end step"
    );
    resolve_top(&mut state, &mut Vec::new());

    assert_eq!(
        state.objects[&source].zone,
        Zone::Exile,
        "the unearthed creature must be exiled at the next end step"
    );
    assert!(!state.battlefield.contains(&source));
}

/// CR 702.84a: the "if it would leave the battlefield, exile it instead"
/// replacement is installed on the returned permanent — `Moved` event,
/// `valid_card: SelfRef`, no destination restriction (any exit), redirecting to
/// exile. (The redirect application itself is owned and covered by the
/// replacement engine; here we prove Unearth installs the correct rider.)
#[test]
fn unearth_installs_leaves_battlefield_exile_replacement() {
    let mut state = main_phase_state();
    let source = setup_graveyard_creature(&mut state);
    activate_and_resolve(&mut state, source);

    let installed = &state.objects[&source].replacement_definitions;
    let repl = installed
        .iter_all()
        .find(|r| r.event == ReplacementEvent::Moved)
        .expect("a Moved replacement must be installed on the returned permanent");
    assert_eq!(repl.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        repl.destination_zone, None,
        "any battlefield exit is redirected, not just one destination"
    );
    let execute = repl.execute.as_deref().expect("redirect effect present");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "the replacement must redirect the card to exile, got {:?}",
        execute.effect
    );
}

// ---------------------------------------------------------------------------
// Real-pipeline integration (MTGJSON -> parse -> synthesize)
// ---------------------------------------------------------------------------

/// Build a minimal real `AtomicCard` for an Unearth creature so the production
/// path (`build_oracle_face` -> `synthesize_all` -> `synthesize_unearth`) is
/// exercised end to end.
fn atomic_unearth_creature() -> AtomicCard {
    AtomicCard {
        name: "Hellspark Elemental".to_string(),
        mana_cost: Some("{1}{R}".to_string()),
        colors: vec!["R".to_string()],
        color_identity: vec!["R".to_string()],
        text: Some(
            "Trample, haste\n\
             At the beginning of the end step, sacrifice Hellspark Elemental.\n\
             Unearth {1}{R} ({1}{R}: Return this card from your graveyard to the battlefield. \
             It gains haste. Exile it at the beginning of the next end step or if it would leave \
             the battlefield. Unearth only as a sorcery.)"
                .to_string(),
        ),
        power: Some("3".to_string()),
        toughness: Some("1".to_string()),
        loyalty: None,
        defense: None,
        layout: "normal".to_string(),
        type_line: Some("Creature — Elemental".to_string()),
        types: vec!["Creature".to_string()],
        subtypes: vec!["Elemental".to_string()],
        supertypes: Vec::new(),
        keywords: Some(vec![
            "Trample".to_string(),
            "Haste".to_string(),
            "Unearth".to_string(),
        ]),
        side: None,
        face_name: None,
        mana_value: 2.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some("hellspark-oracle".to_string()),
            scryfall_id: Some("hellspark-face".to_string()),
        },
        foreign_data: Vec::new(),
    }
}

/// A real Unearth card parses the keyword and synthesizes a graveyard-activated
/// reanimation ability with no `Unimplemented` parts (genuinely runnable).
#[test]
fn real_unearth_card_synthesizes_reanimation_ability() {
    let atomic = atomic_unearth_creature();
    let face = crate::database::synthesis::build_oracle_face(&atomic, None);

    assert!(
        face.keywords
            .iter()
            .any(|k| matches!(k, Keyword::Unearth(_))),
        "Unearth keyword must parse from MTGJSON"
    );

    let def = face
        .abilities
        .iter()
        .find(|d| {
            d.activation_zone == Some(Zone::Graveyard)
                && matches!(
                    d.effect.as_ref(),
                    Effect::ChangeZone {
                        destination: Zone::Battlefield,
                        target: TargetFilter::SelfRef,
                        ..
                    }
                )
        })
        .expect("a graveyard-activated reanimation ability must be synthesized");
    assert!(def.is_sorcery_speed(), "activate only as a sorcery");

    assert!(
        !crate::game::coverage::card_face_has_unimplemented_parts(&face),
        "face must have no Unimplemented parts after synthesis"
    );
}
