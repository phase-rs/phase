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
//! trigger fires once per removal EVENT. The Protean Hydra ruling ("If multiple
//! +1/+1 counters are removed from Protean Hydra at once, its last ability will
//! trigger that many times.") wants one per counter; that per-counter
//! multiplicity is fenced and tracked in #10901 (no unbounded expansion), so
//! these rows pin the once-per-event behavior. CR 704.5q: the +1/+1 / -1/-1 annihilation
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
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::SubjectIncarnationScope;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::player::PlayerId;

use crate::support::shared_card_db;
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
/// per removal event of that kind (CR 603.2c; per-counter multiplicity is
/// #10901). A shield counter removed from Protean Hydra does not trigger it;
/// the +1/+1 removal event triggers it once.
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
        1,
        "the +1/+1 removal event triggers the Hydra once (the shield removal does not); per-counter is #10901"
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
/// for that event (CR 603.2c; the ruling wants one per counter, fenced as
/// #10901). The damage is prevented (CR 615.1), so none is marked. The firing
/// puts two +1/+1 counters on it at the next end step: 5 - 3 + 2 = 4.
/// Sibling: with only 2 counters, Bolt's 3 removes both (ruling: "all the
/// +1/+1 counters on Protean Hydra are removed from it"), firing once, and
/// the damage is still fully prevented.
#[test]
fn singular_removal_trigger_fires_once_per_removal_event_by_damage_prevention() {
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
    // Discriminator: one trigger per removal event.
    assert_eq!(
        triggers_from(&runner, hydra).len(),
        1,
        "three +1/+1 counters removed at once trigger the Hydra once (per-counter is #10901)"
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
        4,
        "the one delayed trigger puts two +1/+1 counters on it: 2 + 2"
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
        1,
        "two +1/+1 counters removed in one event trigger the Hydra once"
    );
}

/// Row 9 (U7 discriminator): CR 704.5q annihilates 2 +1/+1 and 2 -1/-1
/// counters on Protean Hydra (3 and 2 before). The removal goes through the
/// counter-removal authority, so the Hydra's typed +1/+1 trigger fires once for
/// the +1/+1 removal event (per-counter is #10901); the -1/-1 removals do not
/// match it. The
/// `with_counter` setup is the CR 704.5q precondition, not a shortcut around
/// the producer under test (the SBA).
#[test]
fn annihilation_removing_plus_one_counters_triggers_the_hydra_once() {
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
        1,
        "the +1/+1 annihilation removal event triggers the Hydra once"
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

// ---------------------------------------------------------------------------
// CR 608.2c + CR 603.10: a later instruction of the same resolution moves the
// observer off the battlefield.
//
// Printed cards (Oracle text verbatim, data/card-data.json):
//   Chandra, Fire Artisan — "Whenever one or more loyalty counters are removed
//     from Chandra, she deals that much damage to target opponent or
//     planeswalker."
//   Fractured Identity — "Exile target nonland permanent. Each player other
//     than its controller creates a token that's a copy of it."
//   Aether Snap — "Remove all counters from all permanents and exile all
//     tokens."
//   Nadier's Nightblade — "Whenever a token you control leaves the battlefield,
//     each opponent loses 1 life and you gain 1 life."
//
// CR 608.2c: Aether Snap's instructions happen in the order written, so a
// Chandra token still on the battlefield loses its loyalty counters first and
// is exiled second. CR 603.10: objects that exist immediately after an event
// are checked for its triggers — counter removal is not a CR 603.10a look-back
// event — and the token existed right after the removal, so it triggers.
// CR 122.2: the exile itself removes nothing (counters simply cease to exist).
// ---------------------------------------------------------------------------

fn add_mana(runner: &mut GameRunner, player: PlayerId, colored: &[ManaType], generic: usize) {
    let units = colored
        .iter()
        .copied()
        .chain(std::iter::repeat_n(ManaType::Colorless, generic));
    for color in units {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

/// The token copy of `name` that `controller` controls on the battlefield.
fn token_named(runner: &GameRunner, controller: PlayerId, name: &str) -> ObjectId {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let object = &state.objects[id];
            object.is_token && object.controller == controller && object.name == name
        })
        .unwrap_or_else(|| panic!("{controller:?} controls no token copy of {name}"))
}

/// Resolve Fractured Identity on `target`; returns P0's token copy of it.
fn fracture(runner: &mut GameRunner, identity: ObjectId, target: ObjectId) -> ObjectId {
    let name = runner.state().objects[&target].name.clone();
    add_mana(runner, P0, &[ManaType::White, ManaType::Blue], 3);
    runner.cast(identity).target_object(target).resolve();
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Exile,
        "reach: Fractured Identity exiled its target"
    );
    token_named(runner, P0, &name)
}

/// Cast Aether Snap and pass priority until it has left the stack, stopping at
/// any trigger target prompt its resolution raised. Returns every event.
fn snap(runner: &mut GameRunner, aether_snap: ObjectId) -> Vec<GameEvent> {
    add_mana(runner, P0, &[ManaType::Black, ManaType::Black], 3);
    let mut events = runner.cast(aether_snap).commit().events().to_vec();
    pass_until(runner, &mut events, |r| {
        is_trigger_target_prompt(r)
            || !r
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == aether_snap)
    });
    events
}

fn removed_loyalty(events: &[GameEvent], id: ObjectId) -> u32 {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CounterRemoved {
                object_id,
                counter_type: CounterType::Loyalty,
                count,
                ..
            } if *object_id == id => Some(*count),
            _ => None,
        })
        .sum()
}

fn left_battlefield_to_exile(events: &[GameEvent], id: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Exile,
                ..
            } if *object_id == id
        )
    })
}

/// CR 603.10 + CR 400.7: every source context the stacked triggers of
/// `source` carry is `Live` — the departure-window scope is matching-local.
fn stacked_trigger_sources_are_live(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.iter().all(|entry| match &entry.kind {
        StackEntryKind::TriggeredAbility {
            source_id, ability, ..
        } if *source_id == source => ability
            .trigger_source
            .as_ref()
            .is_some_and(|context| context.subject_scope == SubjectIncarnationScope::Live),
        _ => true,
    })
}

struct SnapBoard {
    runner: GameRunner,
    chandra: ObjectId,
    identity: ObjectId,
    aether_snap: ObjectId,
}

/// P1 controls the printed Chandra (4 loyalty); P0 holds Fractured Identity
/// and Aether Snap. `None` only when no card database is available.
fn snap_board(extra_p0: &[&str]) -> Option<(SnapBoard, Vec<ObjectId>)> {
    let db = shared_card_db()?;
    let mut scenario = main_phase();
    let chandra = scenario.add_real_card(P1, "Chandra, Fire Artisan", Zone::Battlefield, db);
    let identity = scenario.add_real_card(P0, "Fractured Identity", Zone::Hand, db);
    let aether_snap = scenario.add_real_card(P0, "Aether Snap", Zone::Hand, db);
    let extras = extra_p0
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
    let mut runner = scenario.build();
    if loyalty(&runner, chandra) == 0 {
        runner
            .state_mut()
            .objects
            .get_mut(&chandra)
            .unwrap()
            .counters
            .insert(CounterType::Loyalty, 4);
    }
    assert_eq!(loyalty(&runner, chandra), 4, "reach: printed loyalty 4");
    Some((
        SnapBoard {
            runner,
            chandra,
            identity,
            aether_snap,
        },
        extras,
    ))
}

/// Maintainer's case: P0's Chandra token (from Fractured Identity) loses its
/// four loyalty counters to Aether Snap's first instruction and is exiled by
/// its second. The token triggers on the removal and deals 4 to P1.
#[test]
fn chandra_token_removed_then_exiled_by_one_resolution_deals_that_much() {
    let Some((board, _)) = snap_board(&[]) else {
        return;
    };
    let SnapBoard {
        mut runner,
        chandra,
        identity,
        aether_snap,
    } = board;
    let token = fracture(&mut runner, identity, chandra);
    assert_eq!(loyalty(&runner, token), 4, "reach: the copy entered with 4");

    let mut events = snap(&mut runner, aether_snap);
    assert_eq!(
        removed_loyalty(&events, token),
        4,
        "instruction 1 removed the token's four loyalty counters"
    );
    assert!(
        left_battlefield_to_exile(&events, token),
        "instruction 2 exiled the token"
    );
    assert!(
        !runner.state().battlefield.contains(&token),
        "the token is gone from the battlefield"
    );

    if is_trigger_target_prompt(&runner) {
        choose_trigger_target(&mut runner, TargetRef::Player(P1), &mut events);
    }
    assert_eq!(
        triggers_from(&runner, token).len(),
        1,
        "the departed token's removal trigger is on the stack exactly once"
    );
    assert!(
        stacked_trigger_sources_are_live(&runner, token),
        "the stacked trigger carries a Live source context"
    );

    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.life(P1),
        16,
        "she deals that much (4) damage to the opponent"
    );
}

/// Positive control on the live path: a nontoken Chandra is not exiled by
/// Aether Snap (only tokens are) and triggers exactly once for the four
/// counters removed, from the live battlefield scan; she then dies to the
/// zero-loyalty state-based action and the trigger resolves with her last
/// known information.
#[test]
fn live_chandra_loses_counters_to_aether_snap_and_triggers_once() {
    let Some((board, _)) = snap_board(&[]) else {
        return;
    };
    let SnapBoard {
        mut runner,
        chandra,
        aether_snap,
        ..
    } = board;
    let mut events = snap(&mut runner, aether_snap);
    assert_eq!(removed_loyalty(&events, chandra), 4);
    assert!(
        !left_battlefield_to_exile(&events, chandra),
        "a nontoken is not exiled"
    );
    if is_trigger_target_prompt(&runner) {
        choose_trigger_target(&mut runner, TargetRef::Player(P0), &mut events);
    }
    assert_eq!(triggers_from(&runner, chandra).len(), 1);
    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert_eq!(runner.life(P0), 16, "P1's Chandra deals 4 to P0");
}

/// The token's exile also triggers Nadier's Nightblade, which collects (and
/// so claims) the exile record before the post-action scan. The removal
/// trigger must still fire: the departure is read from the action's raw
/// events, not from the claimed-filtered slice. P1 takes 4 + 1.
#[test]
fn chandra_token_trigger_survives_a_claimed_exile_record() {
    let Some((board, extras)) = snap_board(&["Nadier's Nightblade"]) else {
        return;
    };
    let SnapBoard {
        mut runner,
        chandra,
        identity,
        aether_snap,
    } = board;
    let nightblade = extras[0];
    let token = fracture(&mut runner, identity, chandra);

    let mut events = snap(&mut runner, aether_snap);
    assert_eq!(removed_loyalty(&events, token), 4);
    assert!(left_battlefield_to_exile(&events, token));
    for _ in 0..8 {
        if is_trigger_target_prompt(&runner) {
            choose_trigger_target(&mut runner, TargetRef::Player(P1), &mut events);
        } else if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
        } else {
            break;
        }
    }
    assert_eq!(
        triggers_from(&runner, nightblade).len(),
        1,
        "reach: Nightblade saw the token leave"
    );
    assert_eq!(
        triggers_from(&runner, token).len(),
        1,
        "the token's removal trigger fired once"
    );
    pass_until(&mut runner, &mut events, |r| r.state().stack.is_empty());
    assert_eq!(runner.life(P1), 15, "4 from Chandra and 1 from Nightblade");
    assert_eq!(runner.life(P0), 21, "Nightblade's 1 life");
}

// ---------------------------------------------------------------------------
// Cloudsculpt Armorer and Austere Command: "Whenever one or more counters are
// removed from a permanent you control, seek a nonland card."
//
// CR 603.2c: an ability triggers once each time its trigger event occurs. The
// two destroy-all instructions of Austere Command are two events, so shield
// counters removed by different instructions are different occurrences, while
// counters one instruction removes together are one. CR 608.2c: the
// instructions happen in the order written. CR 603.10: an ability checks the
// objects that exist immediately after the event, so an Armorer that the
// second instruction destroys did not exist after the second instruction's
// removal and does not see it (counter removal is not a CR 603.10a look-back
// event). CR 122.1c: a shield counter is removed instead of destroying.
// ---------------------------------------------------------------------------

const AUSTERE_COMMAND: &str = "Choose two —\n• Destroy all artifacts.\n• Destroy all enchantments.\n• Destroy all creatures with mana value 3 or less.\n• Destroy all creatures with mana value 4 or greater.";
const MODE_DESTROY_ARTIFACTS: usize = 0;
const MODE_DESTROY_ENCHANTMENTS: usize = 1;
const MODE_DESTROY_MV_THREE_OR_LESS: usize = 2;
const MODE_DESTROY_MV_FOUR_OR_MORE: usize = 3;

/// P0 casts Austere Command choosing `modes`; returns the number of Cloudsculpt
/// Armorer triggers that reach the stack.
fn armorer_triggers_after_austere_command(
    runner: &mut GameRunner,
    command: ObjectId,
    armorer: ObjectId,
    modes: &[usize],
) -> usize {
    add_mana(runner, P0, &[ManaType::White, ManaType::White], 4);
    let mut events = runner.cast(command).modes(modes).commit().events().to_vec();
    pass_until(runner, &mut events, |r| {
        !r.state()
            .stack
            .iter()
            .any(|entry| entry.source_id == command)
    });
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    triggers_from(runner, armorer).len()
}

fn shield_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Shield)
        .copied()
        .unwrap_or(0)
}

/// F1: the Armorer is destroyed by the second instruction (mana value four or
/// greater), in the same event that removes Air Elemental's shield. It saw the
/// first instruction's removal (Ornithopter's shield) and not the second's, so
/// it triggers once.
#[test]
fn armorer_destroyed_by_the_second_instruction_does_not_see_its_removals() {
    let mut scenario = main_phase();
    let ornithopter = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let air_elemental = scenario
        .add_creature(P0, "Air Elemental", 4, 4)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 5,
        })
        .id();
    let armorer = scenario
        .add_creature_from_oracle(P0, "Cloudsculpt Armorer", 2, 2, CLOUDSCULPT_ARMORER)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Austere Command", false, AUSTERE_COMMAND)
        .id();
    scenario.with_counter(ornithopter, CounterType::Shield, 1);
    scenario.with_counter(air_elemental, CounterType::Shield, 1);
    let mut runner = scenario.build();

    let triggers = armorer_triggers_after_austere_command(
        &mut runner,
        command,
        armorer,
        &[MODE_DESTROY_ARTIFACTS, MODE_DESTROY_MV_FOUR_OR_MORE],
    );

    // Reach: both shields were removed, the Armorer died, the others lived.
    assert_eq!(shield_counters(&runner, ornithopter), 0);
    assert_eq!(shield_counters(&runner, air_elemental), 0);
    assert!(!runner.state().battlefield.contains(&armorer));
    assert!(runner.state().battlefield.contains(&ornithopter));
    assert!(runner.state().battlefield.contains(&air_elemental));
    assert_eq!(
        triggers, 1,
        "the Armorer saw the first removal only, not the one that destroyed it"
    );
}

/// F2: two instructions each remove one shield counter from the same permanent
/// and the Armorer survives both — two removal events, two triggers.
#[test]
fn sequential_removals_from_one_permanent_trigger_the_armorer_each_time() {
    let mut scenario = main_phase();
    let ornithopter = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let armorer = scenario
        .add_creature_from_oracle(P0, "Cloudsculpt Armorer", 2, 2, CLOUDSCULPT_ARMORER)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Austere Command", false, AUSTERE_COMMAND)
        .id();
    scenario.with_counter(ornithopter, CounterType::Shield, 2);
    let mut runner = scenario.build();

    let triggers = armorer_triggers_after_austere_command(
        &mut runner,
        command,
        armorer,
        &[MODE_DESTROY_ARTIFACTS, MODE_DESTROY_MV_THREE_OR_LESS],
    );

    assert_eq!(
        shield_counters(&runner, ornithopter),
        0,
        "reach: each instruction removed one shield counter"
    );
    assert!(runner.state().battlefield.contains(&ornithopter));
    assert!(runner.state().battlefield.contains(&armorer));
    assert_eq!(
        triggers, 2,
        "two instructions are two removal events, so two triggers"
    );
}

/// Control for the aggregation F2 must keep: one instruction that removes
/// shield counters from two permanents is one simultaneous event with two
/// recipients, so the Armorer triggers once per recipient permanent (the
/// batched "one or more counters … from a permanent" occurrence is per
/// recipient, CR 603.2c). The second chosen mode (enchantments) removes nothing.
#[test]
fn one_instruction_removing_shields_from_two_permanents_triggers_per_recipient() {
    let mut scenario = main_phase();
    let first = scenario
        .add_creature(P0, "First Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let second = scenario
        .add_creature(P0, "Second Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let armorer = scenario
        .add_creature_from_oracle(P0, "Cloudsculpt Armorer", 2, 2, CLOUDSCULPT_ARMORER)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Austere Command", false, AUSTERE_COMMAND)
        .id();
    scenario.with_counter(first, CounterType::Shield, 1);
    scenario.with_counter(second, CounterType::Shield, 1);
    let mut runner = scenario.build();

    let triggers = armorer_triggers_after_austere_command(
        &mut runner,
        command,
        armorer,
        &[MODE_DESTROY_ARTIFACTS, MODE_DESTROY_ENCHANTMENTS],
    );

    assert_eq!(shield_counters(&runner, first), 0);
    assert_eq!(shield_counters(&runner, second), 0);
    assert_eq!(
        triggers, 2,
        "one instruction's simultaneous removals fire once per recipient permanent"
    );
}
