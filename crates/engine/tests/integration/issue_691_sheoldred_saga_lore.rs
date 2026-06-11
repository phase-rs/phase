//! Regression for GitHub issue #691 — Sheoldred transform → The True Scriptures.
//!
//! When Sheoldred's activated ability exiles and returns the card transformed,
//! it enters as the Saga back face (The True Scriptures). CR 714.3a requires a
//! lore counter on ETB; chapter I fires from that counter. The bug: intrinsic ETB
//! counter seeding read the front face (creature) before the transform swap, so
//! no lore counter was added and chapter abilities never triggered.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHEOLDRED_ACTIVATE: &str = "{4}{B}: Exile this permanent, then return it to the battlefield transformed. Activate only as a sorcery.\n\
Activate only if an opponent has eight or more cards in their graveyard.";

fn true_scriptures_back_face() -> BackFaceData {
    BackFaceData {
        name: "The True Scriptures".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Enchantment],
            subtypes: vec!["Saga".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Black],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, generic: u32, black: u32) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let state = runner.state_mut();
    let pool = &mut state.players[0].mana_pool;
    for _ in 0..generic {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
    for _ in 0..black {
        pool.add(ManaUnit::new(ManaType::Black, dummy, false, vec![]));
    }
}

#[test]
fn transformed_saga_back_face_entry_receives_lore_counter() {
    let execute = parse_effect_chain(
        "Exile this permanent, then return it to the battlefield transformed under your control.",
        AbilityKind::Spell,
    );

    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let sheoldred_id = {
        let state = runner.state_mut();
        let id = engine::game::zones::create_object(
            state,
            CardId(1),
            P0,
            "Sheoldred".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Phyrexian".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.back_face = Some(true_scriptures_back_face());
        id
    };

    let resolved = build_resolved_from_def(&execute, sheoldred_id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("exile then return transformed resolves");

    let saga = &runner.state().objects[&sheoldred_id];
    assert_eq!(saga.zone, Zone::Battlefield);
    assert!(
        saga.transformed,
        "must enter showing the Saga back face (CR 712.14a)"
    );
    assert_eq!(saga.name, "The True Scriptures");
    assert!(
        saga.card_types.subtypes.iter().any(|s| s == "Saga"),
        "transformed back face must be a Saga"
    );
    assert_eq!(
        saga.counters.get(&CounterType::Lore).copied().unwrap_or(0),
        1,
        "CR 714.3a: Saga entering the battlefield must receive a lore counter"
    );
}

#[test]
fn sheoldred_activate_returns_true_scriptures_with_lore_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for i in 0..8 {
        scenario.add_creature_to_graveyard(P1, &format!("Graveyard Filler {i}"), 1, 1);
    }

    let sheoldred_id = scenario
        .add_creature_from_oracle(P0, "Sheoldred", 4, 4, SHEOLDRED_ACTIVATE)
        .id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&sheoldred_id).unwrap();
        obj.back_face = Some(true_scriptures_back_face());
        obj.summoning_sick = false;
    }

    add_mana(&mut runner, 4, 1);
    runner.activate(sheoldred_id, 0).resolve();

    let saga = &runner.state().objects[&sheoldred_id];
    assert_eq!(saga.zone, Zone::Battlefield);
    assert!(saga.transformed);
    assert_eq!(saga.name, "The True Scriptures");
    assert_eq!(
        saga.counters.get(&CounterType::Lore).copied().unwrap_or(0),
        1,
        "Sheoldred transform must seed the Saga lore counter on The True Scriptures"
    );
}
