//! Issue #321 regression: Betor, Ancestor's Voice end-step trigger applied
//! double the +1/+1 counters.
//!
//! Oracle: "Flying, lifelink. At the beginning of your end step, put a number
//! of +1/+1 counters on up to one other target creature you control equal to
//! the amount of life you gained this turn. Return up to one target creature
//! card with mana value less than or equal to the amount of life you lost this
//! turn from your graveyard to the battlefield."
//!
//! Root cause: the end-step trigger is a two-effect chain — `PutCounter`
//! (slot 0) followed by a `ChangeZone` sub-ability (slot 1), both `up to one`.
//! `assign_selected_slots_recursive` computed how many target slots the
//! `PutCounter` node should consume as `total_slots - sub_chain_minimum`.
//! Because the `ChangeZone` sub-ability is itself `up to one` (minimum 0), the
//! `PutCounter` node greedily claimed BOTH chosen targets. `resolve_add` then
//! iterated `ability.targets` and applied the counters once per entry —
//! doubling the counters whenever both trigger slots were filled.
//!
//! The fix caps each multi-target node at its own resolved `multi_target` max
//! (mirroring the per-node slot count `collect_target_slots` produces), so each
//! effect resolves against exactly its own chosen targets (CR 601.2c).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 120.3f: "Damage dealt by a source with lifelink causes that source's
//!     controller to gain that much life, in addition to the damage's other
//!     results." Lifelink is NOT a separate triggered ability — the life gain
//!     is simultaneous with the damage, incrementing `life_gained_this_turn`
//!     exactly once.
//!   - CR 513.1a: "At the beginning of [your] end step" triggers fire when the
//!     end step begins.
//!   - CR 601.2c: each effect in a chained ability is assigned its own targets.

use super::rules::{run_combat, GameScenario, Phase, WaitingFor, Zone, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;

const BETOR_ORACLE: &str = "Flying, lifelink\nAt the beginning of your end step, put a number of +1/+1 counters on up to one other target creature you control equal to the amount of life you gained this turn. Return up to one target creature card with mana value less than or equal to the amount of life you lost this turn from your graveyard to the battlefield.";

/// Drive the engine from the post-combat priority window to the end-step
/// trigger's target-selection window. Returns once the trigger is asking for
/// its first target.
fn advance_to_end_step_trigger(runner: &mut super::rules::GameRunner) {
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                return;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!("unexpected waiting state before end-step trigger: {other:?}"),
        }
    }
    panic!("phase machine did not reach the end-step trigger");
}

/// Count `+1/+1` counters on an object.
fn p1p1_counters(runner: &super::rules::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object still present")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Bare repro with the second (graveyard-return) slot declined: Betor (3/3
/// flying lifelink) attacks unblocked for 3, gaining 3 life. At end step the
/// trigger puts counters on the chosen receiver. With no Doubling Season /
/// Hardened Scales anywhere, the receiver must get exactly 3 counters.
#[test]
fn betor_end_step_counters_equal_lifelink_gain_no_doubler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Betor built from real Oracle text → trigger AST produced by the parser.
    let betor = scenario
        .add_creature_from_oracle(P0, "Betor, Ancestor's Voice", 3, 3, BETOR_ORACLE)
        .id();
    let receiver = scenario.add_creature(P0, "Receiver", 1, 1).id();
    scenario.add_creature(P1, "Blocker", 3, 3);

    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    // Betor attacks P1 unblocked → 3 combat damage → 3 lifelink life.
    run_combat(&mut runner, vec![betor], vec![]);

    assert_eq!(
        runner.life(P0),
        life_before + 3,
        "Betor lifelink must gain exactly 3 life from 3 combat damage (CR 120.3f)"
    );
    assert_eq!(
        runner.state().players[0].life_gained_this_turn,
        3,
        "life_gained_this_turn must be exactly 3 — one lifelink combat-damage \
         event increments it once (CR 120.3f)"
    );

    advance_to_end_step_trigger(&mut runner);

    // Slot 0: PutCounter receiver. Slot 1: graveyard return — decline (no
    // creature cards in the graveyard, the realistic case).
    let mut guard = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    ) {
        guard += 1;
        assert!(guard < 10, "target selection did not terminate");

        let target = match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { selection, .. }
            | WaitingFor::TargetSelection { selection, .. } => {
                if selection.current_slot == 0 {
                    Some(TargetRef::Object(receiver))
                } else {
                    None
                }
            }
            _ => None,
        };
        runner
            .act(GameAction::ChooseTarget { target })
            .expect("ChooseTarget should succeed");
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1_counters(&runner, receiver),
        3,
        "Betor end-step trigger must put exactly 3 +1/+1 counters on the \
         receiver (= 3 life gained via lifelink), not double"
    );
}

/// Issue #321 core regression: when BOTH end-step trigger slots are filled,
/// the slot allocator must give each chained effect its own target slot.
///
/// The end-step trigger is a two-effect chain — `PutCounter` (slot 0) then a
/// `ChangeZone` graveyard-return sub-ability (slot 1), both "up to one". The
/// pre-fix allocator set `current_slots = total_slots - sub_chain_minimum`
/// (`2 - 0 = 2`), so the `PutCounter` node greedily claimed BOTH chosen
/// targets and the `ChangeZone` node was starved of its own. With the fix
/// each node is capped at its own resolved `multi_target` max (CR 601.2c).
///
/// Reproduction with the (correct) zone-scoped parse: the `ChangeZone` filter
/// is `InZone: Graveyard` with a `mana value <= life lost this turn` cap, so
/// slot 1 targets a creature *card in the graveyard* (CR 404). P0 loses no
/// life here, so the cap is 0 and a mana-value-0 creature card qualifies.
///
/// The load-bearing assertion is that the slot-1 graveyard creature actually
/// returns to the battlefield: that proves the `ChangeZone` node received its
/// own target rather than having both targets swallowed by `PutCounter`. On
/// the pre-fix allocator `ChangeZone` was starved, so the creature never left
/// the graveyard.
#[test]
fn betor_put_counter_does_not_leak_into_second_trigger_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let betor = scenario
        .add_creature_from_oracle(P0, "Betor, Ancestor's Voice", 3, 3, BETOR_ORACLE)
        .id();
    let receiver = scenario.add_creature(P0, "Receiver", 1, 1).id();
    // Betor's `ChangeZone` sub-ability targets a creature card in the
    // graveyard with mana value <= life lost this turn. P0 loses no life in
    // this scenario, so the cap is 0 — a mana-value-0 creature card (the
    // builder default) qualifies.
    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Creature", 2, 2)
        .id();
    scenario.add_creature(P1, "Blocker", 3, 3);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![betor], vec![]);
    assert_eq!(runner.state().players[0].life_gained_this_turn, 3);

    advance_to_end_step_trigger(&mut runner);

    // Slot 0 → the battlefield receiver, slot 1 → the graveyard creature.
    // Both choices MUST be `Some` for this test to exercise the bug; if a
    // slot does not offer the intended target the test fails loudly.
    let mut slot_choices: Vec<(usize, Option<TargetRef>)> = Vec::new();
    let mut guard = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    ) {
        guard += 1;
        assert!(guard < 10, "target selection did not terminate");

        let (slot_index, target) = match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot_index = selection.current_slot;
                let want = if slot_index == 0 {
                    receiver
                } else {
                    gy_creature
                };
                let legal = &target_slots
                    .get(slot_index)
                    .expect("current slot must exist")
                    .legal_targets;
                // The intended object MUST be a legal target for this slot —
                // if it is not, fail rather than silently declining.
                let target = legal
                    .iter()
                    .find(|t| matches!(t, TargetRef::Object(id) if *id == want))
                    .cloned();
                assert!(
                    target.is_some(),
                    "slot {slot_index} must offer the intended target {want:?}; \
                     legal targets were {legal:?}"
                );
                (slot_index, target)
            }
            _ => unreachable!(),
        };
        slot_choices.push((slot_index, target.clone()));
        runner
            .act(GameAction::ChooseTarget { target })
            .expect("ChooseTarget should succeed");
    }

    // Both trigger slots must have been offered and filled with a `Some`
    // target — this is the precondition that makes the test reproduce #321.
    assert_eq!(
        slot_choices.len(),
        2,
        "the end-step trigger must surface exactly two target slots"
    );
    assert_eq!(
        slot_choices[0],
        (0, Some(TargetRef::Object(receiver))),
        "slot 0 must be filled with the receiver"
    );
    assert_eq!(
        slot_choices[1],
        (1, Some(TargetRef::Object(gy_creature))),
        "slot 1 must be filled with the graveyard creature — without a filled \
         slot 1 the bug does not reproduce"
    );

    runner.advance_until_stack_empty();

    // Slot 0: the receiver gets exactly 3 +1/+1 counters (= life gained).
    assert_eq!(
        p1p1_counters(&runner, receiver),
        3,
        "slot-0 PutCounter target must get exactly 3 +1/+1 counters"
    );

    // Slot 1: the `ChangeZone` node must have received its own target — so the
    // graveyard creature is now on the battlefield. The zone move makes it a
    // new object (CR 400.7), so it is located by name. On the pre-fix
    // allocator `PutCounter` swallowed BOTH targets, starving `ChangeZone`, so
    // the creature never left the graveyard.
    let returned = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Graveyard Creature" && o.zone == Zone::Battlefield);
    assert!(
        returned.is_some(),
        "slot-1 ChangeZone must return the graveyard creature to the \
         battlefield — the slot allocator must give the ChangeZone node its \
         own target rather than letting PutCounter claim both (CR 601.2c)"
    );
    // And no +1/+1 counters leaked onto the returned creature.
    assert_eq!(
        returned
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "the returned creature must carry no +1/+1 counters — the slot-0 \
         PutCounter effect must resolve against only its own target (CR 601.2c)"
    );
}
