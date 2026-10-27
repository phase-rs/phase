//! Floating entry-counter riders retain their installing source (issue #9991).
//!
//! CR 109.5: "you" refers to the triggered ability's controller.
//! CR 113.7a: a triggered ability exists independently of its source.
//! CR 608.2h: source information must come from the specified object.
//! CR 614.12: replacements modify how a permanent enters the battlefield.
//! CR 616.1: the affected object's controller chooses among replacements.
//! Floating quantities are fixed when the installing trigger resolves; entering
//! object quantities retain their entry-time context.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreFinalization, ReplacementChoiceKind, WaitingFor,
};
use engine::types::identifiers::ObjectIncarnationRef;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::{ObjectId, PlayerId};

use crate::support::shared_card_db;

const BREWING: &str = "When this enchantment enters, any number of target opponents each draw a card. Put an ingredient counter on this enchantment, then put an ingredient counter on it for each card drawn this way.\nWhenever you cast a creature spell, that creature enters with X additional +1/+1 counters on it, where X is the number of ingredient counters on this enchantment.";
const RUNADI: &str = "Whenever you cast a creature spell with mana value 5 or greater, that creature enters with X additional +1/+1 counters on it, where X is its mana value minus 4.\nCreatures you control with three or more +1/+1 counters on them have haste.\n{T}: Add {G}.";
const ARCHAIC: &str = "Reach, trample\nConverge — This creature enters with a +1/+1 counter on it for each color of mana spent to cast it.\nWhenever you cast a creature spell, that creature enters with X additional +1/+1 counters on it, where X is the number of colors of mana spent to cast it.";

fn add_brewing(scenario: &mut GameScenario, player: PlayerId, count: u32) -> ObjectId {
    let brewing = scenario
        .add_enchantment_from_oracle(player, "Communal Brewing", BREWING)
        .id();
    scenario.with_counter(
        brewing,
        CounterType::Generic("ingredient".to_string()),
        count,
    );
    brewing
}

fn add_spell(scenario: &mut GameScenario, player: PlayerId, generic: u32) -> ObjectId {
    let spell = scenario
        .add_creature_to_hand_from_oracle(player, "Test Entrant", 1, 1, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic,
        })
        .id();
    scenario.with_mana_pool(
        player,
        [ManaType::Green, ManaType::Blue]
            .into_iter()
            .chain((0..generic).map(|_| ManaType::Colorless))
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    spell
}

fn assert_cast_trigger(runner: &GameRunner, source: ObjectId) {
    assert!(
        runner.state().objects[&source]
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|entry| entry.definition.mode == TriggerMode::SpellCast),
        "source {source:?} must register its SpellCast trigger before casting"
    );
}

fn make_p1_active(runner: &mut GameRunner) {
    // GameScenario exposes no public state_mut; mirror the existing P1-active
    // fixture in stack_session_batch_authority through the runner instead.
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
}

fn assert_entry_complete(state: &GameState, expected: u32) {
    let entrant = state
        .objects
        .values()
        .find(|object| object.name == "Test Entrant" && object.zone == Zone::Battlefield)
        .expect("the cast creature must have entered the battlefield");
    // CR 614.12: the counters belong to the entering creature.
    assert_eq!(
        entrant
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        expected
    );
    assert_eq!(state.stack.len(), 0, "the entire stack must resolve");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must finish at Priority, got {:?}",
        state.waiting_for
    );
}

fn assert_resolved(state: &GameState, expected: u32, sources: &[ObjectId]) {
    assert_entry_complete(state, expected);
    assert_eq!(state.pending_damage_replacements.len(), sources.len());
    for rider in &state.pending_damage_replacements {
        assert!(rider.is_consumed, "every installed rider must have applied");
    }
    let mut actual: Vec<_> = state
        .pending_damage_replacements
        .iter()
        .map(|rider| {
            rider
                .source_object
                .expect("rider must retain its installing source")
        })
        .collect();
    let mut expected_sources = sources.to_vec();
    actual.sort();
    expected_sources.sort();
    assert_eq!(actual, expected_sources);
}

#[test]
fn r1_brewing_reads_its_own_ingredient_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing = add_brewing(&mut scenario, P0, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, brewing);

    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 3, &[brewing]);
}

#[test]
fn r2_only_the_casters_brewing_installs_a_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p0_brewing = add_brewing(&mut scenario, P0, 5);
    let p1_brewing = add_brewing(&mut scenario, P1, 2);
    let spell = add_spell(&mut scenario, P1, 0);
    let mut runner = scenario.build();
    make_p1_active(&mut runner);
    assert_cast_trigger(&runner, p0_brewing);
    assert_cast_trigger(&runner, p1_brewing);

    // CR 109.5: only P1's "Whenever you cast" ability triggers here.
    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 2, &[p1_brewing]);
}

#[test]
fn r3_two_brewings_each_apply_their_own_count_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing_a = add_brewing(&mut scenario, P0, 3);
    let brewing_b = add_brewing(&mut scenario, P0, 2);
    let opposing_brewing = add_brewing(&mut scenario, P1, 7);
    let spell = add_spell(&mut scenario, P0, 0);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, brewing_a);
    assert_cast_trigger(&runner, brewing_b);
    assert_cast_trigger(&runner, opposing_brewing);

    let outcome = runner
        .cast(spell)
        .try_resolve()
        .expect("cast the non-stolen entrant and reach replacement ordering");
    // CR 616.1: two applicable replacement effects on one entering permanent
    // surface an ordering choice to its controller even when the order is
    // immaterial. Pin that boundary, prove its actor and the two distinct rider
    // identities (both Brewings share a name), then answer it.
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        candidates,
        kind,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "two Brewing riders must reach a ReplacementChoice ordering prompt, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*kind, ReplacementChoiceKind::Order);
    assert_eq!(*candidate_count, 2);
    assert_eq!(candidates.len(), 2);
    let mut sources: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.source_id)
        .collect();
    sources.sort();
    let mut expected = vec![brewing_a, brewing_b];
    expected.sort();
    assert_eq!(
        sources, expected,
        "same-named Brewings must expose distinct source identities"
    );
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == brewing_b)
        .expect("Brewing B must be a selectable replacement");
    assert_eq!(
        candidates[index].source_id, brewing_b,
        "choose Brewing B first"
    );
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("replacement ordering choice must be accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_resolved(runner.state(), 5, &[brewing_a, brewing_b]);
    assert_eq!(runner.state().objects[&spell].controller, P0);
}

#[test]
fn r4_preservation_p1_runadi_uses_the_entrants_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runadi = scenario
        .add_creature_from_oracle(P1, "Runadi, Behemoth Caller", 1, 3, RUNADI)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let spell = add_spell(&mut scenario, P1, 6);
    let mut runner = scenario.build();
    make_p1_active(&mut runner);
    assert_cast_trigger(&runner, runadi);

    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 4, &[runadi]);
}

#[test]
fn r5_preservation_archaic_uses_colors_spent_on_the_entrant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archaic = scenario
        .add_creature_from_oracle(P0, "Wildgrowth Archaic", 0, 0, ARCHAIC)
        .id();
    // Wildgrowth Archaic is a 0/0 that normally survives via its own converge
    // counters. As a pre-existing permanent it needs those counters in setup.
    scenario.with_counter(archaic, CounterType::Plus1Plus1, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, archaic);
    assert_eq!(
        runner.state().objects[&archaic]
            .colors_spent_to_cast
            .distinct_colors(),
        0,
        "the pre-existing source's payment facts differ from the entrant's"
    );

    let commit = runner.cast(spell).commit();
    let cast = commit
        .state()
        .objects
        .get(&spell)
        .expect("cast creature must exist on the stack");
    assert_eq!(cast.zone, Zone::Stack);
    assert_eq!(cast.colors_spent_to_cast.green, 1);
    assert_eq!(cast.colors_spent_to_cast.blue, 1);
    assert_eq!(cast.colors_spent_to_cast.distinct_colors(), 2);
    let outcome = commit.resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 2, &[archaic]);
}

#[test]
fn brewing_rider_follows_the_spell_after_aethersnatch() {
    let db = shared_card_db().expect("integration card fixture must be available");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing = add_brewing(&mut scenario, P0, 3);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let snatch = scenario.add_real_card(P1, "Aethersnatch", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Green]
            .into_iter()
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_mana_pool(
        P1,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .chain((0..2).map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, brewing);
    runner.cast(bears).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Bears and Brewing's cast trigger"
    );
    let cast_pin = ObjectIncarnationRef::from_object(&runner.state().objects[&bears]);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on Brewing's trigger");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve Brewing's trigger");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Stack);
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    let rider = &runner.state().pending_damage_replacements[0];
    assert_eq!(rider.source_object, Some(brewing));
    assert_eq!(rider.source_controller, Some(P0));
    assert_eq!(rider.triggering_spell, Some(cast_pin));
    assert!(!rider.is_consumed, "the rider is installed before theft");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("give P1 priority to steal Bears");
    let theft = runner
        .cast(snatch)
        .target_object(bears)
        .commit()
        .stop_at_optional()
        .try_resolve()
        .expect("resolve Aethersnatch up to its optional ChangeTargets");
    assert!(matches!(
        theft.final_waiting_for(),
        WaitingFor::OptionalEffectChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("explicitly decline choosing new targets");
    // CR 110.2b: the stolen permanent spell enters under its new controller.
    assert_eq!(runner.state().objects[&bears].controller, P1);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Stack);
    assert_eq!(
        ObjectIncarnationRef::from_object(&runner.state().objects[&bears]),
        cast_pin,
        "control change must not replace the triggering spell occurrence"
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on stolen Bears");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve stolen Bears");
    let entered = &runner.state().objects[&bears];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.controller, P1);
    // CR 614.12: Brewing modifies the identified entrant, not just P0's permanents.
    assert_eq!(entered.counters.get(&CounterType::Plus1Plus1), Some(&3));
    assert!(runner.state().pending_damage_replacements[0].is_consumed);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn runadi_rider_does_not_follow_a_remanded_recast(restore_after_remand: bool) {
    let db = shared_card_db().expect("integration card fixture must be available");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runadi = scenario.add_real_card(P0, "Runadi, Behemoth Caller", Zone::Battlefield, db);
    let dreadmaw = scenario.add_real_card(P0, "Colossal Dreadmaw", Zone::Hand, db);
    let remand = scenario.add_real_card(P0, "Remand", Zone::Hand, db);
    scenario.add_card_to_library_top(P0, "Island");
    scenario.add_card_to_library_top(P0, "Forest");
    // Two {4}{G}{G} casts and one {1}{U} Remand, all in the same main phase.
    scenario.with_mana_pool(
        P0,
        (0..9)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .chain((0..4).map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])))
            .chain(std::iter::once(ManaUnit::new(
                ManaType::Blue,
                ObjectId(0),
                false,
                vec![],
            )))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, runadi);
    runner.cast(dreadmaw).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Dreadmaw and its Runadi trigger"
    );
    let first_cast = ObjectIncarnationRef::from_object(&runner.state().objects[&dreadmaw]);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on the first Runadi trigger");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve the first Runadi trigger");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&dreadmaw].zone, Zone::Stack);
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    let stale_rider = runner.state().pending_damage_replacements[0].clone();
    assert_eq!(stale_rider.source_object, Some(runadi));
    assert_eq!(stale_rider.triggering_spell, Some(first_cast));
    assert!(!stale_rider.is_consumed, "the first rider must await entry");
    let library_before = runner.state().players[P0.0 as usize].library.len();
    runner
        .cast(remand)
        .target_object(dreadmaw)
        .try_resolve()
        .expect("Remand the first Dreadmaw and draw a card");
    assert_eq!(runner.state().objects[&remand].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&dreadmaw].zone, Zone::Hand);
    assert!(runner.state().players[P0.0 as usize]
        .hand
        .contains(&dreadmaw));
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before - 1
    );
    assert_eq!(
        runner.state().pending_damage_replacements,
        vec![stale_rider.clone()]
    );
    assert!(
        runner.state().stack.is_empty(),
        "recast only after Remand completes"
    );
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 400.7: Remand's zone change ended the first stack occurrence.
    assert!(!first_cast.is_current(runner.state()));

    if restore_after_remand {
        let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
            .expect("production-created state with a stale rider serializes");
        let restored = serde_json::from_str::<PersistedGameState>(&json)
            .expect("persisted state decodes")
            .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
            .expect("persisted recast state is admissible")
            .finalize_after_rehydration(|_| Ok(()))
            .expect("restored recast state is publishable");
        runner = GameRunner::from_state(restored);
        assert_eq!(runner.state().objects[&dreadmaw].zone, Zone::Hand);
        assert_eq!(runner.state().pending_damage_replacements.len(), 1);
        assert_eq!(
            runner.state().pending_damage_replacements[0].source_object,
            Some(runadi)
        );
        assert_eq!(
            runner.state().pending_damage_replacements[0].triggering_spell,
            Some(first_cast)
        );
        assert!(!runner.state().pending_damage_replacements[0].is_consumed);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
    }

    let stale_rider = runner.state().pending_damage_replacements[0].clone();
    runner.cast(dreadmaw).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "recast and its fresh Runadi trigger"
    );
    let second_cast = ObjectIncarnationRef::from_object(&runner.state().objects[&dreadmaw]);
    assert_eq!(second_cast.object_id, first_cast.object_id);
    assert_ne!(second_cast.incarnation, first_cast.incarnation);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on the recast's Runadi trigger");
    runner
        .act(GameAction::PassPriority)
        .expect("install the recast's fresh rider");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().pending_damage_replacements.len(), 2);
    assert_eq!(runner.state().pending_damage_replacements[0], stale_rider);
    assert_eq!(
        runner.state().pending_damage_replacements[1].source_object,
        Some(runadi)
    );
    assert_eq!(
        runner.state().pending_damage_replacements[1].triggering_spell,
        Some(second_cast)
    );
    assert!(!runner.state().pending_damage_replacements[1].is_consumed);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on the recast Dreadmaw");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve the recast Dreadmaw up to entry");
    // The broken raw-id path offers both riders. Answer its order prompt so
    // base fails on four counters versus two, not on an unanswered choice.
    if let WaitingFor::ReplacementChoice {
        player,
        kind,
        candidate_count,
        ..
    } = runner.state().waiting_for.clone()
    {
        assert_eq!(player, P0);
        assert_eq!(kind, ReplacementChoiceKind::Order);
        assert_eq!(candidate_count, 2);
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("order both raw-id riders on the broken path");
    }
    let entered = &runner.state().objects[&dreadmaw];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.controller, P0);
    // CR 400.7 + CR 614.12: only the recast's own rider modifies its entry.
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1),
        Some(&2),
        "the stale first-cast rider must not add another two counters"
    );
    assert_eq!(runner.state().pending_damage_replacements[0], stale_rider);
    assert!(!runner.state().pending_damage_replacements[0].is_consumed);
    assert!(runner.state().pending_damage_replacements[1].is_consumed);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn runadi_rider_rejects_the_same_card_after_remand_and_recast() {
    runadi_rider_does_not_follow_a_remanded_recast(false);
}

#[test]
fn restored_runadi_rider_rejects_the_same_card_after_remand_and_recast() {
    runadi_rider_does_not_follow_a_remanded_recast(true);
}

#[test]
fn departed_runadi_cast_trigger_installs_no_rider_or_installation_event() {
    let db = shared_card_db().expect("integration card fixture must be available");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runadi = scenario.add_real_card(P0, "Runadi, Behemoth Caller", Zone::Battlefield, db);
    let dreadmaw = scenario.add_real_card(P0, "Colossal Dreadmaw", Zone::Hand, db);
    let remand = scenario.add_real_card(P0, "Remand", Zone::Hand, db);
    scenario.add_card_to_library_top(P0, "Island");
    scenario.add_card_to_library_top(P0, "Forest");
    scenario.with_mana_pool(
        P0,
        (0..9)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .chain((0..4).map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])))
            .chain(std::iter::once(ManaUnit::new(
                ManaType::Blue,
                ObjectId(0),
                false,
                vec![],
            )))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, runadi);
    runner.cast(dreadmaw).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Dreadmaw and the old Runadi trigger"
    );
    assert_eq!(
        runner.state().stack.back().expect("cast trigger").source_id,
        runadi
    );
    let first_cast = ObjectIncarnationRef::from_object(&runner.state().objects[&dreadmaw]);
    runner.cast(remand).target_object(dreadmaw).commit();
    assert_eq!(
        runner.state().stack.len(),
        3,
        "Remand is above the unresolved trigger"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("pass on Remand");
    runner
        .act(GameAction::PassPriority)
        .expect("Remand Dreadmaw before its trigger resolves");
    assert_eq!(runner.state().objects[&remand].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&dreadmaw].zone, Zone::Hand);
    assert!(runner.state().players[P0.0 as usize]
        .hand
        .contains(&dreadmaw));
    assert_eq!(runner.state().players[P0.0 as usize].library.len(), 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .expect("old trigger remains")
            .source_id,
        runadi
    );
    assert!(!first_cast.is_current(runner.state()));
    let rider_count = runner.state().pending_damage_replacements.len();
    assert_eq!(rider_count, 0, "no cast trigger has resolved yet");
    let mut trigger_events = runner
        .act(GameAction::PassPriority)
        .expect("pass on the departed spell's trigger")
        .events;
    trigger_events.extend(
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the departed spell's trigger successfully")
            .events,
    );
    assert!(
        runner.state().stack.is_empty(),
        "the old trigger actually resolved"
    );
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 400.7 + CR 614.1c: the departed occurrence cannot acquire a future-entry rider.
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        rider_count,
        "the old trigger must install nothing, not a permanently stale replacement"
    );
    assert!(
        !trigger_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::AddTargetReplacement,
                ..
            }
        )),
        "a rejected installation must not emit an installation event: {trigger_events:?}"
    );

    runner.cast(dreadmaw).commit();
    assert_eq!(runner.state().stack.len(), 2);
    let second_cast = ObjectIncarnationRef::from_object(&runner.state().objects[&dreadmaw]);
    assert_eq!(second_cast.object_id, first_cast.object_id);
    assert_ne!(second_cast.incarnation, first_cast.incarnation);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on the new Runadi trigger");
    let installed = runner
        .act(GameAction::PassPriority)
        .expect("resolve the new Runadi trigger");
    assert!(installed.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::AddTargetReplacement,
            source_id,
            ..
        } if *source_id == runadi
    )));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    assert_eq!(
        runner.state().pending_damage_replacements[0].source_object,
        Some(runadi)
    );
    assert_eq!(
        runner.state().pending_damage_replacements[0].triggering_spell,
        Some(second_cast)
    );
    assert!(!runner.state().pending_damage_replacements[0].is_consumed);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on the recast Dreadmaw");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve the recast Dreadmaw");
    let entered = &runner.state().objects[&dreadmaw];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.counters.get(&CounterType::Plus1Plus1), Some(&2));
    assert!(runner.state().pending_damage_replacements[0].is_consumed);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn tenacious_pup_boon_follows_a_stolen_spell_and_is_used_only_once() {
    let db = shared_card_db().expect("integration card fixture must be available");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pup = scenario.add_real_card(P0, "Tenacious Pup", Zone::Hand, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let later_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let snatch = scenario.add_real_card(P1, "Aethersnatch", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .chain((0..3).map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])))
            .collect(),
    );
    scenario.with_mana_pool(
        P1,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .chain((0..2).map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])))
            .collect(),
    );
    let mut runner = scenario.build();
    runner
        .cast(pup)
        .try_resolve()
        .expect("cast Tenacious Pup and resolve its real ETB boon creation");
    assert_eq!(runner.state().objects[&pup].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    let holders: Vec<_> = runner
        .state()
        .delayed_triggers
        .iter()
        .filter(|trigger| trigger.kind.is_boon())
        .map(|trigger| trigger.controller)
        .collect();
    assert_eq!(holders, vec![P0], "Pup's ETB actually created P0's boon");
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    runner.cast(bears).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Bears and the boon's real cast trigger"
    );
    assert_eq!(runner.state().objects[&bears].zone, Zone::Stack);
    let cast_pin = ObjectIncarnationRef::from_object(&runner.state().objects[&bears]);
    assert!(
        !runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| trigger.kind.is_boon()),
        "the previously observed one-time boon is spent when it triggers"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("pass on the boon's cast trigger");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve the boon's cast trigger");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    assert_eq!(
        runner.state().pending_damage_replacements[0].triggering_spell,
        Some(cast_pin)
    );
    assert!(!runner.state().pending_damage_replacements[0].is_consumed);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("give P1 priority for Aethersnatch");
    let theft = runner
        .cast(snatch)
        .target_object(bears)
        .commit()
        .stop_at_optional()
        .try_resolve()
        .expect("resolve Aethersnatch up to its optional ChangeTargets");
    assert!(matches!(
        theft.final_waiting_for(),
        WaitingFor::OptionalEffectChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("explicitly decline choosing new targets for Bears");
    assert_eq!(runner.state().objects[&bears].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&bears].controller, P1);
    assert_eq!(
        ObjectIncarnationRef::from_object(&runner.state().objects[&bears]),
        cast_pin
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner
        .act(GameAction::PassPriority)
        .expect("pass on the stolen boon recipient");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve the stolen boon recipient");
    let granted_counters = [
        CounterType::Plus1Plus1,
        CounterType::Keyword(KeywordKind::Trample),
        CounterType::Keyword(KeywordKind::Vigilance),
    ];
    let entered = &runner.state().objects[&bears];
    assert_eq!(entered.zone, Zone::Battlefield);
    // CR 110.2b + CR 614.12: the new controller gets the same identified entrant and boon.
    assert_eq!(entered.controller, P1);
    for counter in &granted_counters {
        assert_eq!(
            entered.counters.get(counter),
            Some(&1),
            "missing {counter:?}"
        );
    }
    assert!(runner.state().pending_damage_replacements[0].is_consumed);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    runner
        .cast(later_bears)
        .try_resolve()
        .expect("cast and resolve a later creature after the boon was spent");
    let later = &runner.state().objects[&later_bears];
    assert_eq!(later.zone, Zone::Battlefield);
    assert_eq!(later.controller, P0);
    for counter in &granted_counters {
        assert_eq!(
            later.counters.get(counter).copied().unwrap_or(0),
            0,
            "the consumed boon must not grant another {counter:?}"
        );
    }
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    assert!(runner.state().pending_damage_replacements[0].is_consumed);
    assert!(!runner
        .state()
        .delayed_triggers
        .iter()
        .any(|trigger| trigger.kind.is_boon()));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// Synthetic response spell: change counters through a production cast, without
// changing the parsed Communal Brewing trigger under test.
fn ingredient_response(scenario: &mut GameScenario, brewing: ObjectId) -> ObjectId {
    use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
    scenario
        .add_spell_to_hand(P0, "Ingredient response", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::PutCounter {
            counter_type: CounterType::Generic("ingredient".to_string()),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SpecificObject { id: brewing },
        })
        .id()
}

fn departure_response(scenario: &mut GameScenario) -> ObjectId {
    // Naturalize, verbatim Oracle. Cost is waived only by the fixture.
    scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Naturalize",
            true,
            "Destroy target artifact or enchantment.",
        )
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn commit_brewing_creature(runner: &mut GameRunner, brewing: ObjectId, spell: ObjectId) {
    assert_cast_trigger(runner, brewing);
    runner.cast(spell).commit();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().stack.len(), 2, "creature and cast trigger");
}

fn resolve_brewing_trigger(runner: &mut GameRunner) {
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1, "creature still waiting");
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    assert!(runner.state().current_trigger_event.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn brewing_ingredient_growth_before_trigger_resolution_is_counted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing = add_brewing(&mut scenario, P0, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let response = ingredient_response(&mut scenario, brewing);
    let mut runner = scenario.build();
    commit_brewing_creature(&mut runner, brewing, spell);
    runner.cast(response).commit();
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(
        runner.state().objects[&brewing].counters[&CounterType::Generic("ingredient".into())],
        4
    );
    // CR 611.2d + CR 608.2h: X uses information at trigger resolution.
    runner.advance_until_stack_empty();
    assert_resolved(runner.state(), 4, &[brewing]);
}

#[test]
fn brewing_ingredient_growth_after_trigger_resolution_is_not_counted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing = add_brewing(&mut scenario, P0, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let response = ingredient_response(&mut scenario, brewing);
    let mut runner = scenario.build();
    commit_brewing_creature(&mut runner, brewing, spell);
    resolve_brewing_trigger(&mut runner);
    runner.cast(response).commit();
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner.state().objects[&brewing].counters[&CounterType::Generic("ingredient".into())],
        4
    );
    // CR 611.2d: the installed effect retains the already determined X.
    runner.advance_until_stack_empty();
    assert_resolved(runner.state(), 3, &[brewing]);
}

#[test]
fn brewing_departure_after_trigger_resolution_preserves_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing = add_brewing(&mut scenario, P0, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let naturalize = departure_response(&mut scenario);
    let mut runner = scenario.build();
    commit_brewing_creature(&mut runner, brewing, spell);
    resolve_brewing_trigger(&mut runner);
    runner.cast(naturalize).target_object(brewing).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&brewing].zone, Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 1);
    // CR 113.7a + CR 611.2d: source departure cannot change the installed X.
    runner.advance_until_stack_empty();
    assert_resolved(runner.state(), 3, &[brewing]);
}

#[test]
fn brewing_departure_before_trigger_resolution_preserves_unchanged_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing = add_brewing(&mut scenario, P0, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let naturalize = departure_response(&mut scenario);
    let mut runner = scenario.build();
    commit_brewing_creature(&mut runner, brewing, spell);
    runner.cast(naturalize).target_object(brewing).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&brewing].zone, Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 2);
    // CR 113.7a + CR 608.2h: the independent trigger uses source information.
    runner.advance_until_stack_empty();
    assert_entry_complete(runner.state(), 3);
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    assert!(runner.state().pending_damage_replacements[0].is_consumed);
}

// Known source-quantity limitation (probed through production casts): if the
// ingredient count changes from 3 to 4 after triggering and Brewing then leaves
// before resolution, the trigger's source snapshot still supplies 3 rather than
// departure-time 4. Fixing that source/LKI authority is outside this install seam.
