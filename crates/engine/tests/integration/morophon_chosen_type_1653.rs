//! Issue #1653: Morophon +1/+1 must apply to multi-subtype creatures (e.g. Human Warrior).
//!
//! https://github.com/phase-rs/phase/issues/1653

use engine::game::layers::{evaluate_layers, flush_layers};
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    ChoiceType, ChosenAttribute, ContinuousModification, ControllerRef, FilterProp,
    StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;

fn morophon_buff_filter() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::Another, FilterProp::IsChosenCreatureType]),
    )
}

fn add_morophon_static(state: &mut engine::types::game_state::GameState, morophon: ObjectId) {
    state
        .objects
        .get_mut(&morophon)
        .unwrap()
        .static_definitions
        .push(
            StaticDefinition::continuous()
                .affected(morophon_buff_filter())
                .modifications(vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::AddToughness { value: 1 },
                ]),
        );
}

fn add_chosen_name_power_static(
    state: &mut engine::types::game_state::GameState,
    source: ObjectId,
) {
    state
        .objects
        .get_mut(&source)
        .unwrap()
        .static_definitions
        .push(
            StaticDefinition::continuous()
                .affected(TargetFilter::HasChosenName)
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );
}

// Shape test: manually constructs expected state.
#[test]
fn morophon_buffs_creature_with_matching_subtype_among_many() {
    let mut scenario = GameScenario::new();
    let morophon = scenario
        .add_creature(P0, "Morophon, the Boundless", 5, 5)
        .id();
    let bruse = scenario
        .add_creature(P0, "Bruse Tarl, Roving Rancher", 4, 3)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&morophon)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::CreatureType("Human".to_string()));
        add_morophon_static(state, morophon);
        let bruse_obj = state.objects.get_mut(&bruse).unwrap();
        bruse_obj.card_types.subtypes = vec!["Human".to_string(), "Warrior".to_string()];
        bruse_obj.base_card_types = bruse_obj.card_types.clone();
    }

    evaluate_layers(runner.state_mut());

    let bruse_obj = runner.state().objects.get(&bruse).unwrap();
    assert_eq!(
        bruse_obj.power,
        Some(5),
        "Human Warrior must get +1/+1 when Morophon chose Human (#1653)"
    );
    assert_eq!(bruse_obj.toughness, Some(4));
}

#[test]
fn morophon_creature_type_choice_marks_layers_dirty() {
    let mut scenario = GameScenario::new();
    let morophon = scenario
        .add_creature(P0, "Morophon, the Boundless", 5, 5)
        .id();
    let bruse = scenario
        .add_creature(P0, "Bruse Tarl, Roving Rancher", 4, 3)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        add_morophon_static(state, morophon);
        let bruse_obj = state.objects.get_mut(&bruse).unwrap();
        bruse_obj.card_types.subtypes = vec!["Human".to_string(), "Warrior".to_string()];
        bruse_obj.base_card_types = bruse_obj.card_types.clone();
    }

    evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects.get(&bruse).unwrap().power, Some(4));

    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        player: P0,
        choice_type: ChoiceType::creature_type(),
        options: vec!["Human".to_string(), "Elf".to_string()],
        source_id: Some(morophon),
        persist_player: None,
    };
    runner
        .act(GameAction::ChooseOption {
            choice: "Human".to_string(),
        })
        .expect("creature type choice");

    // `mark_layers_full` may already have been flushed during continuation drain;
    // ensure any pending re-evaluation is applied before asserting P/T.
    flush_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects.get(&bruse).unwrap().power,
        Some(5),
        "buff must apply immediately after choosing Human"
    );
}

#[test]
fn card_name_choice_marks_layers_dirty_for_chosen_name_static() {
    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature(P0, "Pithing Needle Stand-In", 1, 1)
        .id();
    let llanowar = scenario.add_creature(P0, "Llanowar Elves", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.all_card_names = vec!["Llanowar Elves".to_string()].into();
        add_chosen_name_power_static(state, source);
    }

    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects.get(&llanowar).unwrap().power,
        Some(1)
    );

    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        player: P0,
        choice_type: ChoiceType::CardName,
        options: Vec::new(),
        source_id: Some(source),
        persist_player: None,
    };
    runner
        .act(GameAction::ChooseOption {
            choice: "Llanowar Elves".to_string(),
        })
        .expect("card name choice");
    flush_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects.get(&llanowar).unwrap().power,
        Some(2),
        "chosen-name continuous effects must update immediately after CardName choice"
    );
}
