//! Regression coverage for issue #7221: a completed non-cost Forage action
//! must publish the generic player-action event that drives "Whenever you
//! forage" triggers. Declining or failing to perform the action must not.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectScope, ReplacementDefinition, TapStateChange,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const CORPSEBERRY_ORACLE: &str = "At the beginning of combat on your turn, you may forage. (Exile three cards from your graveyard or sacrifice a Food.)\nWhenever you forage, put a +1/+1 counter on this creature.";

fn destination_redirect_replacement(
    from: Zone,
    to: Zone,
    description: &str,
) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(from)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: to,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description(description.to_string())
}

fn zone_tap_state_replacement(
    destination: Zone,
    state: TapStateChange,
    description: &str,
) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(destination)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state,
            },
        ))
        .description(description.to_string())
}

fn corpseberry_board(
    with_food: bool,
    graveyard_cards: usize,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let corpseberry = scenario
        .add_creature_from_oracle(P0, "Corpseberry Cultivator", 2, 3, CORPSEBERRY_ORACLE)
        .id();
    let food = with_food.then(|| {
        scenario
            .add_creature(P0, "Food", 0, 0)
            .as_artifact()
            .with_subtypes(vec!["Food"])
            .id()
    });
    let names: Vec<String> = (0..graveyard_cards)
        .map(|index| format!("Graveyard Card {index}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    scenario.with_graveyard(P0, &refs);
    (scenario.build(), corpseberry, food)
}

fn plus_one_counters(runner: &GameRunner, object_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object_id)
        .and_then(|object| object.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0)
}

fn reach_optional_forage(runner: &mut GameRunner) -> Vec<GameEvent> {
    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::BeginCombat);

    let mut events = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0);
                return events;
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("advance Corpseberry begin-combat trigger")
                        .events,
                );
            }
            other => panic!("expected Corpseberry optional forage, got {other:?}"),
        }
    }
    panic!("Corpseberry optional forage did not surface");
}

fn drain_stack(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("drain Corpseberry trigger stack")
                        .events,
                );
            }
            other => panic!("unexpected prompt while draining Corpseberry stack: {other:?}"),
        }
    }
    panic!("Corpseberry trigger stack did not settle");
}

fn forage_action_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: PlayerActionKind::Forage,
                    ..
                }
            )
        })
        .count()
}

fn assert_ordered_forage_completion(events: &[GameEvent], source_id: ObjectId) {
    assert!(events.windows(2).any(|pair| {
        matches!(
            pair,
            [
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Forage,
                    source_id: event_source,
                    ..
                },
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: PlayerActionKind::Forage,
                    ..
                }
            ] if *event_source == source_id
        )
    }));
}

/// CR 701.61a + CR 603.2: sacrificing the only Food completes Forage once,
/// publishes the action after completion, and fires Corpseberry's trigger once.
#[test]
fn corpseberry_food_forage_fires_trigger_once() {
    let (mut runner, corpseberry, food) = corpseberry_board(true, 0);
    let food = food.expect("Food fixture");
    let mut events = reach_optional_forage(&mut runner);

    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept optional forage")
            .events,
    );
    drain_stack(&mut runner, &mut events);

    assert_eq!(runner.state().objects[&food].zone, Zone::Graveyard);
    assert_eq!(forage_action_count(&events), 1);
    assert_ordered_forage_completion(&events, corpseberry);
    assert_eq!(plus_one_counters(&runner, corpseberry), 1);
    assert_eq!(
        runner
            .state()
            .player_actions_this_turn
            .iter()
            .filter(|(player, action)| *player == P0 && *action == PlayerActionKind::Forage)
            .count(),
        1,
        "the outer Forage frame must not duplicate the nested completion ledger"
    );
}

/// CR 701.61a + CR 603.2: when several Foods are available, selecting one is
/// part of the forage instruction. Completion is published only after the
/// selected Food is sacrificed through the real EffectZoneChoice resume path.
#[test]
fn corpseberry_food_choice_fires_after_selected_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let corpseberry = scenario
        .add_creature_from_oracle(P0, "Corpseberry Cultivator", 2, 3, CORPSEBERRY_ORACLE)
        .id();
    let foods = ["Food A", "Food B"].map(|name| {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_artifact()
            .with_subtypes(vec!["Food"])
            .id()
    });
    let mut runner = scenario.build();
    let mut events = reach_optional_forage(&mut runner);

    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept Food forage")
            .events,
    );
    let selected = match runner.state().waiting_for.clone() {
        WaitingFor::EffectZoneChoice {
            cards,
            count: 1,
            effect_kind: engine::types::ability::EffectKind::Sacrifice,
            ..
        } => cards[0],
        other => panic!("expected Food sacrifice choice, got {other:?}"),
    };
    assert_eq!(forage_action_count(&events), 0);
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![selected],
            })
            .expect("select Food to sacrifice")
            .events,
    );
    drain_stack(&mut runner, &mut events);

    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    assert_eq!(
        foods
            .iter()
            .filter(|food| runner.state().objects[food].zone == Zone::Battlefield)
            .count(),
        1
    );
    assert_eq!(forage_action_count(&events), 1);
    assert_ordered_forage_completion(&events, corpseberry);
    assert_eq!(plus_one_counters(&runner, corpseberry), 1);
}

/// CR 608.2d: declining the optional instruction performs no Forage action and
/// therefore cannot trigger Corpseberry's second ability.
#[test]
fn corpseberry_decline_does_not_forage() {
    let (mut runner, corpseberry, food) = corpseberry_board(true, 0);
    let food = food.expect("Food fixture");
    let mut events = reach_optional_forage(&mut runner);

    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("decline optional forage")
            .events,
    );
    drain_stack(&mut runner, &mut events);

    assert_eq!(runner.state().objects[&food].zone, Zone::Battlefield);
    assert_eq!(forage_action_count(&events), 0);
    assert_eq!(plus_one_counters(&runner, corpseberry), 0);
}

/// CR 608.2d: when neither complete Forage mode is possible, the optional
/// instruction is infeasible and must not open an acceptance prompt.
#[test]
fn corpseberry_impossible_optional_is_not_offered() {
    let (mut runner, corpseberry, _) = corpseberry_board(false, 2);
    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    let mut events = Vec::new();

    drain_stack(&mut runner, &mut events);

    assert_eq!(forage_action_count(&events), 0);
    assert_eq!(plus_one_counters(&runner, corpseberry), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 701.61a + CR 608.2c: the exile mode does not publish completion at its
/// selection prompt. It completes only after exactly three chosen graveyard
/// cards arrive in exile, then fires Corpseberry once.
#[test]
fn foraging_from_graveyard_triggers_corpseberry_cultivator() {
    let (mut runner, corpseberry, _) = corpseberry_board(false, 4);
    let mut events = reach_optional_forage(&mut runner);
    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept optional forage")
            .events,
    );

    let cards = match runner.state().waiting_for.clone() {
        WaitingFor::EffectZoneChoice {
            cards,
            count: 3,
            zone: Zone::Graveyard,
            destination: Some(Zone::Exile),
            ..
        } => cards.into_iter().take(3).collect::<Vec<_>>(),
        other => panic!("expected exile-three selection, got {other:?}"),
    };
    assert_eq!(forage_action_count(&events), 0);
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: cards.clone(),
            })
            .expect("select three graveyard cards for forage")
            .events,
    );
    drain_stack(&mut runner, &mut events);

    assert!(cards
        .iter()
        .all(|card| runner.state().objects[card].zone == Zone::Exile));
    assert_eq!(runner.state().players[P0.0 as usize].graveyard.len(), 1);
    assert_eq!(forage_action_count(&events), 1);
    assert_ordered_forage_completion(&events, corpseberry);
    assert_eq!(plus_one_counters(&runner, corpseberry), 1);
}

/// CR 701.61a + CR 616.1: replacement ordering can pause each member of the
/// exile-three operation. The exact moved-count and completion continuation
/// must survive serialization and publish Forage only after all three cards
/// have actually arrived in exile.
#[test]
fn corpseberry_exile_replacement_pause_completes_after_restore() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let corpseberry = scenario
        .add_creature_from_oracle(P0, "Corpseberry Cultivator", 2, 3, CORPSEBERRY_ORACLE)
        .id();
    scenario.with_graveyard(P0, &["A", "B", "C"]);
    for (name, tap_state) in [
        ("Synthetic Exile Tap", TapStateChange::Tap),
        ("Synthetic Exile Untap", TapStateChange::Untap),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(zone_tap_state_replacement(
                Zone::Exile,
                tap_state,
                "If a card would be exiled, modify that event.",
            ));
    }
    let mut runner = scenario.build();
    let mut events = reach_optional_forage(&mut runner);
    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept exile-mode forage")
            .events,
    );
    let chosen = match runner.state().waiting_for.clone() {
        WaitingFor::EffectZoneChoice { cards, .. } => cards,
        other => panic!("expected exile selection, got {other:?}"),
    };
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: chosen.clone(),
            })
            .expect("select cards for replacement-paused forage")
            .events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(forage_action_count(&events), 0);

    let serialized = serde_json::to_string(runner.state())
        .expect("replacement-paused exile Forage state serializes");
    *runner.state_mut() = serde_json::from_str(&serialized)
        .expect("replacement-paused exile Forage state deserializes");

    for _ in 0..chosen.len() {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("order exile replacement")
                .events,
        );
    }
    drain_stack(&mut runner, &mut events);

    assert!(chosen
        .iter()
        .all(|card| runner.state().objects[card].zone == Zone::Exile));
    assert_eq!(forage_action_count(&events), 1);
    assert_ordered_forage_completion(&events, corpseberry);
    assert_eq!(plus_one_counters(&runner, corpseberry), 1);
}

/// CR 701.21a + CR 614.6: a Food remains sacrificed when a replacement sends
/// it to exile instead of its owner's graveyard. The paused operation result
/// and its completion continuation also survive a serialized state round trip.
#[test]
fn corpseberry_food_redirected_to_exile_still_forges_after_restore() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let corpseberry = scenario
        .add_creature_from_oracle(P0, "Corpseberry Cultivator", 2, 3, CORPSEBERRY_ORACLE)
        .id();
    let food = scenario
        .add_creature(P0, "Food", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Food"])
        .id();
    for name in ["Rest in Peace", "Leyline of the Void"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(destination_redirect_replacement(
                Zone::Graveyard,
                Zone::Exile,
                "If a card would be put into a graveyard, exile it instead.",
            ));
    }
    let mut runner = scenario.build();
    let mut events = reach_optional_forage(&mut runner);
    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept Food forage under redirects")
            .events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(forage_action_count(&events), 0);

    let serialized =
        serde_json::to_string(runner.state()).expect("replacement-paused Forage state serializes");
    *runner.state_mut() =
        serde_json::from_str(&serialized).expect("replacement-paused Forage state deserializes");
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("choose graveyard redirect")
            .events,
    );
    drain_stack(&mut runner, &mut events);

    assert_eq!(runner.state().objects[&food].zone, Zone::Exile);
    assert_eq!(forage_action_count(&events), 1);
    assert_ordered_forage_completion(&events, corpseberry);
    assert_eq!(plus_one_counters(&runner, corpseberry), 1);
}

/// CR 614.6: the exile mode completes only for cards that actually arrive in
/// exile. Redirecting every selected card to hand resolves the instruction but
/// does not perform the Forage action or fire Corpseberry.
#[test]
fn corpseberry_redirected_exile_does_not_forage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let corpseberry = scenario
        .add_creature_from_oracle(P0, "Corpseberry Cultivator", 2, 3, CORPSEBERRY_ORACLE)
        .id();
    scenario.with_graveyard(P0, &["A", "B", "C"]);
    scenario
        .add_creature(P0, "Synthetic Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(destination_redirect_replacement(
            Zone::Exile,
            Zone::Hand,
            "If a card would be exiled, put it into its owner's hand instead.",
        ));
    let mut runner = scenario.build();
    let mut events = reach_optional_forage(&mut runner);
    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept exile-mode forage")
            .events,
    );
    let chosen = match runner.state().waiting_for.clone() {
        WaitingFor::EffectZoneChoice { cards, .. } => cards,
        other => panic!("expected exile selection, got {other:?}"),
    };
    events.extend(
        runner
            .act(GameAction::SelectCards { cards: chosen })
            .expect("select redirected exile cards")
            .events,
    );
    drain_stack(&mut runner, &mut events);

    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), 3);
    assert_eq!(forage_action_count(&events), 0);
    assert_eq!(plus_one_counters(&runner, corpseberry), 0);
}
