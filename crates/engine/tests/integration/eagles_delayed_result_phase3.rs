//! Instruction-local return results frozen into a next-upkeep delayed trigger.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, ChoiceType, DelayedTriggerCondition, Effect,
    PtValue, QuantityExpr, RepeatContinuation, ReplacementDefinition, ReturnResultId,
    SubAbilityLink, TargetFilter, TargetRef, TargetSelectionMode, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::PersistedGameState;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::{ResolutionFrame, ResolutionStateWire};
use engine::types::zones::{EtbTapState, Zone};

const EAGLES: &str = "Kicker {2}{W}{W} (You may pay an additional {2}{W}{W} as you cast this spell.)\nChoose target creature you own. If this spell was kicked, instead choose any number of target creatures you own. Return each chosen creature to your hand. At the beginning of the next upkeep, create a 4/4 white Bird Soldier creature token with flying for each creature returned to your hand this way.";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const INTERLEAVED_RETURNS: &str = "Choose target creature you own. Return each chosen creature to your hand. Choose target artifact you own. Return each chosen artifact to your hand. At the beginning of the next upkeep, create a 4/4 white Bird Soldier creature token with flying for each creature returned to your hand this way.";
const DISTINCT_RETURN_READERS: &str = "Choose target creature you own. Return each chosen creature to your hand. Return target land you control to your hand. Choose target artifact you own. Return each chosen artifact to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each artifact returned to your hand this way.";
const CONDITIONAL_RETURN_WITNESS: &str = "Choose target creature you own. Return each chosen creature to your hand. At the beginning of the next upkeep, create a 4/4 white Bird Soldier creature token with flying for each creature returned to your hand this way.";
const TAINTED_PACT_PROCESS: &str = "Exile the top card of your library. You may put that card into your hand unless it has the same name as another card exiled this way. Repeat this process until you put a card into your hand or you exile two cards with the same name, whichever comes first.";

fn bird_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| object.is_token && object.name.contains("Bird Soldier"))
        .count()
}

#[test]
fn repeated_eagles_process_keeps_each_return_result_separate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let selected = scenario.add_creature(P0, "Repeated Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    for abilities in [&mut object.abilities, &mut object.base_abilities] {
        let root = &mut Arc::make_mut(abilities)[0];
        root.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
        root.sub_ability
            .as_mut()
            .expect("selected return is part of the parsed process")
            .sub_link = SubAbilityLink::ContinuationStep;
    }
    let outcome = runner.cast(spell).target_object(selected).resolve();
    outcome.assert_zone(&[selected], Zone::Hand);
    let counts: Vec<_> = outcome
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            effect => panic!("expected frozen delayed tokens, got {effect:?}"),
        })
        .collect();
    assert_eq!(
        counts,
        [1, 0],
        "each iteration must read its own settled return; waiting={:?}, stack={:?}",
        outcome.state().waiting_for,
        outcome.state().resolution_stack
    );
    assert!(outcome.state().return_result_frames.is_empty());
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
}

#[test]
fn repeated_return_after_zone_choice_and_reload_keeps_each_result() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let first = scenario.add_creature(P0, "First Repeated Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Repeated Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut returned = definitions[0].clone();
    while returned.declares_return_result.is_none() {
        returned = *returned
            .sub_ability
            .expect("the parsed chain contains the return producer");
    }
    returned.reads_chosen_group = None;
    returned.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
    returned.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: Some(QuantityExpr::Fixed { value: 1 }),
    });
    returned
        .sub_ability
        .as_mut()
        .expect("the delayed reader follows the return")
        .sub_link = SubAbilityLink::ContinuationStep;
    definitions[0] = returned;
    object.base_abilities = object.abilities.clone();
    let outcome = runner.cast(spell).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            assert!(cards.contains(&first));
            assert!(cards.contains(&second));
        }
        other => panic!("the first repeated return must prompt for a creature: {other:?}"),
    }
    assert!(outcome.state().delayed_triggers.is_empty());
    assert!(!outcome.state().return_result_frames.is_empty());

    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("paused repeated return serializes");
    let selected_publisher = saved["resolution_frames"]["frames"]
        .as_array()
        .expect("typed resolution frames")
        .iter()
        .find(|frame| {
            frame["type"] == "AbilityContinuation"
                && frame["data"]["pending"]["pending_return_result_producer"].is_array()
        })
        .expect("selected return retains its exact producer key");
    let occurrence = &selected_publisher["data"]["pending"]["return_result_occurrence"];
    assert_eq!(
        &selected_publisher["data"]["pending"]["pending_return_result_producer"][0],
        occurrence
    );
    let restored: ResolutionStateWire =
        serde_json::from_value(saved).expect("paused repeated return restores");
    *runner.state_mut() = restored.into_game_state();
    runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("choose the first creature for iteration one");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    let counts: Vec<_> = runner
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            effect => panic!("expected frozen delayed tokens, got {effect:?}"),
        })
        .collect();
    assert_eq!(
        counts,
        [1, 1],
        "each iteration must publish its own settled return"
    );
    assert!(runner.state().return_result_frames.is_empty());
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 2);
}

#[test]
fn selected_return_result_survives_redirect_pause_and_rejects_corrupt_carriers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let redirected = scenario
        .add_creature(P0, "Redirected Selection Bear", 2, 2)
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Graveyard))
        .id();
    let returned = scenario
        .add_creature(P0, "Returned Selection Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut producer = definitions[0].clone();
    while producer.declares_return_result.is_none() {
        producer = *producer
            .sub_ability
            .expect("printed return producer is present");
    }
    producer.reads_chosen_group = None;
    producer.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
    producer.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: Some(QuantityExpr::Fixed { value: 1 }),
    });
    producer
        .sub_ability
        .as_mut()
        .expect("delayed reader")
        .sub_link = SubAbilityLink::ContinuationStep;
    definitions[0] = producer;
    object.base_abilities = object.abilities.clone();

    let first_prompt = runner.cast(spell).resolve();
    assert!(matches!(
        first_prompt.final_waiting_for(),
        WaitingFor::EffectZoneChoice { cards, .. }
            if cards.contains(&redirected) && cards.contains(&returned)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![redirected],
        })
        .expect("choose the redirecting creature");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(runner.state().delayed_triggers.is_empty());

    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("selected return's replacement pause serializes");
    let frames = saved["resolution_frames"]["frames"]
        .as_array()
        .expect("typed resolution frames");
    let zone = frames
        .iter()
        .find(|frame| {
            frame["type"] == "ChangeZone"
                && frame["data"]["pending"]["pending_return_result_producer"].is_array()
        })
        .expect("zone iteration owns the transferred producer key");
    let key = zone["data"]["pending"]["pending_return_result_producer"].clone();
    let occurrence = key[0].as_u64().expect("occurrence id");
    let result = key[1].as_u64().expect("result id");
    let reader = frames
        .iter()
        .find(|frame| {
            frame["type"] == "AbilityContinuation"
                && frame["data"]["pending"]["return_result_occurrence"] == occurrence
        })
        .expect("reader is parked beneath the unsettled zone iteration");
    assert!(reader["data"]["pending"]["pending_return_result_producer"].is_null());
    assert!(reader["data"]["pending"]["chain"]
        .to_string()
        .contains("reads_return_result"));

    let mut corruptions = Vec::new();
    let mut duplicate = saved.clone();
    for frame in duplicate["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if frame["type"] == "AbilityContinuation" {
            frame["data"]["pending"]["pending_return_result_producer"] = key.clone();
        }
    }
    corruptions.push((duplicate, "duplicate parked publishers"));

    let mut prepublished = saved.clone();
    prepublished["return_result_frames"][occurrence.to_string()]
        .as_object_mut()
        .expect("return-result frame")
        .insert(result.to_string(), serde_json::json!([]));
    corruptions.push((prepublished, "already published"));

    let mut missing = saved.clone();
    missing["return_result_frames"]
        .as_object_mut()
        .expect("return-result arena")
        .remove(&occurrence.to_string());
    corruptions.push((missing, "missing occurrence"));

    let mut mismatched = saved.clone();
    let other_occurrence = mismatched["next_return_result_occurrence_id"]
        .as_u64()
        .expect("next occurrence id");
    mismatched["next_return_result_occurrence_id"] = serde_json::json!(other_occurrence + 1);
    mismatched["return_result_frames"]
        .as_object_mut()
        .expect("return-result arena")
        .insert(other_occurrence.to_string(), serde_json::json!({}));
    for frame in mismatched["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if frame["type"] == "AbilityContinuation" {
            frame["data"]["pending"]["return_result_occurrence"] =
                serde_json::json!(other_occurrence);
        }
    }
    corruptions.push((
        mismatched,
        "disagrees with its continuation occurrence or reader",
    ));

    let mut wrong_reader = saved.clone();
    for frame in wrong_reader["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if frame["type"] == "ChangeZone"
            && frame["data"]["pending"]["pending_return_result_producer"].is_array()
        {
            frame["data"]["pending"]["pending_return_result_producer"][1] =
                serde_json::json!(result + 100);
        }
    }
    corruptions.push((
        wrong_reader,
        "disagrees with its continuation occurrence or reader",
    ));

    for (corrupt, expected) in corruptions {
        for envelope in [corrupt.clone(), serde_json::json!({ "state": corrupt })] {
            let error = serde_json::from_value::<PersistedGameState>(envelope)
                .expect_err("corrupt transferred publisher must fail at restore");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?}, got {error}"
            );
        }
    }

    let mut legacy = saved.clone();
    for frame in legacy["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if matches!(
            frame["type"].as_str(),
            Some("AbilityContinuation" | "ChangeZone")
        ) {
            frame["data"]["pending"]
                .as_object_mut()
                .expect("pending owner")
                .remove("pending_return_result_producer");
        }
    }
    let _: ResolutionStateWire =
        serde_json::from_value(legacy).expect("legacy save without producer keys decodes");
    let restored: ResolutionStateWire =
        serde_json::from_value(saved).expect("valid transferred key survives reload");
    *runner.state_mut() = restored.into_game_state();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect selected creature away from hand");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().objects[&redirected].zone,
        Zone::Exile | Zone::Graveyard
    ));
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    let counts: Vec<_> = runner
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            effect => panic!("expected frozen delayed tokens, got {effect:?}"),
        })
        .collect();
    assert_eq!(counts, [0, 1]);
    assert!(runner.state().return_result_frames.is_empty());
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
}

#[test]
fn selected_return_result_keeps_one_key_across_two_zone_repauses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let first = scenario
        .add_creature(P0, "First Redirected Bear", 2, 2)
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Graveyard))
        .id();
    let second = scenario
        .add_creature(P0, "Second Redirected Bear", 2, 2)
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Graveyard))
        .id();
    let untouched = scenario.add_creature(P0, "Unchosen Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut producer = definitions[0].clone();
    while producer.declares_return_result.is_none() {
        producer = *producer.sub_ability.expect("return producer");
    }
    producer.reads_chosen_group = None;
    producer.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: Some(QuantityExpr::Fixed { value: 2 }),
    });
    producer
        .sub_ability
        .as_mut()
        .expect("delayed reader")
        .sub_link = SubAbilityLink::ContinuationStep;
    definitions[0] = producer;
    object.base_abilities = object.abilities.clone();

    let prompt = runner.cast(spell).resolve();
    assert!(matches!(
        prompt.final_waiting_for(),
        WaitingFor::EffectZoneChoice { cards, count: 2, .. }
            if cards.contains(&first) && cards.contains(&second) && cards.contains(&untouched)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("choose two redirecting creatures");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let mut original_key = None;
    for remaining in [1, 0] {
        let saved =
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .expect("repaused selected return serializes");
        let zone = saved["resolution_frames"]["frames"]
            .as_array()
            .expect("typed frames")
            .iter()
            .find(|frame| {
                frame["type"] == "ChangeZone"
                    && frame["data"]["pending"]["pending_return_result_producer"].is_array()
            })
            .expect("the zone iteration keeps producer authority");
        let key = zone["data"]["pending"]["pending_return_result_producer"].clone();
        assert_eq!(
            zone["data"]["pending"]["remaining"]
                .as_array()
                .expect("remaining selected members")
                .len(),
            remaining
        );
        if let Some(original_key) = &original_key {
            assert_eq!(&key, original_key, "a re-pause cannot rebind the producer");
        } else {
            original_key = Some(key);
        }
        let restored: ResolutionStateWire =
            serde_json::from_value(saved).expect("the exact key survives each reload");
        *runner.state_mut() = restored.into_game_state();
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("settle the current selected member");
    }
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().objects[&first].zone,
        Zone::Exile | Zone::Graveyard
    ));
    assert!(matches!(
        runner.state().objects[&second].zone,
        Zone::Exile | Zone::Graveyard
    ));
    assert_eq!(runner.state().objects[&untouched].zone, Zone::Battlefield);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &runner.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 0 },
            ..
        }
    ));
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn selected_return_result_publishes_present_empty_result_for_permitted_empty_pick() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let first = scenario.add_creature(P0, "First Optional Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Optional Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut producer = definitions[0].clone();
    while producer.declares_return_result.is_none() {
        producer = *producer.sub_ability.expect("return producer");
    }
    producer.reads_chosen_group = None;
    producer.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: Some(QuantityExpr::Fixed { value: 1 }),
    });
    producer
        .sub_ability
        .as_mut()
        .expect("delayed reader")
        .sub_link = SubAbilityLink::ContinuationStep;
    definitions[0] = producer;
    object.base_abilities = object.abilities.clone();

    let prompt = runner.cast(spell).resolve();
    assert!(matches!(
        prompt.final_waiting_for(),
        WaitingFor::EffectZoneChoice { cards, .. }
            if cards.contains(&first) && cards.contains(&second)
    ));
    assert!(runner.state().resolution_stack.iter().any(|frame| matches!(
        frame,
        ResolutionFrame::AbilityContinuation(continuation)
            if continuation.pending.pending_return_result_producer.is_some()
    )));
    // The printed Eagles choice is mandatory. Make this already-produced
    // BounceAll prompt optional to exercise the generic selected-result empty
    // branch without claiming a new Oracle grammar for the card.
    if let WaitingFor::EffectZoneChoice {
        up_to, min_count, ..
    } = &mut runner.state_mut().waiting_for
    {
        *up_to = true;
        *min_count = 0;
    }
    let producer = runner
        .state()
        .resolution_stack
        .iter()
        .find_map(|frame| match frame {
            ResolutionFrame::AbilityContinuation(continuation) => {
                continuation.pending.pending_return_result_producer
            }
            _ => None,
        })
        .expect("selected return has a live producer");
    let displaced_frame = runner
        .state_mut()
        .return_result_frames
        .remove(&producer.0)
        .expect("selected return has a result frame");
    assert!(runner
        .act(GameAction::SelectCards { cards: vec![] })
        .is_err());
    assert!(runner.state().resolution_stack.iter().any(|frame| matches!(
        frame,
        ResolutionFrame::AbilityContinuation(continuation)
            if continuation.pending.pending_return_result_producer == Some(producer)
    )));
    runner
        .state_mut()
        .return_result_frames
        .insert(producer.0, displaced_frame);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("permitted empty choice settles the named instruction");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &runner.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 0 },
            ..
        }
    ));
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn selected_second_return_uses_its_own_result_key_after_an_earlier_producer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "First Return Relic", "")
        .id();
    let first_creature = scenario.add_creature(P0, "First Choice Bear", 2, 2).id();
    let second_creature = scenario.add_creature(P0, "Second Choice Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut first = definitions[0].clone();
    while first.declares_return_result.is_none() {
        first = *first.sub_ability.expect("return producer");
    }
    let mut second = first.clone();
    let second_result = second.declares_return_result.expect("second result id");
    second.reads_chosen_group = None;
    second.sub_link = SubAbilityLink::SequentialSibling;
    second.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: Some(QuantityExpr::Fixed { value: 1 }),
    });
    second
        .sub_ability
        .as_mut()
        .expect("delayed reader")
        .sub_link = SubAbilityLink::ContinuationStep;
    first.reads_chosen_group = None;
    first.declares_return_result = Some(ReturnResultId(100));
    first.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
        destination: None,
        count: None,
    });
    first.sub_ability = Some(Box::new(second));
    definitions[0] = first;
    object.base_abilities = object.abilities.clone();

    let prompt = runner.cast(spell).resolve();
    assert!(matches!(
        prompt.final_waiting_for(),
        WaitingFor::EffectZoneChoice { cards, .. }
            if cards.contains(&first_creature) && cards.contains(&second_creature)
    ));
    assert_eq!(prompt.state().objects[&artifact].zone, Zone::Hand);
    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("second producer prompt serializes");
    let key = saved["resolution_frames"]["frames"]
        .as_array()
        .expect("typed frames")
        .iter()
        .find_map(|frame| {
            frame["data"]["pending"]["pending_return_result_producer"]
                .as_array()
                .cloned()
        })
        .expect("selected second producer key");
    assert_eq!(key[1], serde_json::json!(second_result.0));
    assert_ne!(key[1], serde_json::json!(100));
    let occurrence = key[0].as_u64().expect("shared occurrence");
    assert_eq!(
        saved["return_result_frames"][occurrence.to_string()]["100"]
            .as_array()
            .expect("first producer already settled")
            .len(),
        1
    );
    assert!(saved["return_result_frames"][occurrence.to_string()]
        .get(second_result.0.to_string())
        .is_none());

    runner
        .act(GameAction::SelectCards {
            cards: vec![first_creature],
        })
        .expect("select the second producer's creature");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&first_creature].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&second_creature].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &runner.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn repeated_return_after_replacement_choice_and_reload_keeps_each_result() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let redirected = scenario
        .add_creature(P0, "Redirected Bear", 2, 2)
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Graveyard))
        .id();
    let returned = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut returned_ability = definitions[0].clone();
    while returned_ability.declares_return_result.is_none() {
        returned_ability = *returned_ability
            .sub_ability
            .expect("the parsed chain contains the return producer");
    }
    returned_ability.reads_chosen_group = None;
    returned_ability.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
    returned_ability.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: None,
    });
    returned_ability
        .sub_ability
        .as_mut()
        .expect("the delayed reader follows the return")
        .sub_link = SubAbilityLink::ContinuationStep;
    definitions[0] = returned_ability;
    object.base_abilities = object.abilities.clone();
    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(outcome.state().delayed_triggers.is_empty());
    assert!(!outcome.state().return_result_frames.is_empty());
    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("paused repeated return serializes");
    let restored: ResolutionStateWire =
        serde_json::from_value(saved).expect("paused repeated return restores");
    *runner.state_mut() = restored.into_game_state();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose first redirect for iteration one");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().objects[&redirected].zone,
        Zone::Exile | Zone::Graveyard
    ));
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    let counts: Vec<_> = runner
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            effect => panic!("expected frozen delayed tokens, got {effect:?}"),
        })
        .collect();
    assert_eq!(counts, [1, 0]);
    assert!(runner.state().return_result_frames.is_empty());
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
}

#[test]
fn repeated_eagles_controller_choice_and_while_condition_use_new_results() {
    for repeat in [
        RepeatContinuation::ControllerChoice,
        RepeatContinuation::WhileCondition {
            condition: Box::new(AbilityCondition::IsYourTurn),
            max_iterations: Some(1),
        },
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        let selected = scenario.add_creature(P0, "Repeated Bear", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        for abilities in [&mut object.abilities, &mut object.base_abilities] {
            Arc::make_mut(abilities)[0].repeat_until = Some(repeat.clone());
        }
        let outcome = runner.cast(spell).target_object(selected).resolve();
        outcome.assert_zone(&[selected], Zone::Hand);
        if matches!(repeat, RepeatContinuation::ControllerChoice) {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::RepeatDecision { .. }
            ));
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("accept second process iteration");
            assert!(runner.state().return_result_frames.len() <= 1);
            for _ in 0..3 {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept another process iteration");
                assert!(
                    runner.state().return_result_frames.len() <= 1,
                    "completed child occurrences must retire while another repeat prompt remains"
                );
            }
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .expect("stop repeated process");
        }
        let counts: Vec<_> = runner
            .state()
            .delayed_triggers
            .iter()
            .map(|trigger| match &trigger.ability.effect {
                Effect::Token {
                    count: QuantityExpr::Fixed { value },
                    ..
                } => *value,
                effect => panic!("expected frozen delayed tokens, got {effect:?}"),
            })
            .collect();
        let expected = if matches!(repeat, RepeatContinuation::ControllerChoice) {
            vec![1, 0, 0, 0, 0]
        } else {
            vec![1, 0]
        };
        assert_eq!(counts, expected, "repeat mode {repeat:?}");
        assert!(runner.state().return_result_frames.is_empty());
        runner.advance_to_phase(Phase::Upkeep);
        runner.advance_until_stack_empty();
        assert_eq!(bird_count(&runner), 1);
    }
}

#[test]
fn repeated_until_stop_conditions_keeps_each_return_result_separate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let returned = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let first = scenario
        .add_spell_to_library_top(P0, "Matching Bolt", true)
        .id();
    let second = scenario
        .add_spell_to_library_top(P0, "Matching Bolt", true)
        .id();
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_PROCESS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let parsed_eagles = parse_oracle_text(
        EAGLES,
        "The Eagles Are Coming!",
        &[],
        &["Instant".into()],
        &[],
    );
    let mut returned_ability = parsed_eagles.abilities[0].clone();
    while returned_ability.declares_return_result.is_none() {
        returned_ability = *returned_ability
            .sub_ability
            .expect("the parsed Eagles chain contains a return producer");
    }
    returned_ability.reads_chosen_group = None;
    returned_ability.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: None,
    });
    returned_ability
        .sub_ability
        .as_mut()
        .expect("the delayed reader follows the return")
        .sub_link = SubAbilityLink::ContinuationStep;
    let object = runner.state_mut().objects.get_mut(&pact).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let root = &mut definitions[0];
    assert!(matches!(
        root.repeat_until,
        Some(RepeatContinuation::UntilStopConditions { .. })
    ));
    let mut tail = root;
    while tail.sub_ability.is_some() {
        tail = tail.sub_ability.as_mut().unwrap();
    }
    tail.sub_link = SubAbilityLink::ContinuationStep;
    tail.sub_ability = Some(Box::new(returned_ability));
    object.base_abilities = object.abilities.clone();

    let outcome = runner.cast(pact).decline_optional().resolve();
    outcome.assert_zone(&[first, second], Zone::Exile);
    outcome.assert_zone(&[returned], Zone::Hand);
    let counts: Vec<_> = outcome
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            effect => panic!("expected frozen delayed tokens, got {effect:?}"),
        })
        .collect();
    assert_eq!(counts, [1, 0]);
    assert!(outcome.state().return_result_frames.is_empty());
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
}

#[test]
fn nested_repeat_does_not_shadow_an_outer_named_return_result() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let creature = scenario.add_creature(P0, "Outer Bear", 2, 2).id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Inner Relic", "")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut outer = definitions[0].clone();
    while outer.declares_return_result.is_none() {
        outer = *outer
            .sub_ability
            .expect("the parsed chain contains the return producer");
    }
    outer.reads_chosen_group = None;
    outer.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::creature()),
        destination: None,
        count: None,
    });
    let outer_reader = outer
        .sub_ability
        .take()
        .expect("the delayed reader follows the return");
    let mut inner = outer.clone();
    inner.declares_return_result = Some(ReturnResultId(100));
    inner.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
    inner.effect = Box::new(Effect::BounceAll {
        target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
        destination: None,
        count: None,
    });
    inner.sub_ability = Some(outer_reader);
    outer.sub_ability = Some(Box::new(inner));
    definitions[0] = outer;
    object.base_abilities = object.abilities.clone();

    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[creature, artifact], Zone::Hand);
    assert_eq!(outcome.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &outcome.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
    assert!(outcome.state().return_result_frames.is_empty());
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
}

#[test]
fn repeated_process_without_named_results_does_not_allocate_occurrences() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Repeated Life", true, "You gain 1 life.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    for abilities in [&mut object.abilities, &mut object.base_abilities] {
        Arc::make_mut(abilities)[0].repeat_until = Some(RepeatContinuation::ControllerChoice);
    }
    let starting_life = runner.state().players[P0.0 as usize].life;
    runner.cast(spell).resolve();
    for _ in 0..3 {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::RepeatDecision { .. }
        ));
        assert!(runner.state().return_result_frames.is_empty());
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("repeat a result-free instruction");
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("stop result-free repeat");
    assert!(runner.state().return_result_frames.is_empty());
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        starting_life + 4
    );
}

#[test]
fn eagles_preserves_printed_delayed_token_and_binds_exact_return() {
    let parsed = parse_oracle_text(
        EAGLES,
        "The Eagles Are Coming!",
        &[],
        &["Instant".into()],
        &[],
    );
    let mut node = &parsed.abilities[0];
    let mut producer = None;
    let mut delayed = None;
    loop {
        if node.declares_return_result.is_some() {
            producer = Some(node);
        }
        if node.reads_return_result.is_some() {
            delayed = Some(node);
        }
        match node.sub_ability.as_deref() {
            Some(next) => node = next,
            None => break,
        }
    }
    let producer = producer.expect("selected return declares its result");
    let delayed = delayed.expect("delayed clause reads the result");
    assert_eq!(
        delayed.reads_return_result.as_ref().map(|(id, _)| *id),
        producer.declares_return_result
    );
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = &*delayed.effect
    else {
        panic!("delayed wrapper must survive parsing: {delayed:#?}");
    };
    assert!(matches!(
        condition,
        DelayedTriggerCondition::AtNextPhase {
            phase: Phase::Upkeep
        }
    ));
    let Effect::Token {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        count,
        ..
    } = &*effect.effect
    else {
        panic!("printed token body must survive parsing: {effect:#?}");
    };
    assert!(name.contains("Bird Soldier"));
    assert_eq!(power, &PtValue::Fixed(4));
    assert_eq!(toughness, &PtValue::Fixed(4));
    assert!(types.iter().any(|kind| kind == "Bird"));
    assert!(types.iter().any(|kind| kind == "Soldier"));
    assert_eq!(colors, &[ManaColor::White]);
    assert!(keywords.contains(&Keyword::Flying));
    assert!(!matches!(count, QuantityExpr::Fixed { value: 1 }));
}

#[test]
fn delayed_return_reader_rejects_wrong_noun_destination_and_missing_producer() {
    for oracle in [
        EAGLES.replace("for each creature returned", "for each artifact returned"),
        EAGLES.replace("returned to your hand this way", "returned to your graveyard this way"),
        "At the beginning of the next upkeep, create a 4/4 white Bird Soldier creature token with flying for each creature returned to your hand this way.".to_string(),
    ] {
        let parsed = parse_oracle_text(&oracle, "Delayed Return Witness", &[], &["Instant".into()], &[]);
        let mut cursor = &parsed.abilities[0];
        while let Some(next) = cursor.sub_ability.as_deref() {
            cursor = next;
        }
        assert!(matches!(&*cursor.effect, Effect::Unimplemented { .. }), "{oracle}: {cursor:#?}");
        assert!(cursor.reads_return_result.is_none());
    }
}

#[test]
fn leading_conditional_chosen_return_remains_strict_unsupported() {
    let oracle = CONDITIONAL_RETURN_WITNESS.replacen(
        "Return each chosen creature to your hand.",
        "If you control a Bird, return each chosen creature to your hand.",
        1,
    );
    let parsed = parse_oracle_text(
        &oracle,
        "Leading Conditional Return Witness",
        &[],
        &["Instant".into()],
        &[],
    );
    let mut node = &parsed.abilities[0];
    let mut strict = false;
    loop {
        strict |= matches!(&*node.effect, Effect::Unimplemented { .. });
        match node.sub_ability.as_deref() {
            Some(next) => node = next,
            None => break,
        }
    }
    assert!(strict, "conditional return must not silently claim support");
}

#[test]
fn eagles_one_return_creates_one_bird_at_the_next_upkeep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let selected = scenario.add_creature(P0, "Selected Bear", 2, 2).id();
    let other = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(selected).resolve();
    outcome.assert_zone(&[selected], Zone::Hand);
    outcome.assert_zone(&[other], Zone::Battlefield);
    assert_eq!(outcome.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &outcome.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
    assert_eq!(bird_count(&runner), 0);
    let mut external_move_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        selected,
        Zone::Graveyard,
        &mut external_move_events,
    );
    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
    assert!(runner.state().delayed_triggers.is_empty());
}

#[test]
fn all_invalid_eagles_targets_fizzle_without_installing_a_delayed_reader() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let mut committed = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[first, second])
        .commit();
    let mut external_move_events = Vec::new();
    engine::game::zones::move_to_zone(
        committed.state_mut(),
        first,
        Zone::Graveyard,
        &mut external_move_events,
    );
    engine::game::zones::move_to_zone(
        committed.state_mut(),
        second,
        Zone::Graveyard,
        &mut external_move_events,
    );
    let outcome = committed.resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event, GameEvent::StackResolved { object_id } if *object_id == spell
    )));
    outcome.assert_zone(&[first, second], Zone::Graveyard);
    assert!(outcome.state().delayed_triggers.is_empty());
    assert!(outcome.state().return_result_frames.is_empty());
}

#[test]
fn kicked_zero_and_two_returns_freeze_independent_token_counts() {
    for chosen_count in [0, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
        let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
        let unchosen = scenario.add_creature(P0, "Unchosen Bear", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
            .with_mana_cost(ManaCost::zero())
            .id();
        scenario.with_mana_pool(
            P0,
            (0..4)
                .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        let cast = runner.cast(spell).accept_optional();
        let outcome = if chosen_count == 0 {
            cast.resolve()
        } else {
            cast.target_objects(&[first, second]).resolve()
        };
        if chosen_count == 0 {
            outcome.assert_zone(&[first, second, unchosen], Zone::Battlefield);
        } else {
            outcome.assert_zone(&[first, second], Zone::Hand);
            outcome.assert_zone(&[unchosen], Zone::Battlefield);
        }
        assert_eq!(outcome.state().delayed_triggers.len(), 1);
        assert!(matches!(
            &outcome.state().delayed_triggers[0].ability.effect,
            Effect::Token { count: QuantityExpr::Fixed { value }, .. } if *value == chosen_count
        ));
        runner.advance_to_phase(Phase::Upkeep);
        assert_eq!(runner.state().phase, Phase::Upkeep);
        runner.advance_until_stack_empty();
        assert_eq!(bird_count(&runner), chosen_count as usize);
        assert!(runner.state().delayed_triggers.is_empty());
    }
}

#[test]
fn skipped_return_publishes_empty_before_independent_delayed_reader() {
    // The parser does not claim a leading conditional-return grammar here.
    // Add a typed condition to its successfully parsed return instruction so
    // the cast pipeline exercises a skipped producer followed by an independent
    // delayed reader. The alternative case guards one-result publication when
    // an else branch executes the same producer instead.
    for (cast_zone, alternative_returns, expected_count) in [
        (Zone::Hand, false, 1),
        (Zone::Exile, false, 0),
        (Zone::Exile, true, 1),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        scenario.with_library_top(P0, &["P0 Draw A", "P0 Draw B"]);
        scenario.with_library_top(P1, &["P1 Draw A", "P1 Draw B"]);
        let selected = scenario.add_creature(P0, "Selected Bear", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Conditional Return Witness",
                true,
                CONDITIONAL_RETURN_WITNESS,
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        let definitions = Arc::make_mut(&mut object.abilities);
        let mut producer = definitions
            .iter_mut()
            .find(|definition| matches!(definition.kind, AbilityKind::Spell))
            .expect("printed spell ability");
        while producer.declares_return_result.is_none() {
            producer = producer
                .sub_ability
                .as_deref_mut()
                .expect("parsed chain must contain a return producer");
        }
        assert!(matches!(&*producer.effect, Effect::BounceAll { .. }));
        assert!(
            producer.else_ability.is_none(),
            "return producer already has an alternative: {:?}",
            producer.else_ability
        );
        assert_eq!(
            producer.sub_ability.as_ref().map(|sub| sub.sub_link),
            Some(SubAbilityLink::SequentialSibling),
            "the delayed reader must remain independent of the return gate"
        );
        assert!(
            producer.sub_ability.as_ref().unwrap().condition.is_none(),
            "delayed reader unexpectedly has a gate: {:?}",
            producer.sub_ability.as_ref().unwrap().condition
        );
        if alternative_returns {
            producer.else_ability = Some(Box::new(producer.clone()));
        }
        producer.condition = Some(AbilityCondition::WasCast {
            zone: Some(cast_zone),
        });
        object.base_abilities = object.abilities.clone();
        let outcome = runner.cast(spell).target_object(selected).resolve();
        outcome.assert_zone(
            &[selected],
            if expected_count == 0 {
                Zone::Battlefield
            } else {
                Zone::Hand
            },
        );
        assert_eq!(
            outcome.state().delayed_triggers.len(),
            1,
            "cast zone {cast_zone:?}, alternative {alternative_returns}"
        );
        assert!(matches!(
            &outcome.state().delayed_triggers[0].ability.effect,
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } if *value == expected_count
        ));
        runner.advance_to_phase(Phase::Upkeep);
        runner.advance_until_stack_empty();
        assert_eq!(bird_count(&runner), expected_count as usize);
        assert!(runner.state().delayed_triggers.is_empty());
        runner.advance_to_phase(Phase::Draw);
        runner.advance_to_phase(Phase::Upkeep);
        runner.advance_until_stack_empty();
        assert_eq!(bird_count(&runner), expected_count as usize);
    }
}

#[test]
fn declined_optional_return_publishes_empty_before_independent_delayed_reader() {
    // The parsed chain supplies the result binding and independent reader;
    // marking only its return producer optional exercises the runtime choice
    // without claiming support for an additional Oracle grammar.
    for (accept_return, alternative_returns, expected_count) in
        [(false, false, 0), (true, false, 1), (false, true, 1)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        scenario.with_library_top(P0, &["P0 Draw A", "P0 Draw B"]);
        scenario.with_library_top(P1, &["P1 Draw A", "P1 Draw B"]);
        let selected = scenario.add_creature(P0, "Selected Bear", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Optional Return Witness",
                true,
                CONDITIONAL_RETURN_WITNESS,
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        let definitions = Arc::make_mut(&mut object.abilities);
        let mut producer = definitions
            .iter_mut()
            .find(|definition| matches!(definition.kind, AbilityKind::Spell))
            .expect("printed spell ability");
        while producer.declares_return_result.is_none() {
            producer = producer
                .sub_ability
                .as_deref_mut()
                .expect("parsed chain must contain a return producer");
        }
        assert!(matches!(&*producer.effect, Effect::BounceAll { .. }));
        assert_eq!(
            producer.sub_ability.as_ref().map(|sub| sub.sub_link),
            Some(SubAbilityLink::SequentialSibling),
            "the delayed reader must remain independent of the optional return"
        );
        if alternative_returns {
            let mut alternative = producer.clone();
            alternative.optional = false;
            alternative.else_ability = None;
            producer.else_ability = Some(Box::new(alternative));
        }
        producer.optional = true;
        object.base_abilities = object.abilities.clone();

        let cast = runner.cast(spell).target_object(selected);
        let outcome = if accept_return {
            cast.accept_optional().resolve()
        } else {
            cast.decline_optional().resolve()
        };
        outcome.assert_zone(
            &[selected],
            if expected_count == 0 {
                Zone::Battlefield
            } else {
                Zone::Hand
            },
        );
        assert_eq!(
            outcome.state().delayed_triggers.len(),
            1,
            "accept return {accept_return}, alternative {alternative_returns}"
        );
        assert!(matches!(
            &outcome.state().delayed_triggers[0].ability.effect,
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } if *value == expected_count
        ));
        runner.advance_to_phase(Phase::Upkeep);
        runner.advance_until_stack_empty();
        assert_eq!(bird_count(&runner), expected_count as usize);
        assert!(runner.state().delayed_triggers.is_empty());
    }
}

#[test]
fn optional_return_prompt_restores_exact_result_occurrence() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let selected = scenario.add_creature(P0, "Selected Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Optional Return Save Witness",
            true,
            CONDITIONAL_RETURN_WITNESS,
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    let definitions = Arc::make_mut(&mut object.abilities);
    let mut producer = definitions
        .iter_mut()
        .find(|definition| matches!(definition.kind, AbilityKind::Spell))
        .expect("printed spell ability");
    while producer.declares_return_result.is_none() {
        producer = producer
            .sub_ability
            .as_deref_mut()
            .expect("parsed chain must contain a return producer");
    }
    producer.optional = true;
    object.base_abilities = object.abilities.clone();

    runner.cast(spell).target_object(selected).commit();
    runner.resolve_top();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("parked optional return serializes");
    let occurrence = saved["resolution_frames"]["frames"]
        .as_array()
        .expect("typed frames")
        .iter()
        .find(|frame| frame["type"] == "OptionalEffect")
        .and_then(|frame| frame["data"]["return_result_occurrence"].as_u64())
        .expect("optional frame captures its named-result occurrence");
    assert!(saved["return_result_frames"][occurrence.to_string()].is_object());

    for (index, (mut corrupt, expected)) in [
        (saved.clone(), "optional effect has no occurrence stamp"),
        (saved.clone(), "optional effect names missing occurrence"),
    ]
    .into_iter()
    .enumerate()
    {
        let frame = corrupt["resolution_frames"]["frames"]
            .as_array_mut()
            .expect("typed frames")
            .iter_mut()
            .find(|frame| frame["type"] == "OptionalEffect")
            .expect("parked optional frame");
        if index == 0 {
            frame["data"]
                .as_object_mut()
                .expect("optional frame data")
                .remove("return_result_occurrence");
        } else {
            frame["data"]["return_result_occurrence"] = serde_json::json!(999_u64);
        }
        let error = serde_json::from_value::<PersistedGameState>(corrupt)
            .expect_err("malformed optional occurrence must fail at restore");
        assert!(error.to_string().contains(expected), "{error}");
    }

    let restored: ResolutionStateWire =
        serde_json::from_value(saved).expect("valid parked optional return restores");
    *runner.state_mut() = restored.into_game_state();
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining after reload resumes the original result frame");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&selected].zone, Zone::Battlefield);
    assert!(matches!(
        &runner.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 0 },
            ..
        }
    ));
}

#[test]
fn token_creature_counts_from_its_prior_object_record_after_leaving() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let card = scenario.add_creature(P0, "Card Bear", 2, 2).id();
    let token = scenario.add_creature(P0, "Token Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[card, token])
        .resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == token
    )));
    outcome.assert_zone(&[card], Zone::Hand);
    assert!(
        !outcome.state().objects.contains_key(&token),
        "the returned token must cease to exist after its zone change"
    );
    assert!(matches!(
        &outcome.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 2 },
            ..
        }
    ));
}

fn redirect_own_hand_move_to(destination: Zone) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Hand)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

#[test]
fn replacement_redirect_counts_only_final_hand_returns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let redirected = scenario
        .add_creature(P0, "Redirected Bear", 2, 2)
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .id();
    let returned = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let opponent_owned = scenario
        .add_creature(P1, "Borrowed Bear", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[redirected, returned])
        .resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "return paused: {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[redirected], Zone::Exile);
    outcome.assert_zone(&[returned], Zone::Hand);
    outcome.assert_zone(&[opponent_owned], Zone::Battlefield);
    assert!(matches!(
        &outcome.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
}

#[test]
fn competing_redirect_choice_survives_save_and_counts_settled_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let redirected = scenario
        .add_creature(P0, "Redirected Bear", 2, 2)
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Graveyard))
        .id();
    let returned = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[redirected, returned])
        .resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        engine::types::game_state::WaitingFor::ReplacementChoice { .. }
    ));
    assert!(outcome.state().delayed_triggers.is_empty());
    assert!(!outcome.state().return_result_frames.is_empty());
    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("paused return batch serializes");
    let restored: ResolutionStateWire =
        serde_json::from_value(saved).expect("paused return batch restores");
    *runner.state_mut() = restored.into_game_state();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose first redirect");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().objects[&redirected].zone,
        Zone::Exile | Zone::Graveyard
    ));
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &runner.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn paused_return_result_restore_rejects_corrupt_authority_before_resume() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let redirected = scenario
        .add_creature(P0, "Redirected Bear", 2, 2)
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Graveyard))
        .id();
    let returned = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[redirected, returned])
        .resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(!outcome.state().return_result_frames.is_empty());

    let canonical = serde_json::to_value(runner.state()).expect("canonical pause serializes");
    assert!(!runner.state().resolution_stack.is_empty());
    assert!(runner.state().next_return_result_occurrence_id > 1);
    // The production pause has a live result frame before its batch publishes a
    // record. Seed that exact private carrier in a clone to make wire leakage
    // observable even when the pending frame's inner map is still empty.
    let mut leak_probe = runner.state().clone();
    let occurrence_id = *leak_probe
        .return_result_frames
        .keys()
        .next()
        .expect("paused return owns a result frame");
    let secret_name = "PRIVATE_RETURN_RESULT_PROJECTION_CANARY";
    let mut secret_record = leak_probe.objects[&redirected].snapshot_for_zone_change(
        redirected,
        Some(Zone::Battlefield),
        Zone::Hand,
    );
    secret_record.name = secret_name.to_string();
    leak_probe
        .return_result_frames
        .get_mut(&occurrence_id)
        .unwrap()
        .insert(ReturnResultId(u32::MAX), vec![secret_record]);
    leak_probe.active_return_result_occurrence = Some(occurrence_id);
    assert!(serde_json::to_string(&leak_probe)
        .expect("private result carrier serializes")
        .contains(secret_name));
    for projected in [
        filter_state_for_viewer(&leak_probe, P0),
        filter_state_for_viewer(&leak_probe, P1),
        filter_state_for_unseated_viewer(&leak_probe),
    ] {
        assert!(projected.resolution_stack.is_empty());
        assert!(projected.return_result_frames.is_empty());
        assert!(projected.active_return_result_occurrence.is_none());
        assert_eq!(projected.next_return_result_occurrence_id, 1);
        let wire = serde_json::to_value(projected).expect("viewer pause serializes");
        assert!(wire.get("return_result_frames").is_none());
        assert!(wire.get("active_return_result_occurrence").is_none());
        assert_eq!(wire["next_return_result_occurrence_id"], 1);
        assert!(!wire.to_string().contains(secret_name));
    }
    assert_eq!(
        serde_json::to_value(runner.state()).expect("canonical pause still serializes"),
        canonical,
        "viewer projection must not mutate the authoritative pause"
    );

    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("parked production batch serializes");
    let batch_completion = |wire: &serde_json::Value| {
        wire["resolution_frames"]["frames"]
            .as_array()
            .expect("typed resolution frames")
            .iter()
            .find_map(|frame| frame["data"]["completion"].get("RecordInstructionZoneResult"))
            .cloned()
            .expect("production return batch carries a named completion")
    };
    let completion = batch_completion(&saved);
    let occurrence = completion["occurrence_id"].as_u64().expect("occurrence id");
    let result = completion["result_id"].as_u64().expect("result id");
    assert!(completion["settled_records"].is_null());
    assert!(saved["return_result_frames"]
        .get(occurrence.to_string())
        .is_some());
    let continuation = saved["resolution_frames"]["frames"]
        .as_array()
        .expect("typed frames")
        .iter()
        .find(|frame| {
            frame["type"] == "AbilityContinuation"
                && frame["data"]["pending"]["return_result_occurrence"] == occurrence
        })
        .expect("the return reader is parked behind the replacement choice");
    assert!(
        continuation["data"]["pending"]["chain"]
            .to_string()
            .contains("reads_return_result"),
        "the parked continuation must carry a named reader"
    );

    let restored: ResolutionStateWire =
        serde_json::from_value(saved.clone()).expect("unsettled None remains valid");
    let mut saved_v2 = saved.clone();
    saved_v2["resolution_state_version"] = serde_json::json!(2);
    let _: ResolutionStateWire =
        serde_json::from_value(saved_v2).expect("v2 typed parked batch restores");
    let raw: PersistedGameState =
        serde_json::from_value(saved.clone()).expect("raw parked save restores");
    let trusted: PersistedGameState = serde_json::from_value(serde_json::json!({
        "state": saved.clone()
    }))
    .expect("trusted parked save restores");
    assert!(matches!(raw, PersistedGameState::Raw(_)));
    assert!(matches!(trusted, PersistedGameState::Trusted(_)));
    *runner.state_mut() = restored.into_game_state();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("valid parked batch resumes");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &runner.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));

    let mut corruptions = Vec::new();
    let mut missing = saved.clone();
    missing["return_result_frames"]
        .as_object_mut()
        .expect("occurrence arena")
        .remove(&occurrence.to_string());
    corruptions.push((missing, "missing occurrence"));

    let mut zero_allocator = saved.clone();
    zero_allocator["next_return_result_occurrence_id"] = serde_json::json!(0);
    corruptions.push((zero_allocator, "allocator is zero"));

    let mut collided_allocator = saved.clone();
    collided_allocator["next_return_result_occurrence_id"] = serde_json::json!(occurrence);
    corruptions.push((collided_allocator, "outside the allocator"));

    let mut orphan = saved.clone();
    let orphan_id = orphan["next_return_result_occurrence_id"]
        .as_u64()
        .expect("next free occurrence id");
    orphan["next_return_result_occurrence_id"] = serde_json::json!(orphan_id + 1);
    orphan["return_result_frames"]
        .as_object_mut()
        .expect("occurrence arena")
        .insert(orphan_id.to_string(), serde_json::json!({}));
    corruptions.push((orphan, "has no live owner"));

    let mut missing_active = saved.clone();
    missing_active["active_return_result_occurrence"] = serde_json::json!(999_u64);
    corruptions.push((missing_active, "active return-result occurrence"));

    let mut wrong_batch = saved.clone();
    for frame in wrong_batch["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if let Some(completion) = frame["data"]["completion"].get_mut("RecordInstructionZoneResult")
        {
            completion["occurrence_id"] = serde_json::json!(999_u64);
        }
    }
    corruptions.push((wrong_batch, "batch names missing occurrence"));

    let mut premature = saved.clone();
    for frame in premature["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if let Some(completion) = frame["data"]["completion"].get_mut("RecordInstructionZoneResult")
        {
            completion["settled_records"] = serde_json::json!([]);
        }
    }
    corruptions.push((premature, "prematurely settled"));

    let mut duplicate_publication = saved.clone();
    duplicate_publication["return_result_frames"][occurrence.to_string()]
        .as_object_mut()
        .expect("result frame")
        .insert(result.to_string(), serde_json::json!([]));
    corruptions.push((duplicate_publication, "already published"));

    let mut unstamped_reader = saved.clone();
    for frame in unstamped_reader["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if frame["type"] == "AbilityContinuation"
            && frame["data"]["pending"]["return_result_occurrence"] == occurrence
        {
            frame["data"]["pending"]
                .as_object_mut()
                .expect("pending continuation")
                .remove("return_result_occurrence");
        }
    }
    corruptions.push((unstamped_reader, "continuation has no occurrence stamp"));

    let mut wrong_continuation = saved.clone();
    for frame in wrong_continuation["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames")
    {
        if frame["type"] == "AbilityContinuation"
            && frame["data"]["pending"]["return_result_occurrence"] == occurrence
        {
            frame["data"]["pending"]["return_result_occurrence"] = serde_json::json!(999_u64);
        }
    }
    corruptions.push((wrong_continuation, "continuation names missing occurrence"));

    let mut duplicate_batch = saved.clone();
    let frames = duplicate_batch["resolution_frames"]["frames"]
        .as_array_mut()
        .expect("typed frames");
    let publisher = frames
        .iter()
        .find(|frame| {
            frame["data"]["completion"]
                .get("RecordInstructionZoneResult")
                .is_some()
        })
        .expect("parked named publisher")
        .clone();
    frames.insert(0, publisher);
    corruptions.push((duplicate_batch, "duplicate parked publishers"));

    for (corrupt, expected) in corruptions {
        let mut v2 = corrupt.clone();
        v2["resolution_state_version"] = serde_json::json!(2);
        for envelope in [corrupt.clone(), serde_json::json!({ "state": corrupt }), v2] {
            let error = serde_json::from_value::<PersistedGameState>(envelope)
                .expect_err("malformed authority must fail at restore");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?}, got {error}"
            );
        }
    }

    let mut direct = serde_json::to_value(runner.state()).expect("authoritative raw state");
    direct["next_return_result_occurrence_id"] = serde_json::json!(0);
    let error = serde_json::from_value::<engine::types::game_state::GameState>(direct)
        .expect_err("direct authoritative state rejects corrupt allocator");
    assert!(error.to_string().contains("allocator is zero"));
}

#[test]
fn replacement_post_effect_nested_root_preserves_outer_result() {
    let mut replacement = redirect_own_hand_move_to(Zone::Exile);
    replacement
        .execute
        .as_mut()
        .expect("redirect has an execute chain")
        .sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let redirected = scenario
        .add_creature(P0, "Redirected Bear", 2, 2)
        .with_replacement_definition(replacement)
        .id();
    let returned = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[redirected, returned])
        .resolve();
    outcome.assert_zone(&[redirected], Zone::Exile);
    outcome.assert_zone(&[returned], Zone::Hand);
    assert_eq!(outcome.state().players[P0.0 as usize].life, life_before + 1);
    assert_eq!(outcome.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &outcome.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
    assert!(outcome.state().return_result_frames.is_empty());
}

#[test]
fn nested_post_effect_repause_and_reload_preserve_outer_result() {
    let named_choice = || {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::Labeled {
                    options: vec!["first".to_string(), "second".to_string()],
                },
                persist: false,
                selection: TargetSelectionMode::Chosen,
            },
        )
    };
    let mut replacement = redirect_own_hand_move_to(Zone::Exile);
    let mut first_choice = named_choice();
    first_choice.sub_ability = Some(Box::new(named_choice()));
    replacement
        .execute
        .as_mut()
        .expect("redirect has an execute chain")
        .sub_ability = Some(Box::new(first_choice));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let redirected = scenario
        .add_creature(P0, "Redirected Bear", 2, 2)
        .with_replacement_definition(replacement)
        .id();
    let returned = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let initial = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[redirected, returned])
        .resolve();
    assert!(matches!(
        initial.final_waiting_for(),
        WaitingFor::NamedChoice { .. }
    ));
    assert!(initial.state().delayed_triggers.is_empty());
    assert!(!initial.state().return_result_frames.is_empty());
    initial
        .state()
        .resolution_stack
        .validate(&initial.state().waiting_for)
        .expect("nested child and parent continuations preserve stack order");
    for (index, choice) in ["first", "second"].into_iter().enumerate() {
        let saved =
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .expect("nested post-effect pause serializes");
        let restored: ResolutionStateWire =
            serde_json::from_value(saved).expect("nested post-effect pause restores");
        *runner.state_mut() = restored.into_game_state();
        runner
            .act(GameAction::ChooseOption {
                choice: choice.to_string(),
            })
            .expect("answer nested post-effect choice");
        runner
            .state()
            .resolution_stack
            .validate(&runner.state().waiting_for)
            .expect("nested continuation stack stays valid after each choice");
        if index == 0 {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
                "after first post-effect choice: waiting={:?}, delayed={:?}, frames={:?}",
                runner.state().waiting_for,
                runner.state().delayed_triggers,
                runner.state().resolution_stack
            );
            assert!(runner.state().delayed_triggers.is_empty());
        }
    }
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&redirected].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(matches!(
        &runner.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn legacy_save_without_return_result_fields_decodes_with_fresh_identity() {
    let mut state_json =
        serde_json::to_value(GameScenario::new().build().state()).expect("fresh game serializes");
    let object = state_json.as_object_mut().expect("game state is an object");
    object.remove("return_result_frames");
    object.remove("active_return_result_occurrence");
    object.remove("next_return_result_occurrence_id");
    let restored: engine::types::game_state::GameState =
        serde_json::from_value(state_json).expect("old game state decodes");
    assert!(restored.return_result_frames.is_empty());
    assert!(restored.active_return_result_occurrence.is_none());
    assert_eq!(restored.next_return_result_occurrence_id, 1);
}

#[test]
fn two_casts_keep_distinct_frozen_results_until_the_same_next_upkeep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let third = scenario.add_creature(P0, "Third Bear", 2, 2).id();
    let spell_a = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let spell_b = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(spell_a).target_object(first).resolve();
    runner
        .cast(spell_b)
        .accept_optional()
        .target_objects(&[second, third])
        .resolve();
    let counts: Vec<_> = runner
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            other => panic!("expected a frozen token body, got {other:?}"),
        })
        .collect();
    assert_eq!(counts, [1, 2]);
    assert!(runner.state().return_result_frames.is_empty());
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 3);
    assert!(runner.state().delayed_triggers.is_empty());
}

#[test]
fn copied_kicked_spell_freezes_its_own_retargeted_return_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let replacement = scenario.add_creature(P0, "Replacement Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let mut original = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[first, second])
        .commit();
    let copied = original
        .cast(twincast)
        .target_object(spell)
        .commit()
        .resolve();
    assert!(matches!(
        copied.final_waiting_for(),
        WaitingFor::CopyRetarget { .. }
    ));
    let copy_id = copied
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::SpellCopied { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .expect("Twincast produces a spell copy");
    assert!(!copied.events().iter().any(|event| matches!(
        event,
        GameEvent::SpellCast { object_id, .. } if *object_id == copy_id
    )));
    original
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(replacement)),
        })
        .expect("retarget copied first member");
    original
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep copied second member");
    let outcome = original.resolve();
    let mut counts: Vec<_> = outcome
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            other => panic!("expected frozen token body, got {other:?}"),
        })
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, [1, 2]);
    assert_eq!(outcome.state().objects[&first].zone, Zone::Hand);
    assert_eq!(outcome.state().objects[&second].zone, Zone::Hand);
    assert_eq!(outcome.state().objects[&replacement].zone, Zone::Hand);
    assert!(outcome.state().return_result_frames.is_empty());
}

#[test]
fn owner_not_controller_governs_target_and_return_result() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let owned_by_caster = scenario
        .add_creature(P0, "Borrowed by Opponent", 2, 2)
        .controlled_by(P1)
        .id();
    let controlled_by_caster = scenario
        .add_creature(P1, "Borrowed by Caster", 2, 2)
        .controlled_by(P0)
        .id();
    let another_owned_creature = scenario.add_creature(P0, "Another Owned Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut positive_runner = GameRunner::from_state(runner.state().clone());
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast reaches target selection");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("declining kicker reaches target announcement");
    }
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Eagles must ask for an owned creature target, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal = &target_slots[selection.current_slot].legal_targets;
    // CR 601.2c: "you own" checks owner at target announcement, even when
    // another player controls that creature.
    assert!(legal.contains(&TargetRef::Object(owned_by_caster)));
    assert!(legal.contains(&TargetRef::Object(another_owned_creature)));
    assert!(!legal.contains(&TargetRef::Object(controlled_by_caster)));
    let before_illegal = serde_json::to_value(runner.state()).expect("target prompt serializes");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(controlled_by_caster)),
        })
        .expect_err("a controlled but unowned creature is illegal");
    assert_eq!(
        serde_json::to_value(runner.state()).expect("rejected target state serializes"),
        before_illegal,
        "illegal target selection cannot change the pending cast"
    );

    let outcome = positive_runner
        .cast(spell)
        .target_object(owned_by_caster)
        .resolve();
    // CR 608.2c: the instruction returns the chosen creature to its owner's hand.
    outcome.assert_zone(&[owned_by_caster], Zone::Hand);
    outcome.assert_zone(
        &[controlled_by_caster, another_owned_creature],
        Zone::Battlefield,
    );
    assert!(matches!(
        &outcome.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
}

#[test]
fn cast_during_upkeep_waits_for_the_following_upkeep_and_fires_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario.with_library_top(P0, &["P0 Draw A", "P0 Draw B"]);
    scenario.with_library_top(P1, &["P1 Draw A", "P1 Draw B"]);
    let selected = scenario.add_creature(P0, "Selected Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(selected).resolve();
    assert_eq!(bird_count(&runner), 0);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    runner.advance_to_phase(Phase::Draw);
    assert_eq!(runner.state().phase, Phase::Draw);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
    assert!(runner.state().delayed_triggers.is_empty());
    runner.advance_to_phase(Phase::Draw);
    assert_eq!(runner.state().phase, Phase::Draw);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(bird_count(&runner), 1);
}

#[test]
fn interleaved_artifact_return_does_not_feed_creature_result_reader() {
    let parsed = parse_oracle_text(
        INTERLEAVED_RETURNS,
        "Interleaved Return Witness",
        &[],
        &["Instant".into()],
        &[],
    );
    let mut cursor = &parsed.abilities[0];
    let mut creature_result = None;
    let mut artifact_result = None;
    let mut reader = None;
    loop {
        if let Effect::BounceAll { .. } = &*cursor.effect {
            if creature_result.is_none() {
                creature_result = cursor.declares_return_result;
            } else {
                artifact_result = cursor.declares_return_result;
            }
        }
        if cursor.reads_return_result.is_some() {
            reader = cursor.reads_return_result.as_ref().map(|(id, _)| *id);
        }
        match cursor.sub_ability.as_deref() {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(
        reader, creature_result,
        "reader must name the creature return"
    );
    assert!(reader.is_some());
    assert!(
        artifact_result.is_none(),
        "unrelated artifact return is not a producer"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let creature = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Returned Relic", "")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Interleaved Return Witness", true, INTERLEAVED_RETURNS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[creature, artifact])
        .resolve();
    outcome.assert_zone(&[creature, artifact], Zone::Hand);
    assert!(matches!(
        &outcome.state().delayed_triggers[0].ability.effect,
        Effect::Token {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
}

#[test]
fn distinct_same_verb_results_survive_an_interleaved_return() {
    let parsed = parse_oracle_text(
        DISTINCT_RETURN_READERS,
        "Distinct Return Readers",
        &[],
        &["Instant".into()],
        &[],
    );
    let mut node = &parsed.abilities[0];
    let mut producers = Vec::new();
    let mut readers = Vec::new();
    loop {
        if let Some(id) = node.declares_return_result {
            producers.push(id);
        }
        if let Some((id, _)) = &node.reads_return_result {
            readers.push(*id);
        }
        match node.sub_ability.as_deref() {
            Some(next) => node = next,
            None => break,
        }
    }
    assert_eq!(
        producers.len(),
        2,
        "two return instructions declare results"
    );
    assert_ne!(producers[0], producers[1]);
    assert_eq!(readers, producers);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let creature = scenario.add_creature(P0, "Returned Bear", 2, 2).id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Redirected Relic", "")
        .with_replacement_definition(redirect_own_hand_move_to(Zone::Exile))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Distinct Return Readers", true, DISTINCT_RETURN_READERS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[creature, land, artifact])
        .resolve();
    outcome.assert_zone(&[creature, land], Zone::Hand);
    outcome.assert_zone(&[artifact], Zone::Exile);
    assert_eq!(outcome.state().delayed_triggers.len(), 2);
    let mut counts: Vec<_> = outcome
        .state()
        .delayed_triggers
        .iter()
        .map(|trigger| match &trigger.ability.effect {
            Effect::Token {
                count: QuantityExpr::Fixed { value },
                ..
            } => *value,
            other => panic!("expected frozen token body, got {other:?}"),
        })
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, [0, 1]);
    assert!(outcome.state().return_result_frames.is_empty());
}
