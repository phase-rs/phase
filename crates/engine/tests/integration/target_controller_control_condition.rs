//! A trailing "if" condition on the controller of an instruction's declared
//! object target, checked as the instruction resolves (CR 608.2c + CR 608.2h).
//!
//! - Skulking Killer: "… target creature an opponent controls gets -2/-2 until
//!   end of turn if that opponent controls no other creatures." The ability
//!   triggers regardless; if the target's controller controls another creature
//!   as it resolves, it does nothing (2021-11-19 ruling).
//! - Unified Will: "Counter target spell if you control more creatures than that
//!   spell's controller."
//! - A SYNTHETIC instant: Skulking Killer's instruction and suffix followed by
//!   an independent "Draw a card.", which happens whether or not the gate holds.
//!
//! Every other card is a printed card staged from its verbatim Oracle text
//! (`client/public/card-data.json`). Hill Giant, Grizzly Bears and Craw Wurm are
//! vanilla.
//!
//! REVERT DISCRIMINATORS: `skulking_killer_does_nothing_when_that_opponent_controls_another_creature`
//! (the condition is dropped → the Giant becomes 1/1),
//! `unified_will_does_not_counter_when_counts_are_equal` (the condition is
//! dropped → the Giant is countered), and
//! `gate_fails_and_the_independent_draw_still_resolves` (the condition is
//! dropped → the Giant becomes 1/1).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Comparator, ControllerRef, Duration, Effect, FilterProp, QuantityExpr,
    QuantityRef, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Skulking Killer {3}{B} 4/2 — verbatim Oracle text.
const SKULKING_KILLER: &str = "When this creature enters, target creature an opponent controls \
                               gets -2/-2 until end of turn if that opponent controls no other \
                               creatures.";

/// Unified Will {1}{U} — verbatim Oracle text.
const UNIFIED_WILL: &str =
    "Counter target spell if you control more creatures than that spell's controller.";

/// Murder {1}{B}{B} — verbatim Oracle text.
const MURDER: &str = "Destroy target creature.";

const P2: PlayerId = PlayerId(2);

fn pt(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = &runner.state().objects[&id];
    (obj.power, obj.toughness)
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn assert_resolved(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "the stack must be empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete at a priority window, got {:?}",
        runner.state().waiting_for
    );
}

/// The `ObjectCount` filter a `QuantityExpr` reads, if any.
fn object_count_filter(expr: &QuantityExpr) -> Option<&TargetFilter> {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => Some(filter),
        _ => None,
    }
}

fn typed_controller(filter: &TargetFilter) -> Option<&ControllerRef> {
    match filter {
        TargetFilter::Typed(tf) => tf.controller.as_ref(),
        _ => None,
    }
}

/// Cast Skulking Killer from P0's hand and put its enters trigger on the stack
/// targeting `target`. Asserts the trigger is on the stack with that target
/// unconditionally (CR 603.4 does not apply: the condition is not an
/// intervening-if, so the ability always triggers).
fn cast_killer_targeting(runner: &mut GameRunner, killer: ObjectId, target: ObjectId) {
    runner.cast(killer).commit();
    while zone(runner, killer) == Zone::Stack {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must resolve Skulking Killer");
    }
    assert_eq!(zone(runner, killer), Zone::Battlefield);
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { .. } => {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(target)],
                })
                .expect("the targeted creature must be a legal target");
        }
        // The engine chooses a lone legal target automatically.
        WaitingFor::Priority { .. } => {}
        other => panic!("unexpected state after Skulking Killer entered: {other:?}"),
    }
    let top = runner
        .state()
        .stack
        .back()
        .expect("the enters trigger must be on the stack");
    assert_eq!(top.source_id, killer);
    assert_eq!(
        top.ability().map(|a| a.targets.clone()),
        Some(vec![TargetRef::Object(target)]),
        "the trigger targets the chosen creature"
    );
}

/// P1 (parse): the trailing condition is re-homed onto the instruction, the
/// duration survives, and nothing is left as a warning.
#[test]
fn skulking_killer_parses_target_controller_condition_and_duration() {
    let parsed = parse_oracle_text(
        SKULKING_KILLER,
        "Skulking Killer",
        &[],
        &["Creature".to_owned()],
        &["Vampire".to_owned(), "Assassin".to_owned()],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "{:?}",
        parsed.parse_warnings
    );
    let [trigger] = parsed.triggers.as_slice() else {
        panic!("expected one trigger, got {:?}", parsed.triggers);
    };
    assert!(trigger.condition.is_none(), "not an intervening-if");
    let execute = trigger.execute.as_ref().expect("trigger body");
    assert!(
        matches!(&*execute.effect, Effect::Pump { .. }),
        "reach guard: {:?}",
        execute.effect
    );
    assert_eq!(execute.duration, Some(Duration::UntilEndOfTurn));
    let Some(AbilityCondition::QuantityCheck {
        lhs,
        comparator: Comparator::EQ,
        rhs: QuantityExpr::Fixed { value: 0 },
    }) = &execute.condition
    else {
        panic!("expected a zero-count gate, got {:?}", execute.condition);
    };
    let filter = object_count_filter(lhs).expect("ObjectCount");
    assert_eq!(
        typed_controller(filter),
        Some(&ControllerRef::ParentTargetController)
    );
    let TargetFilter::Typed(tf) = filter else {
        unreachable!()
    };
    assert!(tf.properties.contains(&FilterProp::DistinctFrom {
        reference: Box::new(TargetFilter::ParentTarget),
    }));
}

/// P2 (parse): Unified Will compares your creature count against the target
/// spell's controller's.
#[test]
fn unified_will_parses_target_controller_comparison() {
    let parsed = parse_oracle_text(
        UNIFIED_WILL,
        "Unified Will",
        &[],
        &["Instant".to_owned()],
        &[],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "{:?}",
        parsed.parse_warnings
    );
    let [ability] = parsed.abilities.as_slice() else {
        panic!("expected one ability, got {:?}", parsed.abilities);
    };
    assert!(
        matches!(&*ability.effect, Effect::Counter { .. }),
        "reach guard: {:?}",
        ability.effect
    );
    let Some(AbilityCondition::QuantityCheck {
        lhs,
        comparator: Comparator::GT,
        rhs,
    }) = &ability.condition
    else {
        panic!("expected a GT comparison, got {:?}", ability.condition);
    };
    assert_eq!(
        object_count_filter(lhs).and_then(typed_controller),
        Some(&ControllerRef::You)
    );
    assert_eq!(
        object_count_filter(rhs).and_then(typed_controller),
        Some(&ControllerRef::ParentTargetController)
    );
}

/// R1: the opponent controls only the target → -2/-2. P0's own Grizzly Bears
/// do not count ("that opponent"), and "other" excludes only the target.
#[test]
fn skulking_killer_shrinks_target_when_that_opponent_controls_no_other_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let killer = scenario
        .add_creature_to_hand_from_oracle(P0, "Skulking Killer", 4, 2, SKULKING_KILLER)
        .id();
    let own_bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    cast_killer_targeting(&mut runner, killer, giant);
    runner.advance_until_stack_empty();

    assert_resolved(&runner);
    assert_eq!(pt(&runner, giant), (Some(1), Some(1)));
    assert_eq!(pt(&runner, own_bears), (Some(2), Some(2)));
}

/// R2: the target's controller controls another creature → the ability does
/// nothing (it still triggered and targeted).
#[test]
fn skulking_killer_does_nothing_when_that_opponent_controls_another_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let killer = scenario
        .add_creature_to_hand_from_oracle(P0, "Skulking Killer", 4, 2, SKULKING_KILLER)
        .id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    cast_killer_targeting(&mut runner, killer, giant);
    runner.advance_until_stack_empty();

    assert_resolved(&runner);
    assert_eq!(pt(&runner, giant), (Some(3), Some(3)));
    assert_eq!(pt(&runner, bears), (Some(2), Some(2)));
}

/// R3: CR 608.2h — the check is made as the ability resolves. With the trigger
/// on the stack, Murder destroys the opponent's other creature; the ability
/// then applies.
#[test]
fn skulking_killer_checks_the_condition_on_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let killer = scenario
        .add_creature_to_hand_from_oracle(P0, "Skulking Killer", 4, 2, SKULKING_KILLER)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    cast_killer_targeting(&mut runner, killer, giant);
    runner.cast(murder).target_objects(&[bears]).resolve();
    runner.advance_until_stack_empty();

    assert_resolved(&runner);
    assert_eq!(zone(&runner, bears), Zone::Graveyard);
    assert_eq!(pt(&runner, giant), (Some(1), Some(1)));
}

/// R4: "that opponent" is the target's controller, not any opponent.
/// Case A: P1 controls only the target while P2 controls two creatures → applies.
#[test]
fn skulking_killer_reads_the_targets_controller_not_another_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let killer = scenario
        .add_creature_to_hand_from_oracle(P0, "Skulking Killer", 4, 2, SKULKING_KILLER)
        .id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    scenario.add_creature(P2, "Grizzly Bears", 2, 2);
    scenario.add_creature(P2, "Grizzly Bears", 2, 2);
    let mut runner = scenario.build();

    cast_killer_targeting(&mut runner, killer, giant);
    runner.advance_until_stack_empty();

    assert_resolved(&runner);
    assert_eq!(pt(&runner, giant), (Some(1), Some(1)));
}

/// R4 Case B: P1 controls the target and another creature while P2 controls
/// none → nothing (an existential "an opponent controls no other creatures"
/// reading would apply).
#[test]
fn skulking_killer_ignores_an_opponent_with_no_creatures() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let killer = scenario
        .add_creature_to_hand_from_oracle(P0, "Skulking Killer", 4, 2, SKULKING_KILLER)
        .id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    cast_killer_targeting(&mut runner, killer, giant);
    runner.advance_until_stack_empty();

    assert_resolved(&runner);
    assert_eq!(pt(&runner, giant), (Some(3), Some(3)));
    assert_eq!(pt(&runner, bears), (Some(2), Some(2)));
}

/// `caster` is the active player in their precombat main phase (CR 117.1a)
/// and casts Hill Giant; after they pass priority P0 casts Unified Will
/// targeting it, and the stack resolves.
fn unified_will_against_hill_giant(
    mut scenario: GameScenario,
    caster: PlayerId,
) -> (GameRunner, ObjectId, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    let will = scenario
        .add_spell_to_hand_from_oracle(P0, "Unified Will", true, UNIFIED_WILL)
        .id();
    let giant = scenario
        .add_creature_to_hand(caster, "Hill Giant", 3, 3)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = caster;
        state.priority_player = caster;
        state.waiting_for = WaitingFor::Priority { player: caster };
    }
    runner.cast(giant).commit();
    assert_eq!(zone(&runner, giant), Zone::Stack);
    while runner.state().priority_player != P0 {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority with Hill Giant on the stack");
    }
    assert_eq!(
        zone(&runner, giant),
        Zone::Stack,
        "Hill Giant still on the stack"
    );
    runner.cast(will).target_objects(&[giant]).resolve();
    runner.advance_until_stack_empty();
    assert_resolved(&runner);
    (runner, will, giant)
}

/// U-R1: you control more creatures than the spell's controller → countered.
#[test]
fn unified_will_counters_when_you_control_more_creatures() {
    let mut scenario = GameScenario::new();
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    let (runner, will, giant) = unified_will_against_hill_giant(scenario, P1);

    assert_eq!(zone(&runner, will), Zone::Graveyard);
    assert_eq!(zone(&runner, giant), Zone::Graveyard);
}

/// U-R2: equal counts → not countered.
#[test]
fn unified_will_does_not_counter_when_counts_are_equal() {
    let mut scenario = GameScenario::new();
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let (runner, will, giant) = unified_will_against_hill_giant(scenario, P1);

    assert_eq!(zone(&runner, will), Zone::Graveyard);
    assert_eq!(zone(&runner, giant), Zone::Battlefield);
    assert_eq!(runner.state().objects[&giant].controller, P1);
}

/// U-R3: "that spell's controller" is the target's controller, not any
/// opponent. P2 casts Hill Giant with no other creatures; P1 controls three
/// creatures; P0 controls one → countered.
#[test]
fn unified_will_compares_against_that_spells_controller() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    for _ in 0..3 {
        scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    }
    let (runner, will, giant) = unified_will_against_hill_giant(scenario, P2);

    assert_eq!(zone(&runner, will), Zone::Graveyard);
    assert_eq!(zone(&runner, giant), Zone::Graveyard);
}

/// SYNTHETIC instant: Skulking Killer's printed instruction and suffix as a
/// spell, followed by an independent "Draw a card." The suffix gates only its
/// own single-node instruction, so the draw is that node's independent next
/// instruction and happens whether or not the gate holds (CR 608.2c, "in the
/// order written").
const SHRINK_THEN_DRAW: &str = "Target creature an opponent controls gets -2/-2 until end of \
                                turn if that opponent controls no other creatures. Draw a card.";

/// P0 casts the synthetic instant targeting P1's Hill Giant with one card in
/// P0's library; returns the runner, the Giant and that card.
fn shrink_then_draw_against_hill_giant(
    p1_second_creature: bool,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Shrink Then Draw", true, SHRINK_THEN_DRAW)
        .id();
    let drawn = scenario.add_card_to_library_top(P0, "Grizzly Bears");
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    if p1_second_creature {
        scenario.add_creature(P1, "Craw Wurm", 6, 4);
    }
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[giant]).resolve();
    runner.advance_until_stack_empty();
    assert_resolved(&runner);
    (runner, giant, drawn)
}

/// T-R1: CR 608.2c: the gate holds (P1 controls only the Giant), so both the
/// gated instruction and the independent draw resolve.
#[test]
fn gate_holds_then_the_independent_draw_resolves() {
    let (runner, giant, drawn) = shrink_then_draw_against_hill_giant(false);

    assert_eq!(pt(&runner, giant), (Some(1), Some(1)));
    assert_eq!(zone(&runner, drawn), Zone::Hand);
}

/// T-R2: CR 608.2c: the gate fails (P1 controls another creature), so the
/// gated instruction does nothing, and the independent draw still resolves.
#[test]
fn gate_fails_and_the_independent_draw_still_resolves() {
    let (runner, giant, drawn) = shrink_then_draw_against_hill_giant(true);

    assert_eq!(pt(&runner, giant), (Some(3), Some(3)));
    assert_eq!(zone(&runner, drawn), Zone::Hand);
}
