//! Regression test for GitHub issue #2425 — Fable of the Mirror-Breaker chapter III.
//!
//! Chapter III: "Exile this Saga, then return it to the battlefield transformed
//! under your control." The reported bug: the Saga returns as its front face
//! with a lore counter instead of entering as Reflection of Kiki-Jiki.
//!
//! CR 712.14a: A double-faced card put onto the battlefield transformed enters
//! with its back face up. CR 714.3a lore-counter ETB replacement applies only to
//! Sagas entering the battlefield — not to the transformed creature back face.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::zones::Zone;
use engine::types::Keyword;

fn reflection_back_face() -> BackFaceData {
    BackFaceData {
        name: "Reflection of Kiki-Jiki".to_string(),
        power: Some(2),
        toughness: Some(2),
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Goblin".to_string(), "Shaman".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Red],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

fn etching_back_face() -> BackFaceData {
    BackFaceData {
        name: "Etching of Kumano".to_string(),
        power: Some(2),
        toughness: Some(2),
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Enchantment, CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Shaman".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![Keyword::Haste],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Red],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

#[test]
fn fable_chapter_three_without_back_face_stays_exiled() {
    let execute = parse_effect_chain(
        "Exile this Saga, then return it to the battlefield transformed under your control.",
        AbilityKind::Spell,
    );

    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let fable_id = {
        let state = runner.state_mut();
        let id = create_object(
            state,
            CardId(2),
            P0,
            "Fable of the Mirror-Breaker".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.counters.insert(CounterType::Lore, 3);
        id
    };

    let resolved = build_resolved_from_def(&execute, fable_id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter III resolves");

    let fable = &runner.state().objects[&fable_id];
    assert_eq!(
        fable.zone,
        Zone::Exile,
        "a single-faced copy instructed to return transformed must remain in exile"
    );
    assert!(
        !fable.transformed,
        "a single-faced object cannot enter transformed"
    );
    assert!(
        fable.card_types.subtypes.iter().any(|s| s == "Saga"),
        "must remain the saga front face when transform cannot fire"
    );
    assert_eq!(
        fable.counters.get(&CounterType::Lore).copied().unwrap_or(0),
        0,
        "counters from the exiled Saga must cease to exist"
    );
}

#[test]
fn fable_chapter_three_returns_transformed_not_as_saga() {
    let parsed_fable = parse_oracle_text(
        "I — Create a 2/2 red Goblin Shaman creature token.\nII — You may discard up to two cards. If you do, draw that many cards.\nIII — Exile this Saga, then return it to the battlefield transformed under your control.",
        "Fable of the Mirror-Breaker",
        &[],
        &["Enchantment".to_string()],
        &["Saga".to_string()],
    );
    assert_eq!(parsed_fable.replacements.len(), 1);

    let execute = parse_effect_chain(
        "Exile this Saga, then return it to the battlefield transformed under your control.",
        AbilityKind::Spell,
    );
    let sub = execute.sub_ability.as_ref().expect("return sub");
    match &*sub.effect {
        Effect::ChangeZone {
            enter_transformed, ..
        } => assert!(enter_transformed, "parser must set enter_transformed"),
        other => panic!("expected return ChangeZone, got {other:?}"),
    }

    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let fable_id = {
        let state = runner.state_mut();
        let id = create_object(
            state,
            CardId(1),
            P0,
            "Fable of the Mirror-Breaker".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.counters.insert(CounterType::Lore, 3);
        obj.counters.insert(CounterType::Plus1Plus1, 1);
        obj.replacement_definitions = parsed_fable.replacements.clone().into();
        obj.base_replacement_definitions = parsed_fable.replacements.into();
        obj.back_face = Some(reflection_back_face());
        id
    };

    let resolved = build_resolved_from_def(&execute, fable_id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter III resolves");

    let fable = &runner.state().objects[&fable_id];
    assert_eq!(
        fable.zone,
        Zone::Battlefield,
        "Fable must return to the battlefield after chapter III exile"
    );
    assert!(
        fable.transformed,
        "Fable must enter transformed as Reflection of Kiki-Jiki (CR 712.14a)"
    );
    assert_eq!(fable.name, "Reflection of Kiki-Jiki");
    assert!(
        fable.card_types.core_types.contains(&CoreType::Creature),
        "transformed back face must be a creature, got {:?}",
        fable.card_types.core_types
    );
    assert!(
        !fable.card_types.subtypes.iter().any(|s| s == "Saga"),
        "transformed back face must not remain a Saga"
    );
    assert_eq!(
        fable.counters.get(&CounterType::Lore).copied().unwrap_or(0),
        0,
        "lore counters must not persist on the transformed creature back face"
    );
    assert_eq!(
        fable
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "counters from the exiled Saga must not persist on the returned creature"
    );
}

#[test]
fn kumano_chapter_three_returns_etching_without_lore() {
    let parsed_kumano = parse_oracle_text(
        "I — This Saga deals 1 damage to each opponent and each planeswalker they control.\nII — When you next cast a creature spell this turn, that creature enters with an additional +1/+1 counter on it.\nIII — Exile this Saga, then return it to the battlefield transformed under your control.",
        "Kumano Faces Kakkazan",
        &[],
        &["Enchantment".to_string()],
        &["Saga".to_string()],
    );
    assert_eq!(parsed_kumano.replacements.len(), 1);

    let execute = parse_effect_chain(
        "Exile this Saga, then return it to the battlefield transformed under your control.",
        AbilityKind::Spell,
    );

    let mut runner = GameScenario::new().build();
    let kumano_id = {
        let state = runner.state_mut();
        let id = create_object(
            state,
            CardId(3),
            P0,
            "Kumano Faces Kakkazan".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.counters.insert(CounterType::Lore, 3);
        obj.counters.insert(CounterType::Plus1Plus1, 1);
        obj.replacement_definitions = parsed_kumano.replacements.clone().into();
        obj.base_replacement_definitions = parsed_kumano.replacements.into();
        obj.back_face = Some(etching_back_face());
        id
    };

    let resolved = build_resolved_from_def(&execute, kumano_id, P0);
    resolve_ability_chain(runner.state_mut(), &resolved, &mut Vec::new(), 0)
        .expect("Kumano chapter III resolves");

    let etching = &runner.state().objects[&kumano_id];
    assert_eq!(etching.zone, Zone::Battlefield);
    assert!(etching.transformed);
    assert_eq!(etching.name, "Etching of Kumano");
    assert!(etching.card_types.core_types.contains(&CoreType::Creature));
    assert!(etching
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    assert!(!etching.card_types.subtypes.iter().any(|s| s == "Saga"));
    assert_eq!(etching.power, Some(2));
    assert_eq!(etching.toughness, Some(2));
    assert!(etching.keywords.contains(&Keyword::Haste));
    assert_eq!(
        etching
            .counters
            .get(&CounterType::Lore)
            .copied()
            .unwrap_or(0),
        0,
        "the returned Etching is not a Saga and must not receive a lore counter"
    );
    assert_eq!(
        etching
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "counters from the exiled Saga must not persist on Etching"
    );
}
