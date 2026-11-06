//! Chandra, Fire Artisan — "Whenever one or more loyalty counters are removed
//! from Chandra, she deals that much damage to target opponent or
//! planeswalker." — and the counter-removal trigger class it belongs to.
//!
//! CR 122.1: counters are markers on an object; removing them is the trigger
//! event. CR 603.2c: an ability triggers once each time its trigger event
//! occurs, so a batched "one or more … counters are removed" trigger fires once
//! per simultaneous removal on each recipient, and "that much" reads the whole
//! removal. CR 120.3c / CR 306.8: damage dealt to a planeswalker removes that
//! many loyalty counters from it. CR 608.2h + CR 113.7a: the trigger resolves
//! with its source's last known information after Chandra has left the
//! battlefield (Scryfall ruling: "If all of Chandra's loyalty counters are
//! removed, her first ability has her deal damage even though she's left the
//! battlefield.").
//!
//! CR 603.2c + CR 122.1: a singular "Whenever a [type] counter is removed"
//! trigger fires once for EACH counter removed — one removal event of N
//! counters contains N occurrences (Protean Hydra ruling: "If multiple +1/+1
//! counters are removed from Protean Hydra at once, its last ability will
//! trigger that many times."). CR 704.5q: the +1/+1 / -1/-1 annihilation
//! state-based action removes counters through the counter-removal authority,
//! so it emits `CounterRemoved` events and these triggers see it (Protean Hydra
//! ruling: "that -1/-1 counter and a +1/+1 counter will be removed from it as a
//! state-based action. This will cause Protean Hydra's last ability to
//! trigger.").
//!
//! Note (code reading, no test): `apply_counter_removal` pushes no
//! `CounterRemoved` event when a replacement leaves nothing to remove, so a
//! fully prevented removal never reaches these triggers.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::ControllerRef;
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const CHANDRA: &str = "Whenever one or more loyalty counters are removed from Chandra, she deals that much damage to target opponent or planeswalker.\n[+1]: Exile the top card of your library. You may play it this turn.\n[−7]: Exile the top seven cards of your library. You may play them this turn.";
const JACE_BELEREN: &str = "[+2]: Each player draws a card.\n[−1]: Target player draws a card.\n[−10]: Target player mills twenty cards.";
const LILIANA_OF_THE_VEIL: &str = "[+1]: Each player discards a card.\n[−2]: Target player sacrifices a creature.\n[−6]: Separate all permanents target player controls into two piles. That player sacrifices all permanents in the pile of their choice.";
const CLOUDSCULPT_ARMORER: &str = "When this creature enters, put a shield counter on target artifact or creature.\nWhenever one or more counters are removed from a permanent you control, seek a nonland card.";
const PROTEAN_HYDRA: &str = "This creature enters with X +1/+1 counters on it.\nIf damage would be dealt to this creature, prevent that damage and remove that many +1/+1 counters from it.\nWhenever a +1/+1 counter is removed from this creature, put two +1/+1 counters on it at the beginning of the next end step.";
const VAMPIRE_HEXMAGE: &str =
    "First strike\nSacrifice this creature: Remove all counters from target permanent.";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

/// Index of Chandra's `[−7]` loyalty ability (`[+1]` is index 0).
const CHANDRA_MINUS_SEVEN: usize = 1;
/// Index of Jace Beleren's `[−1]` loyalty ability.
const JACE_MINUS_ONE: usize = 1;

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Forest", "Island", "Swamp", "Mountain", "Plains", "Forest", "Island", "Swamp",
        ],
    );
    scenario.with_library_top(P1, &["Forest", "Island", "Swamp"]);
    scenario
}

fn loyalty(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

/// Triggered abilities from `source` currently on the stack, with their
/// stashed batched subject count.
fn triggers_from(runner: &GameRunner, source: ObjectId) -> Vec<Option<u32>> {
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                subject_match_count,
                ..
            } if *source_id == source => Some(*subject_match_count),
            _ => None,
        })
        .collect()
}

fn act(runner: &mut GameRunner, action: GameAction, events: &mut Vec<GameEvent>) {
    let result = runner
        .act(action)
        .expect("the engine must accept the action");
    events.extend(result.events);
}

/// Answer a pending trigger-target prompt with `target`, asserting it is
/// offered. Returns the legal targets the prompt listed.
fn choose_trigger_target(
    runner: &mut GameRunner,
    target: TargetRef,
    events: &mut Vec<GameEvent>,
) -> Vec<TargetRef> {
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a trigger target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal = target_slots[selection.current_slot].legal_targets.clone();
    assert!(
        legal.contains(&target),
        "{target:?} must be a legal target: {legal:?}"
    );
    act(
        runner,
        GameAction::ChooseTarget {
            target: Some(target),
        },
        events,
    );
    legal
}

/// Pass priority (draining trigger ordering) until `done` holds or the engine
/// stops at a prompt this driver does not answer.
fn pass_until(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    done: impl Fn(&GameRunner) -> bool,
) {
    for _ in 0..64 {
        if done(runner) {
            return;
        }
        match runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => act(runner, GameAction::PassPriority, events),
            _ => return,
        }
    }
}

fn is_trigger_target_prompt(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    )
}

fn counter_removed_from(events: &[GameEvent], id: ObjectId) -> bool {
    events.iter().any(
        |event| matches!(event, GameEvent::CounterRemoved { object_id, .. } if *object_id == id),
    )
}

/// Row 1: Chandra's own `[−7]` cost removes seven loyalty counters, so her
/// trigger deals 7 to the chosen opponent — even though she dies to the
/// zero-loyalty state-based action before it resolves.
#[test]
fn minus_seven_cost_deals_seven_to_target_opponent() {
    let mut scenario = main_phase();
    let chandra = scenario
        .add_planeswalker_from_oracle(P0, "Chandra, Fire Artisan", "Chandra", 7, CHANDRA)
        .id();
    let mut runner = scenario.build();

    // Reach guard: the printed trigger parses to a typed, batched removal trigger.
    let trigger = runner.state().objects[&chandra]
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| entry.definition.clone())
        .find(|t| t.mode == TriggerMode::CounterRemoved)
        .expect("Chandra's first line is a CounterRemoved trigger, not Unknown");
    assert!(trigger.batched);
    assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));

    let mut events = Vec::new();
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: chandra,
            ability_index: CHANDRA_MINUS_SEVEN,
            payment_mode: CastPaymentMode::Auto,
        },
        &mut events,
    );
    assert!(
        counter_removed_from(&events, chandra),
        "the loyalty cost removes counters from Chandra"
    );
    if is_trigger_target_prompt(&runner) {
        choose_trigger_target(&mut runner, TargetRef::Player(P1), &mut events);
    }
    assert_eq!(
        triggers_from(&runner, chandra).len(),
        1,
        "exactly one Chandra trigger is on the stack above her ability"
    );

    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&chandra].zone,
        Zone::Graveyard,
        "Chandra died to the zero-loyalty state-based action"
    );
    assert_eq!(
        runner.life(P1),
        13,
        "the trigger deals that much (7) damage to the opponent"
    );
}

/// Row 2: damage dealt to Chandra removes loyalty counters (CR 120.3c), and the
/// trigger deals that much damage.
#[test]
fn damage_to_chandra_deals_that_much_to_target_opponent() {
    let mut scenario = main_phase();
    let chandra = scenario
        .add_planeswalker_from_oracle(P0, "Chandra, Fire Artisan", "Chandra", 7, CHANDRA)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
        .id();
    let mut runner = scenario.build();

    runner.cast(bolt).target_object(chandra).commit();
    let mut events = Vec::new();
    pass_until(&mut runner, &mut events, is_trigger_target_prompt);
    assert_eq!(
        loyalty(&runner, chandra),
        4,
        "reach guard: Bolt's 3 damage removed three loyalty counters"
    );
    let legal = choose_trigger_target(&mut runner, TargetRef::Player(P1), &mut events);
    assert!(
        legal.contains(&TargetRef::Object(chandra)),
        "the opponent-or-planeswalker slot also offers planeswalkers: {legal:?}"
    );
    assert_eq!(triggers_from(&runner, chandra).len(), 1);

    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert_eq!(runner.life(P1), 17, "that much = 3 damage");
}

/// Row 3: two attackers deal combat damage to Chandra simultaneously. The
/// removals are one event, so she triggers once and deals the summed 5.
#[test]
fn simultaneous_combat_damage_triggers_once_for_the_sum() {
    let mut scenario = main_phase();
    let chandra = scenario
        .add_planeswalker_from_oracle(P0, "Chandra, Fire Artisan", "Chandra", 7, CHANDRA)
        .id();
    let small = scenario.add_creature(P1, "Two Power Attacker", 2, 2).id();
    let large = scenario.add_creature(P1, "Three Power Attacker", 3, 3).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();

    let mut events = Vec::new();
    run_combat_until_stack_or_prompt(
        &mut runner,
        &[
            (small, AttackTarget::Planeswalker(chandra)),
            (large, AttackTarget::Planeswalker(chandra)),
        ],
        &mut events,
    );
    assert_eq!(
        loyalty(&runner, chandra),
        2,
        "reach guard: both attackers' damage removed loyalty (7 - 5)"
    );
    choose_trigger_target(&mut runner, TargetRef::Player(P1), &mut events);
    assert_eq!(
        triggers_from(&runner, chandra).len(),
        1,
        "one removal event, one trigger"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no second trigger target prompt: {:?}",
        runner.state().waiting_for
    );

    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert_eq!(runner.life(P1), 15, "that much = 2 + 3 = 5 damage");
}

/// Row 4: counters removed from a different planeswalker do not trigger
/// Chandra. The paired positive twin is row 1 (same scaffolding, her own cost).
#[test]
fn counters_removed_from_another_planeswalker_do_not_trigger_chandra() {
    let mut scenario = main_phase();
    let chandra = scenario
        .add_planeswalker_from_oracle(P0, "Chandra, Fire Artisan", "Chandra", 7, CHANDRA)
        .id();
    let jace = scenario
        .add_planeswalker_from_oracle(P0, "Jace Beleren", "Jace", 3, JACE_BELEREN)
        .id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: jace,
            ability_index: JACE_MINUS_ONE,
            payment_mode: CastPaymentMode::Auto,
        },
        &mut events,
    );
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        act(
            &mut runner,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            },
            &mut events,
        );
    }
    // Reach guard: the removal really happened, on Jace.
    assert!(counter_removed_from(&events, jace));
    assert_eq!(loyalty(&runner, jace), 2);
    assert!(
        !is_trigger_target_prompt(&runner),
        "Chandra must not trigger: {:?}",
        runner.state().waiting_for
    );
    assert!(triggers_from(&runner, chandra).is_empty());

    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert_eq!(runner.life(P1), 20);
    assert_eq!(loyalty(&runner, chandra), 7);
}

/// Activate Vampire Hexmage at `target` and pass priority until its ability
/// has left the stack, returning the collected events.
fn hexmage_remove_all_counters(
    runner: &mut GameRunner,
    hexmage: ObjectId,
    target: ObjectId,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    act(
        runner,
        GameAction::ActivateAbility {
            source_id: hexmage,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
        &mut events,
    );
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        act(
            runner,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            },
            &mut events,
        );
    }
    pass_until(runner, &mut events, |r| {
        !r.state().stack.iter().any(|entry| {
            matches!(entry.kind, StackEntryKind::ActivatedAbility { .. })
                && entry.source_id == hexmage
        })
    });
    events
}

/// Row 5: the singular typed form fires only for its named counter kind, once
/// per counter of that kind removed (CR 603.2c). A shield counter removed from
/// Protean Hydra does not trigger it; two +1/+1 counters removed in the same
/// way trigger it twice.
#[test]
fn singular_typed_removal_trigger_fires_only_for_its_kind() {
    // Negative: only a shield counter is removed.
    let mut scenario = main_phase();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Protean Hydra", 0, 1, PROTEAN_HYDRA)
        .id();
    let hexmage = scenario
        .add_creature_from_oracle(P0, "Vampire Hexmage", 2, 1, VAMPIRE_HEXMAGE)
        .id();
    scenario.with_counter(hydra, CounterType::Shield, 1);
    let mut runner = scenario.build();
    let events = hexmage_remove_all_counters(&mut runner, hexmage, hydra);
    drain_order_triggers_with_identity(runner.state_mut());
    assert!(
        counter_removed_from(&events, hydra),
        "reach guard: the shield counter was removed from the Hydra"
    );
    assert!(
        triggers_from(&runner, hydra).is_empty(),
        "a shield counter is not a +1/+1 counter"
    );

    // Positive: a +1/+1 counter is removed alongside the shield counter.
    let mut scenario = main_phase();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Protean Hydra", 0, 1, PROTEAN_HYDRA)
        .id();
    let hexmage = scenario
        .add_creature_from_oracle(P0, "Vampire Hexmage", 2, 1, VAMPIRE_HEXMAGE)
        .id();
    scenario.with_counter(hydra, CounterType::Shield, 1);
    scenario.with_counter(hydra, CounterType::Plus1Plus1, 2);
    let mut runner = scenario.build();
    let events = hexmage_remove_all_counters(&mut runner, hexmage, hydra);
    drain_order_triggers_with_identity(runner.state_mut());
    assert!(counter_removed_from(&events, hydra));
    assert_eq!(
        triggers_from(&runner, hydra).len(),
        2,
        "each of the two +1/+1 counters removed triggers the Hydra (the shield removal does not)"
    );
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Build a P0 Protean Hydra with `counters` +1/+1 counters and Lightning Bolt
/// in P0's hand, cast Bolt at the Hydra and pass priority until Bolt has left
/// the stack, then place any triggers (identity order) without passing
/// priority. Returns the runner, the Hydra and the collected events.
fn bolt_protean_hydra(counters: u32) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut scenario = main_phase();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Protean Hydra", 0, 1, PROTEAN_HYDRA)
        .id();
    scenario.with_counter(hydra, CounterType::Plus1Plus1, counters);
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
        .id();
    let mut runner = scenario.build();
    runner.cast(bolt).target_object(hydra).commit();
    let mut events = Vec::new();
    pass_until(&mut runner, &mut events, |r| {
        !r.state().stack.iter().any(|entry| entry.source_id == bolt)
    });
    drain_order_triggers_with_identity(runner.state_mut());
    (runner, hydra, events)
}

/// Pass priority through the rest of the turn (no attackers, no blockers,
/// identity trigger order) until `done` holds.
fn drive_turn_until(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    done: impl Fn(&GameRunner) -> bool,
) {
    for _ in 0..200 {
        if done(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => act(runner, GameAction::PassPriority, events),
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                let result = runner
                    .declare_attackers(&[])
                    .expect("an empty attack declaration must be accepted");
                events.extend(result.events);
            }
            WaitingFor::DeclareBlockers { .. } => {
                let result = runner
                    .declare_blockers(&[])
                    .expect("no-block declaration must be accepted");
                events.extend(result.events);
            }
            other => panic!("turn driver stopped at an unexpected prompt: {other:?}"),
        }
    }
    panic!("turn driver never reached its goal");
}

/// Row 8 (U4 discriminator): Protean Hydra's own prevention removes three
/// +1/+1 counters in ONE removal event, and its singular trigger fires once
/// per counter removed (CR 603.2c; ruling: "If multiple +1/+1 counters are
/// removed from Protean Hydra at once, its last ability will trigger that many
/// times."). The damage is prevented (CR 615.1), so none is marked. Each
/// firing puts two +1/+1 counters on it at the next end step: 5 - 3 + 6 = 8.
/// Sibling: with only 2 counters, Bolt's 3 removes both (ruling: "all the
/// +1/+1 counters on Protean Hydra are removed from it"), firing twice, and
/// the damage is still fully prevented.
#[test]
fn singular_removal_trigger_fires_once_per_counter_removed_by_damage_prevention() {
    let (mut runner, hydra, mut events) = bolt_protean_hydra(5);
    // Reach guards: the prevention replacement removed counters, no damage.
    assert!(
        counter_removed_from(&events, hydra),
        "reach guard: the prevention removed +1/+1 counters from the Hydra"
    );
    assert_eq!(plus_one_counters(&runner, hydra), 2, "reach guard: 5 - 3");
    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        0,
        "reach guard: the damage was prevented"
    );
    // Discriminator: one trigger per counter removed.
    assert_eq!(
        triggers_from(&runner, hydra).len(),
        3,
        "three +1/+1 counters removed at once trigger the Hydra three times"
    );

    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert!(runner.state().stack.is_empty());
    drive_turn_until(&mut runner, &mut events, |r| {
        r.state().phase == Phase::End
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
            && r.state().stack.is_empty()
            && r.state().delayed_triggers.is_empty()
    });
    assert_eq!(
        plus_one_counters(&runner, hydra),
        8,
        "three delayed triggers each put two +1/+1 counters on it: 2 + 6"
    );

    // Sibling: fewer counters than damage removes them all.
    let (runner, hydra, events) = bolt_protean_hydra(2);
    assert!(counter_removed_from(&events, hydra));
    assert_eq!(
        plus_one_counters(&runner, hydra),
        0,
        "all the +1/+1 counters are removed"
    );
    assert_eq!(
        runner.state().objects[&hydra].damage_marked,
        0,
        "the damage is still fully prevented"
    );
    assert_eq!(
        triggers_from(&runner, hydra).len(),
        2,
        "two +1/+1 counters removed trigger the Hydra twice"
    );
}

/// Row 9 (U7 discriminator): CR 704.5q annihilates 2 +1/+1 and 2 -1/-1
/// counters on Protean Hydra (3 and 2 before). The removal goes through the
/// counter-removal authority, so the Hydra's typed +1/+1 trigger fires once per
/// +1/+1 counter removed (2); the -1/-1 removals do not match it. The
/// `with_counter` setup is the CR 704.5q precondition, not a shortcut around
/// the producer under test (the SBA).
#[test]
fn annihilation_removing_two_plus_one_counters_triggers_the_hydra_twice() {
    let mut scenario = main_phase();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Protean Hydra", 0, 1, PROTEAN_HYDRA)
        .id();
    scenario.with_counter(hydra, CounterType::Plus1Plus1, 3);
    scenario.with_counter(hydra, CounterType::Minus1Minus1, 2);
    let mut runner = scenario.build();

    // The SBA sweep runs before P1 receives priority.
    let mut events = Vec::new();
    act(&mut runner, GameAction::PassPriority, &mut events);
    drain_order_triggers_with_identity(runner.state_mut());

    // Reach guards: CR 704.5q ran.
    assert_eq!(plus_one_counters(&runner, hydra), 1, "reach guard: 3 - 2");
    assert!(
        !runner.state().objects[&hydra]
            .counters
            .contains_key(&CounterType::Minus1Minus1),
        "reach guard: every -1/-1 counter was annihilated"
    );
    assert_eq!(
        triggers_from(&runner, hydra).len(),
        2,
        "two +1/+1 counters annihilated trigger the Hydra twice"
    );
}

/// Row 10 (U7): CR 704.5q on a permanent P0 controls removes one +1/+1 and one
/// -1/-1 counter: two removal events on one recipient in one batch, so
/// Cloudsculpt Armorer's batched "one or more counters" trigger fires once,
/// carrying both counters (`Some(2)`).
#[test]
fn annihilation_on_a_permanent_you_control_triggers_cloudsculpt_armorer_once() {
    let mut scenario = main_phase();
    let armorer = scenario
        .add_creature_from_oracle(P0, "Cloudsculpt Armorer", 2, 2, CLOUDSCULPT_ARMORER)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.with_counter(bear, CounterType::Plus1Plus1, 1);
    scenario.with_counter(bear, CounterType::Minus1Minus1, 1);
    let mut runner = scenario.build();

    // The SBA sweep runs before P1 receives priority.
    let mut events = Vec::new();
    act(&mut runner, GameAction::PassPriority, &mut events);
    drain_order_triggers_with_identity(runner.state_mut());

    // Reach guard: both counters were annihilated.
    assert!(
        runner.state().objects[&bear].counters.is_empty(),
        "reach guard: CR 704.5q removed both counters: {:?}",
        runner.state().objects[&bear].counters
    );
    assert_eq!(
        triggers_from(&runner, armorer),
        vec![Some(2)],
        "one Armorer trigger for the one recipient, carrying both counters"
    );
}

/// Drive combat with P1 attacking until the stack is non-empty or a trigger
/// target prompt surfaces — stopping BEFORE any trigger resolves.
fn run_combat_until_stack_or_prompt(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
    events: &mut Vec<GameEvent>,
) {
    let mut attacked = false;
    for _ in 0..200 {
        if is_trigger_target_prompt(runner) || !runner.state().stack.is_empty() {
            return;
        }
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => act(runner, GameAction::PassPriority, events),
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                let declared = if attacked {
                    Vec::new()
                } else {
                    attacks.to_vec()
                };
                attacked = true;
                let result = runner
                    .declare_attackers(&declared)
                    .expect("attack declaration must be accepted");
                events.extend(result.events);
            }
            WaitingFor::DeclareBlockers { .. } => {
                let result = runner
                    .declare_blockers(&[])
                    .expect("no-block declaration must be accepted");
                events.extend(result.events);
            }
            other => panic!("combat driver stopped at an unexpected prompt: {other:?}"),
        }
    }
}

/// Row 7: a non-self subject ("a permanent you control", Cloudsculpt Armorer)
/// fires once per recipient, each firing carrying the counters removed from
/// that recipient. Paired negative: an opponent's planeswalker losing counters
/// does not trigger it.
#[test]
fn filtered_subject_fires_once_per_recipient_with_its_removed_count() {
    let mut scenario = main_phase();
    let armorer = scenario
        .add_creature_from_oracle(P0, "Cloudsculpt Armorer", 2, 2, CLOUDSCULPT_ARMORER)
        .id();
    let jace = scenario
        .add_planeswalker_from_oracle(P0, "Jace Beleren", "Jace", 4, JACE_BELEREN)
        .id();
    let liliana = scenario
        .add_planeswalker_from_oracle(P0, "Liliana of the Veil", "Liliana", 4, LILIANA_OF_THE_VEIL)
        .id();
    let small = scenario.add_creature(P1, "Two Power Attacker", 2, 2).id();
    let large = scenario.add_creature(P1, "Three Power Attacker", 3, 3).id();
    let mut runner = scenario.build();

    // Parsed-trigger reach guard: a typed, batched, untyped-counter trigger.
    let trigger = runner.state().objects[&armorer]
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| entry.definition.clone())
        .find(|t| t.mode == TriggerMode::CounterRemoved)
        .expect("Cloudsculpt Armorer's removal line is a CounterRemoved trigger");
    assert!(trigger.batched);
    assert_eq!(
        trigger.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::You)
        ))
    );
    assert_eq!(trigger.counter_filter, None);

    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    let mut events = Vec::new();
    run_combat_until_stack_or_prompt(
        &mut runner,
        &[
            (small, AttackTarget::Planeswalker(jace)),
            (large, AttackTarget::Planeswalker(liliana)),
        ],
        &mut events,
    );
    assert_eq!(loyalty(&runner, jace), 2, "reach guard: Jace 4 -> 2");
    assert_eq!(loyalty(&runner, liliana), 1, "reach guard: Liliana 4 -> 1");
    let mut counts = triggers_from(&runner, armorer);
    counts.sort();
    assert_eq!(
        counts,
        vec![Some(2), Some(3)],
        "one trigger per planeswalker, each carrying its removed count"
    );

    // Paired negative: an opponent's planeswalker loses counters.
    let mut scenario = main_phase();
    let armorer = scenario
        .add_creature_from_oracle(P0, "Cloudsculpt Armorer", 2, 2, CLOUDSCULPT_ARMORER)
        .id();
    let opposing = scenario
        .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 4, JACE_BELEREN)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT)
        .id();
    let mut runner = scenario.build();
    runner.cast(bolt).target_object(opposing).commit();
    let mut events = Vec::new();
    pass_until(&mut runner, &mut events, |r| {
        !r.state().stack.iter().any(|entry| entry.source_id == bolt)
    });
    assert!(counter_removed_from(&events, opposing));
    assert_eq!(loyalty(&runner, opposing), 1, "reach guard: 4 - 3");
    assert!(
        triggers_from(&runner, armorer).is_empty(),
        "an opponent's permanent is not a permanent you control"
    );
}
