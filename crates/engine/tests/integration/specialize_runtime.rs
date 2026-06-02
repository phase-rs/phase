//! Digital-only Specialize runtime: pay cost, discard, choose color, apply face.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::game::specialize::SpecializeFaceMap;
use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr};
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

#[test]
fn specialize_applies_chosen_face_and_emits_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario
        .add_creature(P0, "Test Student", 1, 1)
        .with_keyword(Keyword::Specialize(ManaCost::NoCost))
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, Effect::Specialize)
                .cost(AbilityCost::Composite {
                    costs: vec![AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: None,
                        random: false,
                        self_ref: false,
                    }],
                })
                .sorcery_speed(),
        )
        .id();

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
