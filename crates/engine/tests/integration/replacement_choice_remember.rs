use std::sync::Arc;

use engine::game::engine::{apply, apply_interaction};
use engine::game::layers::mark_layers_full;
use engine::game::public_state::sync_waiting_for;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turn_control::authorized_submitter_for_player;
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, DamageModification, DamageTargetFilter,
    DrawReplacementScope, Effect, QuantityExpr, QuantityModification, QuantityRef,
    ReplacementCondition, ReplacementDefinition, ReplacementMode, StaticDefinition, TargetFilter,
};
use engine::types::actions::{GameAction, ReplacementAutoChoice};
use engine::types::game_state::{
    GameState, PersistedGameState, ReplacementAutoChoiceId, ReplacementAutoChoiceRecord,
    ReplacementChoiceKind, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaType, ManaUnit, StepEndManaAction};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::zones::{EtbTapState, Zone};

fn remember_order(order: &[usize]) -> GameAction {
    GameAction::ChooseReplacementAndRemember {
        choice: ReplacementAutoChoice::Order {
            order: order.to_vec(),
        },
    }
}

fn remember_optional(index: usize) -> GameAction {
    GameAction::ChooseReplacementAndRemember {
        choice: ReplacementAutoChoice::Optional { index },
    }
}

fn move_fixture_object(state: &mut GameState, object: ObjectId, destination: Zone) {
    let before = ObjectIncarnationRef::from_object(&state.objects[&object]);
    assert!(
        !move_object_for_test(
            state,
            ZoneMoveRequest::effect(object, destination, object),
            &mut Vec::new(),
        ),
        "fixture move must complete without a replacement choice"
    );
    assert_eq!(state.objects[&object].zone, destination);
    // CR 400.7: each zone change creates a new object incarnation.
    assert_ne!(
        ObjectIncarnationRef::from_object(&state.objects[&object]),
        before
    );
}

fn replacement_index(state: &GameState, source: ObjectId) -> usize {
    let WaitingFor::ReplacementChoice { candidates, .. } = &state.waiting_for else {
        panic!("expected replacement ordering");
    };
    candidates
        .iter()
        .position(|candidate| candidate.source_id == source)
        .unwrap()
}

fn remember_source_order(state: &GameState, sources: &[ObjectId]) -> GameAction {
    remember_order(
        &sources
            .iter()
            .map(|source| replacement_index(state, *source))
            .collect::<Vec<_>>(),
    )
}

fn life_scenario(optional: bool) -> (GameRunner, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut sources = Vec::new();
    let modifications = if optional {
        vec![QuantityModification::Prevent]
    } else {
        vec![
            QuantityModification::Times { factor: 2 },
            QuantityModification::Plus { value: 1 },
            QuantityModification::Plus { value: 3 },
        ]
    };
    for modification in modifications {
        let definition = ReplacementDefinition::new(ReplacementEvent::GainLife)
            .quantity_modification(modification)
            .mode(if optional {
                ReplacementMode::Optional { decline: None }
            } else {
                ReplacementMode::Mandatory
            });
        sources.push(
            scenario
                .add_creature(P0, "Life modifier", 1, 1)
                .with_replacement_definition(definition)
                .id(),
        );
    }
    let spells = (0..5)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Life spell", true, "You gain 1 life.")
                .id()
        })
        .collect();
    (scenario.build(), sources, spells)
}

fn assert_prompt(state: &GameState, kind: ReplacementChoiceKind, available: bool) {
    let WaitingFor::ReplacementChoice {
        kind: actual,
        remember_identity,
        ..
    } = &state.waiting_for
    else {
        panic!("expected replacement choice, got {:?}", state.waiting_for);
    };
    assert_eq!(*actual, kind);
    assert_eq!(remember_identity.is_some(), available);
}

fn assert_owner_preferences_suspend_under_control(optional: bool) {
    let (mut runner, sources, spells) = life_scenario(optional);
    let kind = if optional {
        ReplacementChoiceKind::OptionalBranch
    } else {
        ReplacementChoiceKind::Order
    };
    assert_eq!(authorized_submitter_for_player(runner.state(), P0), P0);
    runner.cast(spells[0]).resolve();
    assert_prompt(runner.state(), kind, true);
    let action = if optional {
        remember_optional(0)
    } else {
        remember_source_order(runner.state(), &[sources[1], sources[0], sources[2]])
    };
    runner.act(action).unwrap();
    let saved = runner.state().replacement_auto_choices.clone();
    assert_eq!(
        saved.len(),
        1,
        "the owner's preference must exist before control"
    );
    assert_eq!(
        runner.state().players[0].life,
        if optional { 20 } else { 27 }
    );

    {
        let state = runner.state_mut();
        state.turn_decision_controller = Some(P1);
        state.priority_passes.clear();
        sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }
    assert_eq!(authorized_submitter_for_player(runner.state(), P0), P1);
    runner.cast(spells[1]).resolve();
    // CR 723.5b: control grants game decisions, not the owner's saved preferences.
    assert_prompt(runner.state(), kind, false);
    assert_eq!(runner.state().replacement_auto_choices, saved);
    for state in [
        runner.state().clone(),
        filter_state_for_viewer(runner.state(), P0),
        filter_state_for_viewer(runner.state(), P1),
    ] {
        let snapshot = serde_json::to_value(state).unwrap();
        assert!(snapshot["waiting_for"]["data"]["remember_identity"].is_null());
    }
    let action = if optional {
        remember_optional(1)
    } else {
        remember_source_order(runner.state(), &sources)
    };
    let before = serde_json::to_value(runner.state()).unwrap();
    assert!(apply(runner.state_mut(), P1, action.clone()).is_err());
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    assert!(apply_interaction(runner.state_mut(), P1, P0, action).is_err());
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);

    // CR 723.5: the controller still makes ordinary replacement decisions.
    if optional {
        apply(
            runner.state_mut(),
            P1,
            GameAction::ChooseReplacement { index: 1 },
        )
        .unwrap();
    } else {
        for source in &sources {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ) {
                let index = replacement_index(runner.state(), *source);
                apply(
                    runner.state_mut(),
                    P1,
                    GameAction::ChooseReplacement { index },
                )
                .unwrap();
            }
        }
    }
    assert_eq!(
        runner.state().players[0].life,
        if optional { 21 } else { 33 }
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().replacement_auto_choices, saved);

    {
        let state = runner.state_mut();
        state.turn_decision_controller = None;
        state.priority_passes.clear();
        sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }
    assert_eq!(authorized_submitter_for_player(runner.state(), P0), P0);
    let outcome = runner.cast(spells[2]).resolve();
    outcome.assert_life_delta(P0, if optional { 0 } else { 7 });
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(outcome.state().replacement_auto_choices, saved);
    assert!(outcome.state().replacement_auto_choice_tail.is_none());
}

#[test]
fn remembered_order_is_owner_only_and_suspends_until_control_ends() {
    assert_owner_preferences_suspend_under_control(false);
}

#[test]
fn remembered_optional_branch_is_owner_only_and_suspends_until_control_ends() {
    assert_owner_preferences_suspend_under_control(true);
}

#[test]
fn full_order_is_replayed_after_cast_resolution_and_ordinary_choices_are_not_saved() {
    let (mut runner, sources, spells) = life_scenario(false);
    runner.cast(spells[0]).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!();
    };
    let descriptions: Vec<_> = [sources[1], sources[0], sources[2]]
        .iter()
        .map(|source| {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.source_id == *source)
                .unwrap();
            format!("{} — {}", candidate.source_name, candidate.description)
        })
        .collect();
    runner
        .act(remember_source_order(
            runner.state(),
            &[sources[1], sources[0], sources[2]],
        ))
        .unwrap();
    assert_eq!(
        runner.state().replacement_auto_choices[0].descriptions,
        descriptions
    );
    assert_eq!(runner.state().players[0].life, 27, "(1 + 1) * 2 + 3");
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner.cast(spells[1]).resolve();
    assert_eq!(runner.state().players[0].life, 34);
    assert_eq!(runner.state().replacement_auto_choices.len(), 1);
    assert!(runner.state().replacement_auto_choice_tail.is_none());

    runner
        .act(GameAction::SetReplacementAutoChoice { selector: None })
        .unwrap();
    runner.cast(spells[2]).resolve();
    runner
        .act(GameAction::ChooseReplacement {
            index: replacement_index(runner.state(), sources[1]),
        })
        .unwrap();
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    runner
        .act(GameAction::ChooseReplacement {
            index: replacement_index(runner.state(), sources[0]),
        })
        .unwrap();
    runner.cast(spells[3]).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    assert!(runner.state().replacement_auto_choices.is_empty());
}

#[test]
fn matching_sources_with_changed_definition_or_incarnation_or_set_prompt_again() {
    for change in 0..3 {
        let (mut runner, sources, spells) = life_scenario(false);
        runner.cast(spells[0]).resolve();
        runner
            .act(remember_source_order(
                runner.state(),
                &[sources[1], sources[0], sources[2]],
            ))
            .unwrap();
        runner.cast(spells[1]).resolve();
        assert_eq!(
            runner.state().players[0].life,
            34,
            "prove identical replay first"
        );
        match change {
            0 => {
                let object = runner.state_mut().objects.get_mut(&sources[0]).unwrap();
                object.replacement_definitions[0].quantity_modification =
                    Some(QuantityModification::Times { factor: 3 });
                Arc::make_mut(&mut object.base_replacement_definitions)[0].quantity_modification =
                    Some(QuantityModification::Times { factor: 3 });
                mark_layers_full(runner.state_mut());
            }
            1 => {
                move_fixture_object(runner.state_mut(), sources[0], Zone::Exile);
                move_fixture_object(runner.state_mut(), sources[0], Zone::Battlefield);
            }
            2 => {
                move_fixture_object(runner.state_mut(), sources[2], Zone::Exile);
            }
            _ => unreachable!(),
        }
        runner.cast(spells[2]).resolve();
        assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    }
}

#[test]
fn plain_optional_accept_and_decline_are_distinct_and_recur_after_restore() {
    for index in 0..2 {
        let (mut runner, sources, spells) = life_scenario(true);
        runner.cast(spells[0]).resolve();
        assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, true);
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            unreachable!();
        };
        let candidate = &candidates[index];
        let descriptions = vec![format!(
            "{} — {}",
            candidate.source_name, candidate.description
        )];
        runner.act(remember_optional(index)).unwrap();
        assert_eq!(
            runner.state().replacement_auto_choices[0].descriptions,
            descriptions
        );
        let gain = if index == 0 { 0 } else { 1 };
        assert_eq!(runner.state().players[0].life, 20 + gain);
        let encoded =
            serde_json::to_vec(&PersistedGameState::capture(runner.state().clone())).unwrap();
        let mut restored = serde_json::from_slice::<PersistedGameState>(&encoded)
            .unwrap()
            .into_game_state()
            .unwrap();
        // The restored state uses the same production cast and resolution actions.
        let card_id = restored.objects[&spells[1]].card_id;
        apply(
            &mut restored,
            P0,
            GameAction::CastSpell {
                object_id: spells[1],
                card_id,
                targets: vec![],
                payment_mode: Default::default(),
            },
        )
        .unwrap();
        for _ in 0..2 {
            engine::game::engine::apply_as_current(&mut restored, GameAction::PassPriority)
                .unwrap();
        }
        assert!(!matches!(
            restored.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(restored.players[0].life, 20 + 2 * gain);
        move_fixture_object(&mut restored, sources[0], Zone::Exile);
        assert_eq!(
            restored.replacement_auto_choices[0].descriptions,
            descriptions
        );
        let mut raw = serde_json::to_value(&restored).unwrap();
        raw.as_object_mut()
            .unwrap()
            .remove("replacement_auto_choices");
        raw.as_object_mut()
            .unwrap()
            .remove("replacement_auto_choice_tail");
        let legacy: GameState = serde_json::from_value(raw).unwrap();
        assert!(legacy.replacement_auto_choices.is_empty());
        assert!(legacy.replacement_auto_choice_tail.is_none());
    }
}

#[test]
fn ordering_an_optional_effect_cancels_tail_and_never_accepts_its_branch() {
    let (mut runner, sources, spells) = life_scenario(false);
    {
        let object = runner.state_mut().objects.get_mut(&sources[0]).unwrap();
        object.replacement_definitions[0].mode = ReplacementMode::Optional { decline: None };
        object.replacement_definitions[0].quantity_modification =
            Some(QuantityModification::Prevent);
        Arc::make_mut(&mut object.base_replacement_definitions)[0].mode =
            ReplacementMode::Optional { decline: None };
        Arc::make_mut(&mut object.base_replacement_definitions)[0].quantity_modification =
            Some(QuantityModification::Prevent);
    }
    mark_layers_full(runner.state_mut());
    runner.cast(spells[0]).resolve();
    runner.act(remember_order(&[0, 1, 2])).unwrap();
    assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, true);
    assert_eq!(runner.state().players[0].life, 20);
    assert!(runner.state().replacement_auto_choice_tail.is_none());
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert_eq!(runner.state().players[0].life, 25);
    runner.cast(spells[1]).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, true);
    assert_eq!(runner.state().players[0].life, 25);
}

fn assert_saved_optional_replays_after_order(order: [usize; 3]) {
    for index in 0..2 {
        let (mut runner, sources, spells) = life_scenario(false);
        let definition = ReplacementDefinition::new(ReplacementEvent::GainLife)
            .quantity_modification(QuantityModification::Prevent)
            .mode(ReplacementMode::Optional { decline: None });
        let object = runner.state_mut().objects.get_mut(&sources[0]).unwrap();
        object.replacement_definitions[0] = definition.clone();
        Arc::make_mut(&mut object.base_replacement_definitions)[0] = definition;
        mark_layers_full(runner.state_mut());

        runner.cast(spells[0]).resolve();
        assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
        let ordered_sources: Vec<_> = order.iter().map(|position| sources[*position]).collect();
        runner
            .act(remember_source_order(runner.state(), &ordered_sources))
            .unwrap();
        // CR 616.1: ordering alone cannot decide the optional branch.
        assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, true);
        assert_eq!(runner.state().players[0].life, 20);
        assert_eq!(runner.state().replacement_auto_choices.len(), 1);
        assert!(runner.state().replacement_auto_choice_tail.is_none());

        runner.act(remember_optional(index)).unwrap();
        let gain = if index == 0 { 0 } else { 5 };
        assert_eq!(runner.state().players[0].life, 20 + gain);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().replacement_auto_choices.len(), 2);
        assert_eq!(
            runner.state().replacement_auto_choices[0].key.kind,
            ReplacementChoiceKind::Order
        );
        assert_eq!(
            runner.state().replacement_auto_choices[1].key.kind,
            ReplacementChoiceKind::OptionalBranch
        );
        assert_eq!(
            runner.state().replacement_auto_choices[1].choice,
            ReplacementAutoChoice::Optional { index }
        );
        let saved = runner.state().replacement_auto_choices.clone();

        let outcome = runner.cast(spells[1]).resolve();
        outcome.assert_life_delta(P0, gain);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert_eq!(outcome.state().replacement_auto_choices, saved);
        assert!(outcome.state().replacement_auto_choice_tail.is_none());
    }
}

#[test]
fn saved_optional_decision_replays_when_order_selects_it_first() {
    assert_saved_optional_replays_after_order([0, 1, 2]);
}

#[test]
fn saved_optional_decision_replays_as_last_singleton_of_an_order() {
    assert_saved_optional_replays_after_order([1, 2, 0]);
}

#[test]
fn invalid_responses_and_wrong_actors_cannot_record_and_reset_is_owner_scoped() {
    let (mut runner, _, spells) = life_scenario(false);
    runner.cast(spells[0]).resolve();
    for action in [
        remember_order(&[0, 0, 2]),
        remember_order(&[0, 1]),
        remember_order(&[0, 1, 3]),
        remember_optional(0),
    ] {
        assert!(runner.act(action).is_err());
        assert!(runner.state().replacement_auto_choices.is_empty());
    }
    for actor in [P1, PlayerId(99)] {
        assert!(apply(runner.state_mut(), actor, remember_order(&[1, 0, 2])).is_err());
        assert!(runner.state().replacement_auto_choices.is_empty());
    }
    runner.act(remember_order(&[1, 0, 2])).unwrap();
    assert_eq!(
        filter_state_for_viewer(runner.state(), P0)
            .replacement_auto_choices
            .len(),
        1
    );
    assert!(filter_state_for_viewer(runner.state(), P1)
        .replacement_auto_choices
        .is_empty());
    assert!(filter_state_for_unseated_viewer(runner.state())
        .replacement_auto_choices
        .is_empty());
    let id = runner.state().replacement_auto_choices[0].id.clone();
    apply(
        runner.state_mut(),
        P1,
        GameAction::SetReplacementAutoChoice {
            selector: Some(id.clone()),
        },
    )
    .unwrap();
    assert_eq!(runner.state().replacement_auto_choices.len(), 1);
    runner
        .act(GameAction::SetReplacementAutoChoice { selector: Some(id) })
        .unwrap();
    assert!(runner.state().replacement_auto_choices.is_empty());
}

#[test]
fn split_actor_boundary_removes_and_clears_only_authenticated_players_preferences() {
    let (mut runner, _, spells) = life_scenario(false);
    runner.cast(spells[0]).resolve();
    runner.act(remember_order(&[1, 0, 2])).unwrap();
    let saved = runner.state().clone();
    let id = saved.replacement_auto_choices[0].id.clone();

    for selector in [Some(id), None] {
        let action = GameAction::SetReplacementAutoChoice { selector };
        let mut state = saved.clone();
        let result = apply_interaction(&mut state, P1, P0, action.clone()).unwrap();
        assert_eq!(
            state.replacement_auto_choices,
            saved.replacement_auto_choices
        );
        assert_eq!(state.waiting_for, saved.waiting_for);
        assert_eq!(state.players[0].life, saved.players[0].life);
        assert!(result.events.is_empty());

        let result = apply_interaction(&mut state, P0, P1, action).unwrap();
        assert!(state.replacement_auto_choices.is_empty());
        assert_eq!(state.waiting_for, saved.waiting_for);
        assert_eq!(state.players[0].life, saved.players[0].life);
        assert!(result.events.is_empty());
    }
}

fn finish_step(runner: &mut GameRunner) {
    let mut phase = runner.state().phase;
    for _ in 0..4 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).unwrap();
                phase = runner.state().phase;
                continue;
            }
            _ => panic!("unexpected step boundary {:?}", runner.state().waiting_for),
        }
        if runner.state().phase != phase
            || matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            )
        {
            return;
        }
    }
    panic!("step did not finish");
}

#[test]
fn step_end_preferences_follow_real_handler_sources_across_scan_reordering() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let convert = scenario
        .add_creature(P0, "Mana converter", 1, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::StepEndUnspentMana {
                filter: None,
                action: StepEndManaAction::Transform(ManaType::Red),
            })
            .affected(TargetFilter::Controller),
        )
        .id();
    let retain = scenario
        .add_creature(P0, "Mana keeper", 1, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::StepEndUnspentMana {
                filter: None,
                action: StepEndManaAction::Retain,
            })
            .affected(TargetFilter::Controller),
        )
        .id();
    let unrelated = scenario
        .add_creature(P0, "Another converter", 1, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::StepEndUnspentMana {
                filter: None,
                action: StepEndManaAction::Transform(ManaType::Green),
            })
            .affected(TargetFilter::Controller),
        )
        .id();
    let mut runner = scenario.build();
    move_fixture_object(runner.state_mut(), unrelated, Zone::Exile);
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Blue,
        convert,
        false,
        vec![],
    ));
    finish_step(&mut runner);
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("mana choice");
    };
    let first = candidates
        .iter()
        .position(|candidate| candidate.source_id == convert)
        .unwrap();
    runner.act(remember_order(&[first, 1 - first])).unwrap();
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Red),
        1
    );
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    // Change scan positions without changing either source's incarnation.
    let position = runner
        .state()
        .battlefield
        .iter()
        .position(|id| *id == retain)
        .unwrap();
    runner.state_mut().battlefield.remove(position);
    runner.state_mut().battlefield.push_front(retain);
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Blue,
        convert,
        false,
        vec![],
    ));
    finish_step(&mut runner);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Red),
        2
    );
    move_fixture_object(runner.state_mut(), unrelated, Zone::Battlefield);
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Blue,
        convert,
        false,
        vec![],
    ));
    finish_step(&mut runner);
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
}

#[test]
fn may_cost_entry_is_ineligible_and_ordinary_payment_still_pauses_and_pays_once() {
    // Oracle text verified against Scryfall's named-card API before this test was written.
    let oracle = "If this artifact would enter, you may discard a land card instead. If you do, put this artifact onto the battlefield. If you don't, put it into its owner's graveyard.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox = scenario
        .add_artifact_to_hand_from_oracle(P0, "Mox Diamond", oracle)
        .id();
    let land = scenario.add_land_to_hand(P0, "Forest").id();
    let other_land = scenario.add_land_to_hand(P0, "Island").id();
    let mut runner = scenario.build();
    runner.cast(mox).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, false);
    assert!(runner.act(remember_optional(0)).is_err());
    assert!(runner.state().replacement_auto_choices.is_empty());
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { .. }
    ));
    assert_ne!(runner.state().objects[&mox].zone, Zone::Battlefield);
    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .unwrap();
    assert_eq!(runner.state().objects[&mox].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&other_land].zone, Zone::Hand);
}

#[test]
fn hidden_origin_prompt_identity_is_visible_only_to_the_authorized_actor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_to_hand(P0, "Secret replacement land")
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .mode(ReplacementMode::Optional { decline: None }),
        )
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .unwrap();
    assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, true);
    let owner = serde_json::to_value(filter_state_for_viewer(runner.state(), P0)).unwrap();
    assert!(owner["waiting_for"]["data"]["remember_identity"].is_object());
    for projected in [
        filter_state_for_viewer(runner.state(), P1),
        filter_state_for_unseated_viewer(runner.state()),
    ] {
        let snapshot = serde_json::to_value(projected).unwrap();
        assert!(snapshot["waiting_for"]["data"]["remember_identity"].is_null());
        assert_eq!(
            snapshot["waiting_for"]["data"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "candidate_count",
                "candidates",
                "kind",
                "last_applied_decides",
                "player",
            ]
        );
        assert!(snapshot["pending_replacement"].is_null());
        assert!(snapshot["replacement_auto_choice_tail"].is_null());
    }
    runner.act(remember_optional(1)).unwrap();
    assert_eq!(runner.state().replacement_auto_choices.len(), 1);
    for projected in [
        filter_state_for_viewer(runner.state(), P1),
        filter_state_for_unseated_viewer(runner.state()),
    ] {
        let snapshot = serde_json::to_value(projected).unwrap();
        assert!(snapshot.get("replacement_auto_choices").is_none());
        assert!(snapshot["replacement_auto_choice_tail"].is_null());
    }
}

#[test]
fn exhausted_remembered_draw_order_allows_a_newly_applicable_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten",
            "Eleven", "Twelve", "Thirteen", "Fourteen",
        ],
    );
    let original_count = QuantityExpr::Ref {
        qty: QuantityRef::EventContextAmount,
    };
    let counts = [
        QuantityExpr::Offset {
            offset: 1,
            inner: Box::new(original_count.clone()),
        },
        QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(original_count.clone()),
        },
        QuantityExpr::Offset {
            offset: 3,
            inner: Box::new(original_count.clone()),
        },
    ];
    let mut sources = Vec::new();
    for (index, count) in counts.into_iter().enumerate() {
        let mut definition = ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::InstructionCount)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count,
                    target: TargetFilter::Controller,
                },
            ));
        if index == 2 {
            definition = definition.condition(ReplacementCondition::OnlyIfQuantity {
                lhs: original_count.clone(),
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 4 },
                active_player_req: None,
            });
        }
        sources.push(
            scenario
                .add_creature(P0, "Draw modifier", 1, 1)
                .with_replacement_definition(definition)
                .id(),
        );
    }
    let spells: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand(P0, "Draw spell", true)
                .with_ability(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                })
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.cast(spells[0]).resolve().assert_hand_drawn(P0, 0);
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    let WaitingFor::ReplacementChoice {
        candidates,
        candidate_count,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!();
    };
    assert_eq!(
        *candidate_count, 2,
        "only A and B apply to the original count"
    );
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .all(|candidate| sources[..2].contains(&candidate.source_id)));
    runner
        .act(remember_source_order(runner.state(), &sources[..2]))
        .unwrap();
    // CR 616.1f + CR 616.2: after A then B exhaust the order (1 → 2 → 4),
    // the newly applicable mandatory C runs without a confirmation (4 → 7).
    assert_eq!(
        runner.state().players[0].hand.len(),
        8,
        "seven drawn cards and the second spell"
    );
    assert_eq!(runner.state().players[0].library.len(), 7);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().replacement_auto_choice_tail.is_none());
    let saved = runner.state().replacement_auto_choices.clone();
    assert_eq!(saved.len(), 1);

    let outcome = runner.cast(spells[1]).resolve();
    outcome.assert_hand_drawn(P0, 7);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().players[0].library.is_empty());
    assert!(outcome.state().replacement_auto_choice_tail.is_none());
    assert_eq!(outcome.state().replacement_auto_choices, saved);
}

#[test]
fn changed_tail_with_one_remaining_candidate_can_remember_a_singleton_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four", "Five", "Six"]);
    let original_count = QuantityExpr::Ref {
        qty: QuantityRef::EventContextAmount,
    };
    let counts = [
        QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(original_count.clone()),
        },
        QuantityExpr::Offset {
            offset: 1,
            inner: Box::new(original_count.clone()),
        },
        QuantityExpr::Offset {
            offset: 3,
            inner: Box::new(original_count.clone()),
        },
    ];
    let mut sources = Vec::new();
    for (index, count) in counts.into_iter().enumerate() {
        let mut definition = ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::InstructionCount)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count,
                    target: TargetFilter::Controller,
                },
            ));
        if index == 2 {
            definition = definition.condition(ReplacementCondition::OnlyIfQuantity {
                lhs: original_count.clone(),
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 1 },
                active_player_req: None,
            });
        }
        sources.push(
            scenario
                .add_creature(P0, "Draw modifier", 1, 1)
                .with_replacement_definition(definition)
                .id(),
        );
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw spell", true, "Draw a card.")
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    runner
        .act(remember_source_order(
            runner.state(),
            &[sources[1], sources[0], sources[2]],
        ))
        .unwrap();
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    let WaitingFor::ReplacementChoice {
        candidate_count, ..
    } = runner.state().waiting_for
    else {
        unreachable!();
    };
    assert_eq!(
        candidate_count, 1,
        "the count-gated third candidate stopped applying"
    );
    assert!(runner.state().replacement_auto_choice_tail.is_none());
    assert!(runner.act(remember_optional(0)).is_err());
    runner.act(remember_order(&[0])).unwrap();
    assert!(runner.state().replacement_auto_choice_tail.is_none());
    assert_eq!(runner.state().players[0].hand.len(), 4, "(one + one) * two");
    assert_eq!(runner.state().replacement_auto_choices.len(), 2);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

#[test]
fn interactive_leading_damage_continuation_is_ineligible_and_keeps_ordinary_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let continuation = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Hand,
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
    )
    .optional();
    let interactive = scenario
        .add_creature(P0, "Damage replacement with selection", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_modification(DamageModification::Minus { value: 1 })
                .damage_target_filter(DamageTargetFilter::CreatureOnly)
                .execute(continuation),
        )
        .id();
    scenario
        .add_creature(P0, "Damage doubler", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_modification(DamageModification::Double)
                .damage_target_filter(DamageTargetFilter::CreatureOnly),
        );
    let target = scenario.add_creature(P0, "Damage target", 1, 10).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Damage spell",
            true,
            "Damage spell deals 3 damage to target creature.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(target).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, false);
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!();
    };
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == interactive)
        .unwrap();
    assert!(runner.act(remember_order(&[index, 1 - index])).is_err());
    assert!(runner.state().replacement_auto_choices.is_empty());
    runner.act(GameAction::ChooseReplacement { index }).unwrap();
    let WaitingFor::OptionalEffectChoice { .. } = &runner.state().waiting_for else {
        panic!(
            "full damage continuation must ask before returning the damaged creature, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&target].damage_marked, 4);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
}

#[test]
fn interactive_modifier_prefix_in_mixed_decline_is_ineligible_and_resolves_normally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decline = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Hand,
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
    )
    .optional()
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
    ));
    let source = scenario
        .add_creature(P0, "Mixed decline replacement", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::GainLife)
                .quantity_modification(QuantityModification::Prevent)
                .mode(ReplacementMode::Optional {
                    decline: Some(Box::new(decline)),
                }),
        )
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Life spell", true)
        .with_ability(Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, false);
    assert!(runner.act(remember_optional(1)).is_err());
    assert!(runner.state().replacement_auto_choices.is_empty());
    assert_prompt(runner.state(), ReplacementChoiceKind::OptionalBranch, false);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    // CR 608.2d: the nested optional effect must ask before moving its source.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice {
            player: P0,
            source_id,
            ..
        } if source_id == source
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[0].life, 21);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    // CR 608.2c + CR 119.3: return the source before the follow-up life gain.
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].life, 23);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().replacement_auto_choices.is_empty());
}

#[test]
fn opaque_removal_survives_prior_removals_restore_and_recreated_exact_keys() {
    let (mut runner, sources, spells) = life_scenario(false);
    let mut records = Vec::new();
    for (index, factor) in [2, 3, 4].into_iter().enumerate() {
        if index > 0 {
            let object = runner.state_mut().objects.get_mut(&sources[0]).unwrap();
            object.replacement_definitions[0].quantity_modification =
                Some(QuantityModification::Times { factor });
            Arc::make_mut(&mut object.base_replacement_definitions)[0].quantity_modification =
                Some(QuantityModification::Times { factor });
            mark_layers_full(runner.state_mut());
        }
        runner.cast(spells[index]).resolve();
        assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
        runner.act(remember_order(&[1, 0, 2])).unwrap();
        let record = runner
            .state()
            .replacement_auto_choices
            .last()
            .unwrap()
            .clone();
        assert_eq!(record.id.0.len(), 65);
        assert!(record.id.0.starts_with('r'));
        assert!(record.id.0[1..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(serde_json::to_value(&record.id).unwrap(), record.id.0);
        assert!(records
            .iter()
            .all(|prior: &ReplacementAutoChoiceRecord| prior.id != record.id));
        records.push(record);
    }
    assert_eq!(runner.state().replacement_auto_choices, records);
    runner
        .act(GameAction::SetReplacementAutoChoice {
            selector: Some(records[0].id.clone()),
        })
        .unwrap();
    assert_eq!(runner.state().replacement_auto_choices, records[1..]);

    let encoded = serde_json::to_vec(&PersistedGameState::capture(runner.state().clone())).unwrap();
    let restored = serde_json::from_slice::<PersistedGameState>(&encoded)
        .unwrap()
        .into_game_state()
        .unwrap();
    assert_eq!(restored.replacement_auto_choices, records[1..]);
    runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::SetReplacementAutoChoice {
            selector: Some(records[1].id.clone()),
        })
        .unwrap();
    assert_eq!(runner.state().replacement_auto_choices, records[2..]);

    runner
        .act(GameAction::SetReplacementAutoChoice {
            selector: Some(ReplacementAutoChoiceId("unknown".into())),
        })
        .unwrap();
    for selector in [Some(records[2].id.clone()), None] {
        apply(
            runner.state_mut(),
            P1,
            GameAction::SetReplacementAutoChoice { selector },
        )
        .unwrap();
        assert_eq!(runner.state().replacement_auto_choices, records[2..]);
    }
    // A new key saved after restore and an identical key re-created after
    // deletion can both be removed without affecting the surviving record.
    for (spell, factor) in [(spells[3], 5), (spells[4], 2)] {
        let object = runner.state_mut().objects.get_mut(&sources[0]).unwrap();
        object.replacement_definitions[0].quantity_modification =
            Some(QuantityModification::Times { factor });
        Arc::make_mut(&mut object.base_replacement_definitions)[0].quantity_modification =
            Some(QuantityModification::Times { factor });
        mark_layers_full(runner.state_mut());
        runner.cast(spell).resolve();
        assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
        runner.act(remember_order(&[0, 1, 2])).unwrap();
        let recreated = runner.state().replacement_auto_choices.last().unwrap();
        if factor == 2 {
            assert_eq!(recreated.key, records[0].key);
            assert_eq!(recreated.id, records[0].id);
            assert_ne!(recreated.choice, records[0].choice);
        } else {
            assert!(records.iter().all(|record| record.id != recreated.id));
        }
        let id = recreated.id.clone();
        runner
            .act(GameAction::SetReplacementAutoChoice { selector: Some(id) })
            .unwrap();
        assert_eq!(runner.state().replacement_auto_choices, records[2..]);
    }
    runner
        .act(GameAction::SetReplacementAutoChoice { selector: None })
        .unwrap();
    assert!(runner.state().replacement_auto_choices.is_empty());
}

#[test]
fn opaque_id_depends_on_the_exact_key_and_not_the_saved_choice() {
    let (mut runner, _, spells) = life_scenario(false);
    runner.cast(spells[0]).resolve();
    assert_prompt(runner.state(), ReplacementChoiceKind::Order, true);
    let prompt = runner.state().clone();
    runner.act(remember_order(&[1, 0, 2])).unwrap();
    let original = runner.state().replacement_auto_choices[0].clone();
    let mut same_key = GameRunner::from_state(prompt.clone());
    same_key.act(remember_order(&[0, 1, 2])).unwrap();
    let changed_choice = &same_key.state().replacement_auto_choices[0];
    assert_eq!(changed_choice.key, original.key);
    assert_eq!(changed_choice.id, original.id);
    assert_ne!(changed_choice.choice, original.choice);

    // The key's candidate order remains part of its removal identity even
    // though replay matches the same set of exact source definitions.
    let mut reordered = prompt;
    reordered
        .pending_replacement
        .as_mut()
        .unwrap()
        .candidates
        .reverse();
    let mut reordered = GameRunner::from_state(reordered);
    reordered.act(remember_order(&[1, 2, 0])).unwrap();
    let changed_key = &reordered.state().replacement_auto_choices[0];
    let mut expected_key = original.key.clone();
    expected_key.candidates.reverse();
    assert_eq!(changed_key.key, expected_key);
    assert_ne!(changed_key.id, original.id);

    let mut missing_id = serde_json::to_value(&original).unwrap();
    assert!(missing_id.as_object_mut().unwrap().remove("id").is_some());
    assert!(serde_json::from_value::<ReplacementAutoChoiceRecord>(missing_id).is_err());
}
