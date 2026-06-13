//! CR 601.2c + CR 115.3 — object-target distinctness WITHIN one instance of the
//! word "target".
//!
//! The Wise Mothman's milled trigger ("…put a +1/+1 counter on each of up to X
//! target creatures…") is a single `multi_target` instance of "target": its
//! slots must be mutually distinct objects (CR 601.2c). A controller must not be
//! able to drop the same creature into two of those slots.
//!
//! These tests drive the real trigger through the `apply` pipeline (a genuine
//! Tome Scour mill fires the trigger), then exercise BOTH enforcement gates:
//! the offered set surfaced on the live `TriggerTargetSelection` prompt
//! (`ChooseTarget` path) and the whole-selection validate path
//! (`SelectTargets`). The cross-instance reuse guard (the binding CR 601.2c
//! "Destroy target artifact and target land" Example) lives as a spec-layer unit
//! test inside `game/ability_utils.rs`, where the two-distinct-instance
//! abilities can be constructed directly.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{ActionResult, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

use crate::support::shared_card_db as load_db;

/// Give P0 the mana to cast Tome Scour ({U}).
fn add_blue_mana(runner: &mut engine::game::scenario::GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::Blue, dummy, false, vec![]));
}

/// Cast P0's Tome Scour ("Target player mills five cards") aimed at
/// `mill_target`'s library and return the post-cast `ActionResult` (spell on the
/// stack). Mirrors `wise_mothman_milled_trigger::cast_tome_scour`.
fn cast_tome_scour(
    runner: &mut engine::game::scenario::GameRunner,
    tome_scour: ObjectId,
    mill_target: PlayerId,
) -> ActionResult {
    let card_id = runner.state().objects[&tome_scour].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: tome_scour,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Tome Scour cast should be accepted");

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(mill_target)),
            })
            .expect("Tome Scour should accept the chosen player target");
    }
    result
}

/// Build a Mothman scenario with distinct legal +1/+1-counter targets on P0's
/// battlefield, mill P0's own library through a real Tome Scour cast, and drive
/// the stack until the milled trigger surfaces its `TriggerTargetSelection`
/// prompt. Returns the runner and the ids of the creatures (in creation order).
fn mothman_until_target_selection(
    db: &'static CardDatabase,
    creature_names: &[&str],
) -> (engine::game::scenario::GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "The Wise Mothman", Zone::Battlefield, db);

    // Distinct legal counter targets.
    let creatures: Vec<ObjectId> = creature_names
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();

    let tome_scour = scenario.add_real_card(P0, "Tome Scour", Zone::Hand, db);

    // Five nonland cards to mill (Lightning Bolt is an instant).
    for _ in 0..9 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_blue_mana(&mut runner);

    let result = cast_tome_scour(&mut runner, tome_scour, P0);

    let mut result = result;
    let mut guard = 0;
    while !matches!(
        result.waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        guard += 1;
        assert!(
            guard < 64,
            "The Wise Mothman's milled trigger never surfaced a target prompt; \
             last waiting_for = {:?}",
            result.waiting_for
        );
        result = runner
            .act(GameAction::PassPriority)
            .expect("stack should advance toward the milled trigger");
    }

    (runner, creatures)
}

/// Read the live `current_legal_targets` off the trigger target-selection prompt.
fn current_offered_targets(runner: &engine::game::scenario::GameRunner) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { selection, .. } => {
            selection.current_legal_targets.clone()
        }
        other => panic!("expected TriggerTargetSelection, got {other:?}"),
    }
}

/// CR 601.2c + CR 115.3 (offered set): after choosing creature A in an early
/// slot of the Mothman's "up to X target creatures" instance, the NEXT slot's
/// offered set must NOT contain A. Choosing all-distinct creatures succeeds.
#[test]
fn mothman_offered_set_excludes_already_chosen_creature() {
    let Some(db) = load_db() else {
        return;
    };

    let creature_names = ["Grizzly Bears", "Llanowar Elves", "Centaur Courser"];
    let (mut runner, creatures) = mothman_until_target_selection(db, &creature_names);
    let [a, b, c] = [creatures[0], creatures[1], creatures[2]];

    // Slot 0 offers every distinct creature.
    let slot0 = current_offered_targets(&runner);
    for id in [a, b, c] {
        assert!(
            slot0.contains(&TargetRef::Object(id)),
            "slot 0 should offer every legal creature; got {slot0:?}"
        );
    }

    // Choose A for slot 0.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a)),
        })
        .expect("choosing A in slot 0 should be legal");

    // Slot 1 must exclude A but still offer B and C.
    let slot1 = current_offered_targets(&runner);
    assert!(
        !slot1.contains(&TargetRef::Object(a)),
        "CR 601.2c: A is already chosen in this instance — it must not be offered \
         again for the next slot; got {slot1:?}"
    );
    assert!(
        slot1.contains(&TargetRef::Object(b)) && slot1.contains(&TargetRef::Object(c)),
        "the other distinct creatures remain legal for slot 1; got {slot1:?}"
    );

    // Choosing B then C (all distinct) fills three of the up-to-5 slots.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("choosing B in slot 1 should be legal");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(c)),
        })
        .expect("choosing C in slot 2 should be legal");

    // The remaining slots are optional (min was 0) — skip them cleanly so the
    // selection completes and the trigger resolves.
    let mut guard = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        guard += 1;
        assert!(guard < 16, "optional Mothman slots failed to skip cleanly");
        runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("skipping a remaining optional Mothman slot should be legal");
    }
    runner.advance_until_stack_empty();
}

/// CR 601.2c + CR 115.3 (validate path): selecting the same creature twice in
/// one Mothman instance must be REJECTED (illegal target), not silently
/// accepted.
#[test]
fn mothman_validate_rejects_same_creature_twice() {
    let Some(db) = load_db() else {
        return;
    };

    let creature_names = ["Grizzly Bears", "Llanowar Elves"];
    let (mut runner, creatures) = mothman_until_target_selection(db, &creature_names);
    let a = creatures[0];

    // [A, A] in one instance is illegal — the apply pipeline must reject it.
    let rejected = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Object(a), TargetRef::Object(a)],
    });
    assert!(
        rejected.is_err(),
        "CR 601.2c: the same creature can't fill two slots of one multi_target \
         instance; SelectTargets([A, A]) must be rejected"
    );
}

/// CR 601.2c + CR 122.1 end-to-end: with K distinct legal creatures and X > K
/// nonland cards milled, choosing all K distinct creatures lands exactly one
/// +1/+1 counter on each — no duplicates — and the remaining optional slots skip
/// cleanly.
#[test]
fn mothman_distinct_targets_land_one_counter_each() {
    let Some(db) = load_db() else {
        return;
    };

    // K = 3 distinct creatures; X = 5 nonland cards milled (X > K).
    let creature_names = ["Grizzly Bears", "Llanowar Elves", "Centaur Courser"];
    let (mut runner, creatures) = mothman_until_target_selection(db, &creature_names);

    // Select all three distinct creatures in one instance; the remaining two
    // optional slots are skipped by the completed selection.
    runner
        .act(GameAction::SelectTargets {
            targets: creatures.iter().map(|id| TargetRef::Object(*id)).collect(),
        })
        .expect("choosing three distinct legal creatures must be accepted");
    runner.advance_until_stack_empty();

    for id in &creatures {
        let object = &runner.state().objects[id];
        let plus = object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            plus, 1,
            "each distinct chosen creature should receive exactly one +1/+1 \
             counter (object {id:?})"
        );
    }
}
