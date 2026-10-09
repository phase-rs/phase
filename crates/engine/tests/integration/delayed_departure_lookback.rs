//! Runtime coverage for delayed triggered abilities that must remember the
//! battlefield-departure event that created them.

use engine::ai_support::legal_actions;
use engine::game::coverage::card_face_gaps;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ActivationRestriction, ContinuousModification, Effect,
    StaticCondition, StaticDefinition, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;
use serde_json::Value;
use std::sync::Arc;

const VINDICATE: &str = "Destroy target permanent.";
const MAKESHIFT_MANNEQUIN: &str = "Return target creature card from your graveyard to the battlefield with a mannequin counter on it. For as long as that creature has a mannequin counter on it, it has \"When this creature becomes the target of a spell or ability, sacrifice it.\"";
const CHAOS_WARP: &str = "The owner of target permanent shuffles it into their library, then reveals the top card of their library. If it's a permanent card, they put it onto the battlefield.";
const SOUL_SUMMONS: &str = "Manifest the top card of your library. (Put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)";
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const SHIVAN_DRAGON: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn.";
const GENERAL_FERROUS_ROKIRIC: &str = "Hexproof from monocolored\nWhenever you cast a multicolored spell, create a 4/4 red and white Golem artifact creature token.";

const DELAYED_CREATE: &str = "Whenever another creature you control dies, if it had one or more counters on it, at the beginning of your next end step, create that many 1/1 white Soldier creature tokens.";
const DELAYED_SURVEIL: &str = "Whenever another creature you control dies, if it had one or more counters on it, at the beginning of your next end step, surveil that many.";
const DELAYED_FRACTAL: &str = "When this creature dies, if it had one or more counters on it, at the beginning of your next end step, create a 0/0 green and blue Fractal creature token, then put this creature's counters on that token.";
const DELAYED_PUT_ITS: &str = "When this creature dies, at the beginning of the next end step, put its counters on target creature you control.";
const OTHER_SUBJECT_ORACLE: &str = "Whenever a creature you control dies, when another creature you control dies this turn, you gain life equal to the number of counters it had.";
const EVENT_DELAYED_COUNTERS: &str = "Whenever a creature you control dies, choose target creature you control. When that creature dies this turn, you gain life equal to the number of counters it had.";
const DELAYED_POWER: &str =
    "When this creature dies, at the beginning of the next end step, you gain life equal to its power.";
const DELAYED_MV: &str = "When this creature dies, at the beginning of the next end step, you gain life equal to its mana value.";
const DELAYED_OTHER_MV: &str = "Whenever another creature you control dies, at the beginning of your next end step, you gain life equal to its mana value.";
const DELAYED_MANA_SYMBOLS: &str = "Whenever another creature you control dies, at the beginning of your next end step, you gain life equal to the number of red mana symbols in that spell's mana cost.";
const IMMEDIATE_MANA_SYMBOLS: &str = "Whenever another creature you control dies, you gain life equal to the number of red mana symbols in that spell's mana cost.";
const DELAYED_NAME_WORDS: &str = "Whenever another creature you control dies, at the beginning of the next end step, you gain life equal to the number of words in that spell's name.";
const DELAYED_COLORS: &str = "Whenever another creature you control dies, at the beginning of the next end step, you gain 1 life for each of that spell's colors.";
const LEAK_ORACLE: &str = "Whenever another permanent you control leaves the battlefield, at the beginning of your next end step, create a 1/1 white Soldier creature token.";
const HAS_COUNTER_FLYING: &str = "As long as this creature has a counter on it, it has flying.";

fn generic_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn one_mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

fn seed_libraries(scenario: &mut GameScenario) {
    for i in 0..8 {
        scenario.add_card_to_library_top(P0, &format!("P0 filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 filler {i}"));
    }
}

fn p0_life(runner: &GameRunner) -> i32 {
    runner.life(P0)
}

fn counter(object: &engine::game::game_object::GameObject, kind: CounterType) -> u32 {
    object.counters.get(&kind).copied().unwrap_or(0)
}

fn add_counter_pair(scenario: &mut GameScenario, object: ObjectId) {
    scenario.with_counter(object, CounterType::Plus1Plus1, 2);
    scenario.with_counter(object, CounterType::Generic("oil".to_string()), 1);
}

fn tokens_with_subtype(runner: &GameRunner, subtype: &str) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner.state().objects[id].is_token
                && runner.state().objects[id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(subtype))
        })
        .collect()
}

fn pass_to_delayed_trigger(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order delayed trigger");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward delayed trigger");
            }
            WaitingFor::Priority { .. } => return,
            WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::ScryChoice { .. } => return,
            other => panic!("unexpected state while advancing to delayed trigger: {other:?}"),
        }
    }
    panic!("delayed trigger did not surface");
}

fn choose_trigger_target(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose delayed trigger target");
                return;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected state before trigger target: {other:?}"),
        }
    }
    panic!("target prompt did not surface");
}

fn resolve_delayed_targeting(runner: &mut GameRunner, target: ObjectId) {
    pass_to_delayed_trigger(runner);
    // With a single legal target the engine may lock the target in without a
    // prompt; answer the prompt only when it surfaces.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        choose_trigger_target(runner, target);
    }
    runner.advance_until_stack_empty();
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach-guard: the delayed trigger fired and resolved"
    );
}

fn spell_face(name: &str, oracle: &str, types: &[&str]) -> CardFace {
    let types = types.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

fn debug_face(name: &str, oracle: &str, types: &[&str]) -> String {
    format!("{:#?}", spell_face(name, oracle, types))
}

fn assert_effect_shape(name: &str, oracle: &str, types: &[&str], needles: &[&str]) {
    let debug = debug_face(name, oracle, types);
    for needle in needles {
        assert!(
            debug.contains(needle),
            "parsed shape must contain {needle}; got {debug}"
        );
    }
}

fn face_has_unimplemented(face: &CardFace, tag: &str) -> bool {
    fn ability_has(def: &AbilityDefinition, tag: &str) -> bool {
        matches!(&*def.effect, Effect::Unimplemented { name, .. } if name == tag)
            || def
                .sub_ability
                .as_deref()
                .is_some_and(|sub| ability_has(sub, tag))
            || def
                .else_ability
                .as_deref()
                .is_some_and(|sub| ability_has(sub, tag))
    }
    face.abilities.iter().any(|a| ability_has(a, tag))
        || face
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .any(|effect| ability_has(effect, tag))
}

fn face_down_objects(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].face_down)
        .collect()
}

fn answer_copy_prompts(runner: &mut GameRunner, copy_target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept copy replacement");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept copy optional");
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                assert!(
                    valid_targets.contains(&copy_target),
                    "reach-guard: copy choice must offer Shivan Dragon"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(copy_target)),
                    })
                    .expect("choose copy target");
                return;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected clone entry prompt: {other:?}"),
        }
    }
    panic!("copy prompt loop exhausted");
}

#[test]
fn delayed_create_that_many_uses_departure_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    seed_libraries(&mut scenario);
    scenario.with_mana_pool(P0, generic_mana(3));
    scenario.add_enchantment_from_oracle(P0, "Delayed Soldier Engine", DELAYED_CREATE);
    let victim = scenario.add_creature(P0, "Countered Myr", 2, 2).id();
    add_counter_pair(&mut scenario, victim);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(outcome.zone_of(victim), Zone::Graveyard);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the departure installed one delayed trigger"
    );
    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach-guard: the delayed trigger fired and resolved"
    );

    assert_eq!(
        tokens_with_subtype(&runner, "Soldier").len(),
        3,
        "CR 603.7 + CR 608.2h + CR 122.2: the delayed trigger uses the departed creature's three counters"
    );
}

#[test]
fn delayed_surveil_that_many_uses_departure_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    seed_libraries(&mut scenario);
    scenario.with_mana_pool(P0, generic_mana(3));
    scenario.add_enchantment_from_oracle(P0, "Delayed Surveil Engine", DELAYED_SURVEIL);
    let victim = scenario.add_creature(P0, "Countered Scout", 2, 2).id();
    add_counter_pair(&mut scenario, victim);
    for name in ["Top One", "Top Two", "Top Three", "Top Four"] {
        scenario.add_card_to_library_top(P0, name);
    }
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(outcome.zone_of(victim), Zone::Graveyard);
    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();

    match runner.state().waiting_for.clone() {
        WaitingFor::SurveilChoice { cards, .. } => assert_eq!(cards.len(), 3),
        other => panic!("expected surveil choice with three cards, got {other:?}"),
    }
}

fn delayed_fractal_board(reanimate: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    seed_libraries(&mut scenario);
    scenario.with_mana_pool(P0, generic_mana(7));
    let source = scenario
        .add_creature_from_oracle(P0, "Delayed Augmenter", 1, 1, DELAYED_FRACTAL)
        .id();
    add_counter_pair(&mut scenario, source);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let mannequin = scenario
        .add_spell_to_hand_from_oracle(P0, "Makeshift Mannequin", true, MAKESHIFT_MANNEQUIN)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(destroy).target_object(source).resolve();
    assert_eq!(outcome.zone_of(source), Zone::Graveyard);
    if reanimate {
        let outcome = runner.cast(mannequin).target_object(source).resolve();
        assert_eq!(outcome.zone_of(source), Zone::Battlefield);
        assert_eq!(
            outcome.counters(source, CounterType::Generic("mannequin".to_string())),
            1,
            "reach-guard: the returned creature has its mannequin counter"
        );
    }
    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    runner
}

#[test]
fn delayed_put_this_creatures_counters_ignores_reanimated_object() {
    let runner = delayed_fractal_board(true);
    let fractals = tokens_with_subtype(&runner, "Fractal");
    assert_eq!(fractals.len(), 1, "one Fractal token must survive");
    let obj = &runner.state().objects[&fractals[0]];
    assert_eq!(counter(obj, CounterType::Plus1Plus1), 2);
    assert_eq!(counter(obj, CounterType::Generic("oil".to_string())), 1);
    assert_eq!(
        counter(obj, CounterType::Generic("mannequin".to_string())),
        0,
        "CR 400.7 + CR 122.8: the token must not copy counters from the reanimated new object"
    );
}

#[test]
fn delayed_put_this_creatures_counters_after_death() {
    let runner = delayed_fractal_board(false);
    let fractals = tokens_with_subtype(&runner, "Fractal");
    assert_eq!(fractals.len(), 1, "one Fractal token must survive");
    let obj = &runner.state().objects[&fractals[0]];
    assert_eq!(counter(obj, CounterType::Plus1Plus1), 2);
    assert_eq!(counter(obj, CounterType::Generic("oil".to_string())), 1);
}

fn put_its_board(reanimate: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    seed_libraries(&mut scenario);
    scenario.with_mana_pool(P0, generic_mana(7));
    let source = scenario
        .add_creature_from_oracle(P0, "Counter Carrier", 2, 2, DELAYED_PUT_ITS)
        .id();
    let target = scenario.add_creature(P0, "Recipient", 2, 2).id();
    scenario.with_counter(source, CounterType::Plus1Plus1, 3);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let mannequin = scenario
        .add_spell_to_hand_from_oracle(P0, "Makeshift Mannequin", true, MAKESHIFT_MANNEQUIN)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(destroy).target_object(source).resolve();
    assert_eq!(outcome.zone_of(source), Zone::Graveyard);
    if reanimate {
        runner.cast(mannequin).target_object(source).resolve();
    }
    resolve_delayed_targeting(&mut runner, target);
    assert_eq!(
        counter(
            &runner.state().objects[&target],
            CounterType::Generic("mannequin".to_string())
        ),
        0,
        "the target must not receive a mannequin counter"
    );
    runner
}

#[test]
fn delayed_put_its_counters_on_target_creature() {
    for reanimate in [false, true] {
        let runner = put_its_board(reanimate);
        let target = runner
            .state()
            .objects
            .values()
            .find(|obj| obj.name == "Recipient")
            .expect("recipient");
        assert_eq!(
            counter(target, CounterType::Plus1Plus1),
            3,
            "reanimate={reanimate}: CR 122.8 copies the departed creature's three +1/+1 counters"
        );
    }
}

#[test]
fn another_creature_dies_this_turn_delayed_subject_is_strict_unsupported() {
    let face = spell_face(
        "Strict Unsupported Probe",
        OTHER_SUBJECT_ORACLE,
        &["Enchantment"],
    );
    assert!(
        face_has_unimplemented(&face, "delayed_trigger_other_subject"),
        "the inner delayed clause must strict-fail by name: {face:#?}"
    );
    let gaps = card_face_gaps(&face);
    assert!(
        gaps.iter()
            .any(|gap| gap.contains("delayed_trigger_other_subject")),
        "coverage must report the strict-fail gap, got {gaps:?}"
    );
    let debug = format!("{face:#?}");
    assert!(debug.contains("ChangesZone"));
    assert!(debug.contains("Creature"));
    assert!(debug.contains("You"));
}

#[test]
fn another_creature_delayed_does_not_fire_on_the_enchantment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana(6));
    let enchantment = scenario
        .add_enchantment_from_oracle(P0, "Unsupported A", OTHER_SUBJECT_ORACLE)
        .id();
    let x = scenario.add_creature(P0, "Creature X", 2, 2).id();
    scenario.with_counter(x, CounterType::Plus1Plus1, 2);
    let destroy_x = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let destroy_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();

    let mut runner = scenario.build();
    runner.cast(destroy_x).target_object(x).resolve();
    assert_eq!(
        runner.state().objects[&x].zone,
        Zone::Graveyard,
        "reach-guard: X died"
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the outer trigger, if any, has resolved"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "unsupported inner delayed trigger must not be installed"
    );
    let life_before = p0_life(&runner);

    let outcome = runner.cast(destroy_a).target_object(enchantment).resolve();
    assert_eq!(outcome.zone_of(enchantment), Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().delayed_triggers.is_empty());
    assert_eq!(
        p0_life(&runner),
        life_before,
        "life is unchanged because no trigger was installed or fired"
    );
}

#[test]
fn event_delayed_counters_it_had_reads_its_own_event() {
    let debug = debug_face(
        "Event Delayed Counter Engine",
        EVENT_DELAYED_COUNTERS,
        &["Enchantment"],
    );
    assert!(
        debug.contains("WhenDies") || debug.contains("WhenNextEvent"),
        "inner delayed trigger must be event-delayed; got {debug}"
    );
    assert!(
        !debug.contains("AtNextPhase"),
        "inner delayed trigger must not be phase-delayed; got {debug}"
    );
    assert!(
        debug.contains("CountersOn"),
        "expected CountersOn ref; got {debug}"
    );
    assert!(
        debug.contains("EventSource"),
        "expected CountersOn with EventSource scope; got {debug}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_libraries(&mut scenario);
    scenario.with_mana_pool(P0, generic_mana(6));
    scenario.add_enchantment_from_oracle(
        P0,
        "Event Delayed Counter Engine",
        EVENT_DELAYED_COUNTERS,
    );
    let x = scenario.add_creature(P0, "Creature X", 2, 2).id();
    let y = scenario.add_creature(P0, "Creature Y", 2, 2).id();
    scenario.with_counter(x, CounterType::Plus1Plus1, 2);
    scenario.with_counter(y, CounterType::Plus1Plus1, 5);
    let destroy_x = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let destroy_y = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();

    let mut runner = scenario.build();
    runner
        .cast(destroy_x)
        .target_object(x)
        .target_object(y)
        .resolve();
    assert_eq!(runner.state().objects[&x].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the outer trigger installed one event-delayed trigger"
    );
    assert!(
        runner.state().delayed_triggers[0]
            .ability
            .context
            .creation_lookback_event
            .is_none(),
        "event-delayed triggers must not be stamped with phase creation lookback"
    );

    let before = p0_life(&runner);
    runner.cast(destroy_y).target_object(y).resolve();
    assert_eq!(runner.state().objects[&y].zone, Zone::Graveyard);
    assert_eq!(
        p0_life(&runner) - before,
        5,
        "CR 603.7 + CR 608.2h: event-delayed trigger reads Y's own departure counters"
    );
}

fn delayed_life_from_creature(oracle: &str, counters: u32) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    seed_libraries(&mut scenario);
    scenario.with_mana_pool(P0, generic_mana(3));
    let creature = scenario
        .add_creature_from_oracle(P0, "Lookback Creature", 4, 4, oracle)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_counter(creature, CounterType::Plus1Plus1, counters);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let mut runner = scenario.build();
    let life_before = p0_life(&runner);
    let outcome = runner.cast(destroy).target_object(creature).resolve();
    assert_eq!(outcome.zone_of(creature), Zone::Graveyard);
    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    p0_life(&runner) - life_before
}

#[test]
fn delayed_gain_life_equal_to_its_power_uses_lki() {
    assert_eq!(delayed_life_from_creature(DELAYED_POWER, 0), 4);
    assert_eq!(delayed_life_from_creature(DELAYED_POWER, 2), 6);
    assert_eq!(delayed_life_from_creature(DELAYED_MV, 0), 3);
}

#[test]
fn delayed_its_mana_value_reads_face_down_lki_not_reanimated_object() {
    assert_effect_shape(
        "Mana Value Enchantment",
        DELAYED_OTHER_MV,
        &["Enchantment"],
        &["ObjectManaValue", "Anaphoric"],
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(P0, generic_mana(7));
    scenario.add_enchantment_from_oracle(P0, "Delayed MV Engine", DELAYED_OTHER_MV);
    let shivan = scenario
        .add_spell_to_library_top(P0, "Shivan Dragon", false)
        .as_creature()
        .with_subtypes(vec!["Dragon"])
        .with_mana_cost(shivan_cost())
        .from_oracle_text_with_keywords(&["Flying"], SHIVAN_DRAGON)
        .id();
    let soul = scenario
        .add_spell_to_hand_from_oracle(P0, "Soul Summons", false, SOUL_SUMMONS)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let mannequin = scenario
        .add_spell_to_hand_from_oracle(P0, "Makeshift Mannequin", true, MAKESHIFT_MANNEQUIN)
        .id();
    let mut runner = scenario.build();
    runner.cast(soul).resolve();
    let manifested = face_down_objects(&runner);
    assert_eq!(
        manifested,
        vec![shivan],
        "reach-guard: Soul Summons manifests Shivan"
    );
    assert_eq!(
        runner.state().objects[&shivan].effective_mana_value(),
        0,
        "CR 708.2a: face-down battlefield permanent has mana value 0"
    );

    let life_before = p0_life(&runner);
    runner.cast(destroy).target_object(shivan).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert!(
        runner.state().delayed_triggers[0]
            .ability
            .context
            .creation_lookback_event
            .is_some(),
        "reach-guard: delayed ability is stamped with creation lookback"
    );
    runner.cast(mannequin).target_object(shivan).resolve();
    assert_eq!(runner.state().objects[&shivan].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&shivan].effective_mana_value() > 0,
        "reach-guard: live returned object is face up with printed mana value"
    );
    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    assert_eq!(
        p0_life(&runner) - life_before,
        0,
        "CR 608.2h: the delayed trigger reads face-down departure LKI, not the reanimated object"
    );
}

#[test]
fn delayed_characteristic_counts_read_departure_lki() {
    assert_effect_shape(
        "Name Word Engine",
        DELAYED_NAME_WORDS,
        &["Enchantment"],
        &["ObjectNameWordCount", "EventSource"],
    );
    assert_effect_shape(
        "Color Engine",
        DELAYED_COLORS,
        &["Enchantment"],
        &["ObjectColorCount", "EventSource"],
    );

    fn run(oracle: &str, expected_life: i32) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        seed_libraries(&mut scenario);
        let mut mana = generic_mana(3);
        mana.push(one_mana(ManaType::Blue));
        scenario.with_mana_pool(P0, mana);
        scenario.add_enchantment_from_oracle(P0, "Characteristic Engine", oracle);
        let copied = scenario
            .add_creature_from_oracle(P0, "General Ferrous Rokiric", 3, 1, GENERAL_FERROUS_ROKIRIC)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::White],
                generic: 1,
            })
            .id();
        let clone = scenario
            .add_creature_to_hand_from_oracle(P0, "Clone", 0, 0, CLONE)
            .with_subtypes(vec!["Shapeshifter"])
            .with_mana_cost(clone_cost())
            .id();
        let destroy = scenario
            .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
            .id();

        let mut runner = scenario.build();
        runner.cast(clone).resolve();
        answer_copy_prompts(&mut runner, copied);
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&clone].name,
            "General Ferrous Rokiric",
            "reach-guard: Clone copied a three-word, two-color creature"
        );
        runner.state_mut().phase = Phase::PostCombatMain;
        runner.state_mut().priority_player = P0;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

        let before = p0_life(&runner);
        runner.cast(destroy).target_object(clone).resolve();
        assert_eq!(
            runner.state().objects[&clone].name,
            "Clone",
            "reach-guard: the live graveyard card is printed Clone"
        );
        assert_eq!(runner.state().objects[&clone].zone, Zone::Graveyard);
        pass_to_delayed_trigger(&mut runner);
        runner.advance_until_stack_empty();
        assert_eq!(p0_life(&runner) - before, expected_life);
    }

    run(DELAYED_NAME_WORDS, 3);
    run(DELAYED_COLORS, 2);

    // Type-line reader reach note: the natural wordings tried did not produce
    // `ObjectTypelineComponentCount { EventSource | Anaphoric }` in this parser:
    // "the number of card types that spell has" -> unsupported generic quantity;
    // "the number of types in that spell's type line" -> unsupported generic
    // quantity; "the number of card types among that spell's types" ->
    // unsupported generic quantity. No runtime leg is hand-built.
}

fn find_creation_lookbacks<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "creation_lookback_event" {
                    out.push(child);
                }
                find_creation_lookbacks(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                find_creation_lookbacks(child, out);
            }
        }
        _ => {}
    }
}

/// CR 400.2 + CR 401.2: the NEW carrier (`creation_lookback_event`) is redacted
/// in every viewer projection. This checks the carrier, not whole-payload
/// privacy at the moment of the move: the projection is taken after step
/// transitions have cleared the pre-existing, unredacted LKI caches
/// (`lki_cache`, `lki_by_incarnation`, `lki_copiable_values`), which are
/// out of scope here and tracked separately. The whole-JSON name check below is
/// therefore evidence about this carrier only, not a privacy proof at priority
/// in the step where the card moved.
#[test]
fn creation_lookback_event_is_redacted_in_every_viewer_projection() {
    let secret_name = "Secret Leak Sentinel";
    let mut scenario = GameScenario::new_n_player(2, 99);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana(3));
    scenario.add_enchantment_from_oracle(P0, "Delayed Leak Engine", LEAK_ORACLE);
    let secret = scenario.add_creature(P0, secret_name, 2, 2).id();
    for name in ["Lightning Bolt", "Opt", "Counterspell", "Brainstorm"] {
        scenario.add_spell_to_library_top(P0, name, true);
    }
    let warp = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaos Warp", true, CHAOS_WARP)
        .id();

    let mut runner = scenario.build();
    runner.cast(warp).target_object(secret).resolve();
    assert_eq!(
        runner.state().objects[&secret].zone,
        Zone::Library,
        "reach-guard: Chaos Warp puts C into P0's hidden library"
    );
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    let authoritative = runner.state().delayed_triggers[0]
        .ability
        .context
        .creation_lookback_event
        .as_ref()
        .expect("authoritative state is stamped");
    assert!(
        format!("{authoritative:?}").contains(secret_name),
        "authoritative state retains the unredacted record"
    );
    runner.advance_to_phase(Phase::PostCombatMain);
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the delayed trigger remains installed after step transitions"
    );
    // `lki_cache` / `lki_by_incarnation` / `lki_copiable_values` are
    // pre-existing upstream LKI carriers that the viewer projection does not
    // redact. They clear at step transitions, so this test isolates the new
    // `creation_lookback_event` carrier rather than pinning the old policy.
    assert!(
        runner.state().lki_cache.get(&secret).is_none(),
        "reach-guard: pre-existing lki_cache carrier is out of scope"
    );
    assert!(
        runner.state().lki_by_incarnation.get(&secret).is_none(),
        "reach-guard: pre-existing lki_by_incarnation carrier is out of scope"
    );

    for projected in [
        filter_state_for_viewer(runner.state(), P0),
        filter_state_for_viewer(runner.state(), P1),
        filter_state_for_unseated_viewer(runner.state()),
    ] {
        assert_eq!(projected.delayed_triggers.len(), 1);
        let json = serde_json::to_value(&projected).expect("projected state serializes");
        let serialized = serde_json::to_string(&json).expect("json string");
        assert!(
            !serialized.contains(secret_name),
            "projected JSON must not contain hidden card name: {serialized}"
        );
        let mut carriers = Vec::new();
        find_creation_lookbacks(&json, &mut carriers);
        assert!(
            !carriers.is_empty(),
            "projection must still contain the carrier"
        );
        for carrier in carriers {
            let text = carrier.to_string();
            assert!(
                text.contains("Hidden Card"),
                "record name must be hidden: {text}"
            );
            assert!(
                !text.contains("trigger_source_context")
                    || text.contains("\"trigger_source_context\":null"),
                "trigger source context must be absent/null: {text}"
            );
        }
    }
}

#[test]
fn has_a_counter_static_saturates_counter_total() {
    for (charge, oil) in [(u32::MAX, 1), (1, 0)] {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature_from_oracle(P0, "Counter Flyer", 2, 2, HAS_COUNTER_FLYING)
            .id();
        scenario.with_counter(creature, CounterType::Generic("charge".to_string()), charge);
        scenario.with_counter(creature, CounterType::Generic("oil".to_string()), oil);
        let mut runner = scenario.build();
        runner.state_mut().layers_dirty.mark_full();
        evaluate_layers(runner.state_mut());
        assert!(
            runner.state().objects[&creature].has_keyword(&Keyword::Flying),
            "CR 122.1: any positive exact total satisfies the lower-bound static"
        );
    }
}

fn shivan_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 4,
    }
}

fn clone_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 3,
    }
}

fn red_pips(cost: &ManaCost) -> usize {
    match cost {
        ManaCost::Cost { shards, .. } => shards
            .iter()
            .filter(|shard| matches!(shard, ManaCostShard::Red))
            .count(),
        _ => 0,
    }
}

fn setup_clone_as_shivan(delayed: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    seed_libraries(&mut scenario);
    let mut p0_mana = generic_mana(3);
    p0_mana.push(one_mana(ManaType::Blue));
    scenario.with_mana_pool(P0, p0_mana);
    scenario.add_enchantment_from_oracle(
        P0,
        "Mana Symbol Engine",
        if delayed {
            DELAYED_MANA_SYMBOLS
        } else {
            IMMEDIATE_MANA_SYMBOLS
        },
    );
    let shivan = scenario
        .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, SHIVAN_DRAGON)
        .with_subtypes(vec!["Dragon"])
        .with_mana_cost(shivan_cost())
        .id();
    let clone = scenario
        .add_creature_to_hand_from_oracle(P0, "Clone", 0, 0, CLONE)
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(clone_cost())
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();

    let mut runner = scenario.build();
    runner.cast(clone).resolve();
    answer_copy_prompts(&mut runner, shivan);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&clone].name,
        "Shivan Dragon",
        "reach-guard: Clone is a copy of Shivan Dragon on the battlefield"
    );
    runner.state_mut().phase = Phase::PostCombatMain;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    (runner, clone, destroy)
}

#[test]
fn departure_mana_symbols_read_the_copied_cost_not_the_printed_card() {
    assert_effect_shape(
        "Mana Symbol Engine",
        DELAYED_MANA_SYMBOLS,
        &["Enchantment"],
        &["ManaSymbolsInManaCost", "EventSource", "Red"],
    );

    let (mut runner, clone, destroy) = setup_clone_as_shivan(true);
    let life_before = p0_life(&runner);
    runner.cast(destroy).target_object(clone).resolve();
    assert_eq!(
        runner.state().objects[&clone].zone,
        Zone::Graveyard,
        "reach-guard: Clone died"
    );
    assert_eq!(
        runner.state().objects[&clone].name,
        "Clone",
        "reach-guard: the graveyard object is the printed Clone again"
    );
    assert_eq!(
        red_pips(&runner.state().objects[&clone].mana_cost),
        0,
        "reach-guard: live graveyard Clone has no red mana symbols"
    );
    let stamped = runner.state().delayed_triggers[0]
        .ability
        .context
        .creation_lookback_event
        .as_ref()
        .expect("stamped delayed ability");
    let stamped_debug = format!("{stamped:#?}");
    assert!(stamped_debug.contains("Shivan Dragon"));
    assert!(stamped_debug.contains("Red"));
    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    assert_eq!(p0_life(&runner) - life_before, 2);

    let (mut immediate, clone, destroy) = setup_clone_as_shivan(false);
    let life_before = p0_life(&immediate);
    immediate.cast(destroy).target_object(clone).resolve();
    assert_eq!(p0_life(&immediate) - life_before, 2);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(P0, generic_mana(5));
    scenario.add_enchantment_from_oracle(P0, "Mana Symbol Engine", DELAYED_MANA_SYMBOLS);
    let shivan = scenario
        .add_spell_to_library_top(P0, "Shivan Dragon", false)
        .as_creature()
        .with_subtypes(vec!["Dragon"])
        .with_mana_cost(shivan_cost())
        .from_oracle_text_with_keywords(&["Flying"], SHIVAN_DRAGON)
        .id();
    let soul = scenario
        .add_spell_to_hand_from_oracle(P0, "Soul Summons", false, SOUL_SUMMONS)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let mut runner = scenario.build();
    runner.cast(soul).resolve();
    assert_eq!(face_down_objects(&runner), vec![shivan]);
    let life_before = p0_life(&runner);
    runner.cast(destroy).target_object(shivan).resolve();
    assert_eq!(
        red_pips(&runner.state().objects[&shivan].mana_cost),
        2,
        "reach-guard: the live graveyard card is printed Shivan"
    );
    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    assert_eq!(
        p0_life(&runner) - life_before,
        0,
        "CR 708.2a + CR 202.1: face-down departure had no mana cost"
    );
}

fn restricted_artifact_runner(counters: &[(CounterType, u32)]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Threshold Relic", "{T}: You gain 1 life.")
        .id();
    for (kind, count) in counters {
        scenario.with_counter(artifact, kind.clone(), *count);
    }
    let mut runner = scenario.build();
    let ability = Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&artifact)
            .unwrap()
            .abilities,
    )
    .iter_mut()
    .find(|ability| ability.kind == AbilityKind::Activated)
    .expect("activated ability");
    ability
        .activation_restrictions
        .push(ActivationRestriction::CounterThreshold {
            counters: CounterMatch::Any,
            minimum: 0,
            maximum: Some(u32::MAX),
        });
    let updated = ability.clone();
    let base = Arc::make_mut(
        &mut runner
            .state_mut()
            .objects
            .get_mut(&artifact)
            .unwrap()
            .base_abilities,
    )
    .iter_mut()
    .find(|ability| ability.kind == AbilityKind::Activated)
    .expect("base activated ability");
    *base = updated;
    (runner, artifact)
}

#[test]
fn counter_threshold_activation_upper_bound_uses_exact_total() {
    let charge = CounterType::Generic("charge".to_string());
    let oil = CounterType::Generic("oil".to_string());
    let (runner, artifact) = restricted_artifact_runner(&[(charge.clone(), u32::MAX), (oil, 1)]);
    assert!(
        !legal_actions(runner.state()).contains(&GameAction::ActivateAbility {
            source_id: artifact,
            ability_index: 0,
        }),
        "u32::MAX + 1 counters exceeds the upper bound"
    );

    let (mut runner, artifact) = restricted_artifact_runner(&[(charge, u32::MAX)]);
    assert!(
        legal_actions(runner.state()).contains(&GameAction::ActivateAbility {
            source_id: artifact,
            ability_index: 0,
        }),
        "exactly u32::MAX counters satisfies the upper bound"
    );
    let before = p0_life(&runner);
    runner.activate(artifact, 0).resolve();
    assert_eq!(p0_life(&runner) - before, 1);
}

fn static_counter_bound_has_flying(counters: &[(CounterType, u32)]) -> bool {
    let mut scenario = GameScenario::new();
    let static_def = StaticDefinition::new(StaticMode::Continuous)
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }])
        .condition(StaticCondition::HasCounters {
            counters: CounterMatch::Any,
            minimum: 0,
            maximum: Some(u32::MAX),
        });
    let creature = scenario
        .add_creature(P0, "Bounded Static Creature", 2, 2)
        .with_static_definition(static_def)
        .id();
    for (kind, count) in counters {
        scenario.with_counter(creature, kind.clone(), *count);
    }
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner.state().objects[&creature].has_keyword(&Keyword::Flying)
}

#[test]
fn has_counters_static_upper_bound_uses_exact_total() {
    let charge = CounterType::Generic("charge".to_string());
    let oil = CounterType::Generic("oil".to_string());
    assert!(
        !static_counter_bound_has_flying(&[(charge.clone(), u32::MAX), (oil, 1)]),
        "u32::MAX + 1 counters exceeds the upper bound"
    );
    assert!(
        static_counter_bound_has_flying(&[(charge, u32::MAX)]),
        "exactly u32::MAX counters satisfies the upper bound"
    );
}
