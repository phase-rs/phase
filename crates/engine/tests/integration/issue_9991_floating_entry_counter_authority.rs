//! Floating entry-counter riders retain their installing source (issue #9991).
//!
//! CR 109.5: "you" refers to the triggered ability's controller.
//! CR 113.7a: a triggered ability exists independently of its source.
//! CR 608.2h: source information must come from the specified object.
//! CR 614.12: replacements modify how a permanent enters the battlefield.
//! CR 616.1: the affected object's controller chooses among replacements.
//! These fixtures keep source information unchanged between trigger and entry;
//! resolution-time X snapshots and last-known information are separate work.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, ReplacementChoiceKind, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::{ObjectId, PlayerId};

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

fn assert_resolved(state: &GameState, expected: u32, sources: &[ObjectId]) {
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
    assert_eq!(state.stack.len(), 0, "the entire stack must resolve");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must finish at Priority, got {:?}",
        state.waiting_for
    );
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
    let spell = add_spell(&mut scenario, P0, 0);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, brewing_a);
    assert_cast_trigger(&runner, brewing_b);

    let outcome = runner.cast(spell).resolve();
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
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("replacement ordering choice must be accepted");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_resolved(runner.state(), 5, &[brewing_a, brewing_b]);
}

#[test]
fn r4_preservation_p1_runadi_uses_the_entrants_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runadi = scenario
        .add_creature_from_oracle(P1, "Runadi, Behemoth Caller", 1, 3, RUNADI)
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
