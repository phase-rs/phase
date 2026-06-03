//! Digital-only Specialize runtime: pay cost, discard, choose color, apply face.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::game::specialize::SpecializeFaceMap;
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

fn specialize_back(name: &str, color: ManaColor, shard: ManaCostShard) -> BackFaceData {
    BackFaceData {
        name: name.into(),
        power: Some(3),
        toughness: Some(3),
        loyalty: None,
        defense: None,
        card_types: CardType {
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Wizard".to_string()],
            ..Default::default()
        },
        mana_cost: ManaCost::Cost {
            generic: 2,
            shards: vec![shard],
        },
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![color],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

fn add_specialize_creature(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    let mut builder = scenario.add_creature(P0, "Test Student", 1, 1);
    builder.from_oracle_text_with_keywords(&["specialize"], "Specialize {0}");
    builder.id()
}

#[test]
fn specialize_applies_chosen_face_and_emits_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = add_specialize_creature(&mut scenario);

    let discard = scenario
        .add_creature_to_hand(P0, "White Discard", 1, 1)
        .id();

    let mut runner = scenario.build();

    {
        let mut faces = SpecializeFaceMap::new();
        faces.insert(
            ManaColor::White,
            specialize_back(
                "Test Student — White",
                ManaColor::White,
                ManaCostShard::White,
            ),
        );
        faces.insert(
            ManaColor::Blue,
            specialize_back("Test Student — Blue", ManaColor::Blue, ManaCostShard::Blue),
        );
        let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
        obj.specialize_faces = Some(faces);
        runner.state_mut().objects.get_mut(&discard).unwrap().color =
            vec![ManaColor::White, ManaColor::Blue];
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: creature,
            ability_index: 0,
        })
        .expect("activate specialize");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![discard],
        })
        .expect("pay discard cost");

    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::SpecializeColor { .. }
        ) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::SpecializeColor { .. }
    ));

    let result = runner
        .act(GameAction::ChooseSpecializeColor {
            color: ManaColor::White,
        })
        .expect("choose white specialization");

    let obj = runner.state().objects.get(&creature).unwrap();
    assert_eq!(obj.name, "Test Student — White");
    assert_eq!(obj.power, Some(3));
    assert_eq!(obj.specialized_color, Some(ManaColor::White));
    assert!(obj.specialize_faces.is_none());
    assert!(!obj
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Specialize(_))));

    assert!(
        result.events.iter().any(|e| {
            matches!(
                e,
                GameEvent::Specialized { object_id, color }
                    if *object_id == creature && *color == ManaColor::White
            )
        }),
        "expected Specialized event"
    );
}

#[test]
fn specialize_accepts_basic_land_subtype_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = add_specialize_creature(&mut scenario);
    let land = scenario
        .add_land_to_hand(P0, "Breeding Pool")
        .with_subtypes(vec!["Forest", "Island"])
        .id();

    let mut runner = scenario.build();

    {
        let mut faces = SpecializeFaceMap::new();
        faces.insert(
            ManaColor::Green,
            specialize_back(
                "Test Student — Green",
                ManaColor::Green,
                ManaCostShard::Green,
            ),
        );
        runner
            .state_mut()
            .objects
            .get_mut(&creature)
            .unwrap()
            .specialize_faces = Some(faces);
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: creature,
            ability_index: 0,
        })
        .expect("activate specialize with land discard available");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .expect("pay specialize discard with land subtype");

    for _ in 0..8 {
        if runner
            .state()
            .objects
            .get(&creature)
            .is_some_and(|obj| obj.specialized_color == Some(ManaColor::Green))
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    let obj = runner.state().objects.get(&creature).unwrap();
    assert_eq!(obj.name, "Test Student — Green");
    assert_eq!(obj.specialized_color, Some(ManaColor::Green));
}

#[test]
fn specialize_rejects_colorless_nonland_discard_before_cost_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = add_specialize_creature(&mut scenario);
    let colorless = scenario.add_card_to_hand(P0, "Colorless Bauble");

    let mut runner = scenario.build();

    {
        let mut faces = SpecializeFaceMap::new();
        faces.insert(
            ManaColor::White,
            specialize_back(
                "Test Student — White",
                ManaColor::White,
                ManaCostShard::White,
            ),
        );
        runner
            .state_mut()
            .objects
            .get_mut(&creature)
            .unwrap()
            .specialize_faces = Some(faces);
    }

    let err = runner
        .act(GameAction::ActivateAbility {
            source_id: creature,
            ability_index: 0,
        })
        .expect_err("colorless nonland card must not be a legal specialize discard");

    assert!(
        err.to_string().contains("Cannot pay activation cost"),
        "unexpected error: {err}"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(
        runner.state().players[P0.0 as usize]
            .hand
            .contains(&colorless),
        "invalid discard must remain in hand"
    );
}
