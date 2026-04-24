//! Integration tests for landfall trigger interactions.
//!
//! Validates that multiple landfall triggers fire correctly, including:
//! - DoublePT effects from Mightform Harmonizer
//! - Conditional reflexive triggers from Earthbender Ascension (quest counters → +1/+1 + trample)
//! - Graveyard-active landfall (Bloodghast): CR 113.6b / CR 603.6 — trigger declares
//!   `trigger_zones = [Graveyard]` because its self-referential effect returns the
//!   source from the graveyard to the battlefield, so the ability functions while
//!   the card is in its owner's graveyard.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Helper: resolve all triggers and target selections until we reach Priority
/// with an empty stack. Returns the list of object IDs selected as targets.
fn resolve_all_triggers(runner: &mut engine::game::scenario::GameRunner) -> Vec<ObjectId> {
    let mut selected_targets = Vec::new();
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                // Auto-select first legal target
                if let Some(first_target) = target_slots
                    .first()
                    .and_then(|slot| slot.legal_targets.first())
                {
                    let target = first_target.clone();
                    if let engine::types::ability::TargetRef::Object(id) = &target {
                        selected_targets.push(*id);
                    }
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(target),
                        })
                        .expect("target selection should succeed");
                } else {
                    break;
                }
            }
            _ => {
                // Pass priority to advance through other states (including resolving stack)
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
    selected_targets
}

// ---------------------------------------------------------------------------
// Mightform Harmonizer: landfall DoublePT
// ---------------------------------------------------------------------------

#[test]
fn mightform_harmonizer_landfall_doubles_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Mightform Harmonizer: 4/4, landfall doubles power of target creature
    let harmonizer_id = scenario
        .add_creature_from_oracle(
            P0,
            "Mightform Harmonizer",
            4,
            4,
            "Landfall — Whenever a land you control enters, double the power of target creature you control until end of turn.",
        )
        .id();

    // Add a vanilla creature as an alternative target
    let bear_id = scenario.add_vanilla(P0, 2, 2);

    // Forest in hand to trigger landfall
    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    // Opponent gets a creature too (to verify "you control" filter)
    scenario.add_vanilla(P1, 3, 3);

    let mut runner = scenario.build();

    // Play the Forest from hand
    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("should play Forest");

    // Should get a TriggerTargetSelection for the DoublePT landfall
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            // Legal targets should be creatures P0 controls (Harmonizer + Bear)
            let legal = &target_slots[0].legal_targets;
            assert!(
                legal.iter().any(|t| matches!(t, engine::types::ability::TargetRef::Object(id) if *id == harmonizer_id)),
                "Harmonizer should be a legal target"
            );
            assert!(
                legal.iter().any(
                    |t| matches!(t, engine::types::ability::TargetRef::Object(id) if *id == bear_id)
                ),
                "Bear should be a legal target"
            );
        }
        other => panic!("Expected TriggerTargetSelection, got: {other:?}"),
    }

    // Target the Harmonizer itself to double its power (4 → 8)
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(harmonizer_id)),
        })
        .expect("choose Harmonizer as target");

    // Resolve the trigger on the stack
    resolve_all_triggers(&mut runner);

    // Evaluate layers to apply continuous effects
    engine::game::layers::evaluate_layers(runner.state_mut());

    // Harmonizer should now have 8 power (doubled from 4)
    let harmonizer = &runner.state().objects[&harmonizer_id];
    assert_eq!(
        harmonizer.power,
        Some(8),
        "Harmonizer power should be doubled from 4 to 8"
    );
    // Toughness should be unchanged
    assert_eq!(
        harmonizer.toughness,
        Some(4),
        "Harmonizer toughness should be unchanged"
    );
}

// ---------------------------------------------------------------------------
// Earthbender Ascension: quest counter threshold → +1/+1 + trample
// ---------------------------------------------------------------------------

#[test]
fn earthbender_ascension_grants_trample_at_four_quest_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Earthbender Ascension on battlefield with 3 quest counters (one more triggers the bonus)
    let ascension_id = scenario
        .add_creature(P0, "Earthbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(
            "Landfall — Whenever a land you control enters, put a quest counter on this enchantment. When you do, if it has four or more quest counters on it, put a +1/+1 counter on target creature you control. It gains trample until end of turn.",
        )
        .id();

    // Target creature for the +1/+1 counter
    let creature_id = scenario.add_vanilla(P0, 3, 3);

    // Forest in hand to trigger landfall
    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    // Add 3 quest counters via the runner's state_mut()
    add_quest_counters(&mut runner, ascension_id, 3);

    // Verify starting state: 3 quest counters
    assert_eq!(
        runner.state().objects[&ascension_id]
            .counters
            .get(&CounterType::Generic("quest".to_string()))
            .copied()
            .unwrap_or(0),
        3,
        "Ascension should start with 3 quest counters"
    );

    // Play the Forest → triggers Ascension's landfall
    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("should play Forest");

    // Resolve all triggers (quest counter → condition met → target selection for +1/+1)
    resolve_all_triggers(&mut runner);

    // Evaluate layers
    engine::game::layers::evaluate_layers(runner.state_mut());

    // Ascension should now have 4 quest counters
    let ascension = &runner.state().objects[&ascension_id];
    assert_eq!(
        ascension
            .counters
            .get(&CounterType::Generic("quest".to_string()))
            .copied()
            .unwrap_or(0),
        4,
        "Ascension should have 4 quest counters after landfall"
    );

    // Creature should have a +1/+1 counter
    let creature = &runner.state().objects[&creature_id];
    assert_eq!(
        creature
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "Creature should have received a +1/+1 counter"
    );

    // Creature should have trample until end of turn
    assert!(
        creature.keywords.contains(&Keyword::Trample),
        "Creature should have trample from Ascension's trigger"
    );

    // Creature should be 4/4 (3/3 base + 1/1 counter)
    assert_eq!(
        creature.power,
        Some(4),
        "Creature should be 4/4 with the +1/+1 counter"
    );
    assert_eq!(
        creature.toughness,
        Some(4),
        "Creature should be 4/4 with the +1/+1 counter"
    );
}

// ---------------------------------------------------------------------------
// Combined: Harmonizer + Ascension with multiple landfall triggers
// ---------------------------------------------------------------------------

#[test]
fn harmonizer_and_ascension_combined_landfall() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Mightform Harmonizer on battlefield
    let harmonizer_id = scenario
        .add_creature_from_oracle(
            P0,
            "Mightform Harmonizer",
            4,
            4,
            "Landfall — Whenever a land you control enters, double the power of target creature you control until end of turn.",
        )
        .id();

    // Earthbender Ascension with 3 quest counters
    let ascension_id = scenario
        .add_creature(P0, "Earthbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(
            "Landfall — Whenever a land you control enters, put a quest counter on this enchantment. When you do, if it has four or more quest counters on it, put a +1/+1 counter on target creature you control. It gains trample until end of turn.",
        )
        .id();

    // Forest in hand
    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    // Add 3 quest counters via the runner's state_mut()
    add_quest_counters(&mut runner, ascension_id, 3);

    // Play Forest → both landfall triggers fire
    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("should play Forest");

    // Resolve all triggers, targeting Harmonizer for everything
    resolve_all_triggers(&mut runner);

    // Evaluate layers
    engine::game::layers::evaluate_layers(runner.state_mut());

    // Ascension should have 4 quest counters
    let ascension = &runner.state().objects[&ascension_id];
    assert_eq!(
        ascension
            .counters
            .get(&CounterType::Generic("quest".to_string()))
            .copied()
            .unwrap_or(0),
        4,
    );

    // Harmonizer should have doubled power AND a +1/+1 counter AND trample
    let harmonizer = &runner.state().objects[&harmonizer_id];

    // Base 4 + 1 counter = 5, doubled = 10 (or base 4 doubled = 8 + 1 counter = 9,
    // depending on trigger resolution order)
    // Either way, power should be > 4
    assert!(
        harmonizer.power.unwrap_or(0) > 4,
        "Harmonizer power should be greater than base 4 after doubling + counter, got {:?}",
        harmonizer.power
    );

    assert!(
        harmonizer
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
            >= 1,
        "Harmonizer should have at least one +1/+1 counter from Ascension"
    );

    assert!(
        harmonizer.keywords.contains(&Keyword::Trample),
        "Harmonizer should have trample from Ascension's trigger"
    );
}

// ---------------------------------------------------------------------------
// Ascension below threshold: no +1/+1 or trample when < 4 quest counters
// ---------------------------------------------------------------------------

#[test]
fn earthbender_ascension_no_bonus_below_threshold() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Earthbender Ascension with only 2 quest counters (3 after landfall, still < 4)
    let ascension_id = scenario
        .add_creature(P0, "Earthbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(
            "Landfall — Whenever a land you control enters, put a quest counter on this enchantment. When you do, if it has four or more quest counters on it, put a +1/+1 counter on target creature you control. It gains trample until end of turn.",
        )
        .id();

    let creature_id = scenario.add_vanilla(P0, 3, 3);
    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    // Add 2 quest counters (will be 3 after landfall, still < 4)
    add_quest_counters(&mut runner, ascension_id, 2);

    // Play Forest → landfall fires, counter goes 2 → 3, condition (≥4) NOT met
    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("should play Forest");

    resolve_all_triggers(&mut runner);
    engine::game::layers::evaluate_layers(runner.state_mut());

    // Ascension should have 3 quest counters
    let ascension = &runner.state().objects[&ascension_id];
    assert_eq!(
        ascension
            .counters
            .get(&CounterType::Generic("quest".to_string()))
            .copied()
            .unwrap_or(0),
        3,
    );

    // Creature should NOT have a +1/+1 counter (condition not met)
    let creature = &runner.state().objects[&creature_id];
    assert_eq!(
        creature
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "Creature should NOT have a +1/+1 counter (condition not met)"
    );

    // Creature should NOT have trample
    assert!(
        !creature.keywords.contains(&Keyword::Trample),
        "Creature should NOT have trample (condition not met)"
    );
}

// ---------------------------------------------------------------------------
// Bloodghast: graveyard-active landfall ("may return this card from your
// graveyard to the battlefield"). CR 113.6b + CR 603.6: the trigger's declared
// `trigger_zones = [Graveyard]` makes the ability function while the source is
// in its owner's graveyard. The "a land you control enters" event filter keeps
// opponent land-drops from firing the trigger.
// ---------------------------------------------------------------------------

const BLOODGHAST_ORACLE: &str = "This creature can't block.\nThis creature has haste as long as an opponent has 10 or less life.\nLandfall — Whenever a land you control enters, you may return this card from your graveyard to the battlefield.";

/// Move a battlefield object to its owner's graveyard so we can exercise the
/// graveyard-active landfall path without having to cast and kill Bloodghast.
fn relocate_to_graveyard(
    runner: &mut engine::game::scenario::GameRunner,
    id: ObjectId,
    owner: engine::types::PlayerId,
) {
    let state = runner.state_mut();
    state.battlefield.retain(|o| *o != id);
    state
        .players
        .iter_mut()
        .find(|p| p.id == owner)
        .expect("owner exists")
        .graveyard
        .push_back(id);
    state.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
}

/// Resolve optional-effect prompts by accepting, and pass priority otherwise,
/// until the stack empties.
fn resolve_accepting_optional(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

#[test]
fn bloodghast_parser_trigger_is_graveyard_active() {
    // Verify the parser itself wires the Landfall trigger with trigger_zones =
    // [Graveyard]. This guards the self-recursion zone-derivation heuristic in
    // oracle_trigger.rs from silent regressions.
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{Effect, TargetFilter};

    let parsed = parse_oracle_text(BLOODGHAST_ORACLE, "Bloodghast", &[], &[], &[]);
    assert!(
        !parsed.triggers.is_empty(),
        "Bloodghast should parse a triggered ability"
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, engine::types::triggers::TriggerMode::ChangesZone))
        .expect("Landfall is a ChangesZone (land enters) trigger");

    assert_eq!(
        trigger.trigger_zones,
        vec![Zone::Graveyard],
        "Landfall with 'return from graveyard' effect must activate from graveyard"
    );
    assert!(trigger.optional, "Bloodghast's trigger is optional (may)");

    let exec = trigger.execute.as_deref().expect("trigger has effect");
    match exec.effect.as_ref() {
        Effect::ChangeZone {
            origin,
            destination,
            target,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Graveyard));
            assert_eq!(*destination, Zone::Battlefield);
            assert!(matches!(target, TargetFilter::SelfRef));
        }
        other => panic!("expected ChangeZone effect, got {other:?}"),
    }
}

#[test]
fn bloodghast_returns_when_controller_plays_a_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bloodghast_id = scenario
        .add_creature_from_oracle(P0, "Bloodghast", 2, 1, BLOODGHAST_ORACLE)
        .id();
    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();
    relocate_to_graveyard(&mut runner, bloodghast_id, P0);

    assert_eq!(
        runner.state().objects[&bloodghast_id].zone,
        Zone::Graveyard,
        "precondition: Bloodghast is in P0's graveyard"
    );

    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("P0 plays Forest");

    resolve_accepting_optional(&mut runner);

    assert_eq!(
        runner.state().objects[&bloodghast_id].zone,
        Zone::Battlefield,
        "Bloodghast should return to the battlefield after accepting its landfall trigger"
    );
}

#[test]
fn bloodghast_does_not_trigger_on_opponent_land() {
    // "a land you control enters" — P1 playing a land must not offer P0 the
    // graveyard-return option.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bloodghast_id = scenario
        .add_creature_from_oracle(P0, "Bloodghast", 2, 1, BLOODGHAST_ORACLE)
        .id();
    let mountain_id = scenario.add_land_to_hand(P1, "Mountain").id();

    let mut runner = scenario.build();
    relocate_to_graveyard(&mut runner, bloodghast_id, P0);
    // Hand priority/activity to P1 so they can play their land drop.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let card_id = runner.state().objects[&mountain_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: mountain_id,
            card_id,
        })
        .expect("P1 plays Mountain");

    // There should be no optional-effect prompt for P0. Drive whatever is on
    // the stack to completion and assert Bloodghast is still in the graveyard.
    resolve_accepting_optional(&mut runner);

    assert_eq!(
        runner.state().objects[&bloodghast_id].zone,
        Zone::Graveyard,
        "Bloodghast must NOT return from opponent's land drop"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn add_quest_counters(runner: &mut engine::game::scenario::GameRunner, id: ObjectId, count: u32) {
    let counter = CounterType::Generic("quest".to_string());
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    *obj.counters.entry(counter).or_insert(0) += count;
}
