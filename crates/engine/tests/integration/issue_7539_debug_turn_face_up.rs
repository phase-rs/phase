//! Regression for GitHub issue #7539 — the sandbox `Turn Face Up` action must
//! RESTORE the stored face, not just clear the flag.
//!
//! CR 708.2a: a face-down permanent is a 2/2 creature with no name, no mana
//! cost, no creature types and no abilities. Its real characteristics live in
//! `back_face` until it is turned face up. CR 702.37e: the morph effect ends
//! and the permanent "regains its normal characteristics". Clearing `face_down`
//! alone
//! leaves the vanilla 2/2 installed, so the tool appears to do nothing.
//!
//! Same class as #3284 / #3290, where the debug `transformed` write was routed
//! through `transform::transform_permanent` by #3684. The `face_down` write in
//! the same match arm was never carried over.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::GameEvent;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::zones::Zone;

/// A creature card in hand with a real mana cost, so CR 701.40b can derive the
/// turn-face-up cost from the stored face.
fn board() -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature_to_hand(P0, "Hidden Bear", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let mut events = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), P0, id, &mut events)
        .expect("the card is played face down");

    let obj = &runner.state().objects[&id];
    assert!(obj.face_down, "setup: the permanent is face down");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, "", "CR 708.2a: a face-down permanent has no name");
    assert_eq!(obj.base_power, Some(2), "CR 708.2a: it is a 2/2");

    (runner, id)
}

/// The defect: the tool must produce the real card, and it must produce the
/// event the turn-face-up triggers observe.
#[test]
fn the_sandbox_turn_face_up_restores_the_stored_face() {
    let (mut runner, id) = board();

    let result = runner
        .act(GameAction::Debug(DebugAction::SetFaceState {
            object_id: id,
            face_down: Some(false),
            transformed: None,
            flipped: None,
        }))
        .expect("the debug turn-face-up runs");

    let obj = &runner.state().objects[&id];
    assert!(!obj.face_down);
    assert_eq!(obj.name, "Hidden Bear", "the stored face is restored");
    assert_eq!(
        (obj.base_power, obj.base_toughness),
        (Some(3), Some(3)),
        "with its printed power and toughness, not the CR 708.2a 2/2"
    );

    // The discriminating assertion. A flag-only write also leaves `face_down`
    // false, so the flag alone cannot tell the two implementations apart — the
    // restored characteristics and this event can. `TurnedFaceUp` is what the
    // "when this is turned face up" triggers and the
    // "as ~ is turned face up" replacement key on; without it the tool changes a
    // flag and the game never learns anything happened.
    assert!(
        result.events.iter().any(
            |event| matches!(event, GameEvent::TurnedFaceUp { object_id, .. } if *object_id == id)
        ),
        "the turn-face-up event must reach the triggers, got {:?}",
        result.events
    );
}

/// #7541, the other direction: turning a permanent face down must SNAPSHOT its
/// face, or the permanent keeps its name and printed P/T while claiming to be
/// face down — and `back_face` stays empty, so the repaired face-up path can
/// never bring it back. The round trip is the assertion.
#[test]
fn the_sandbox_turn_face_down_snapshots_the_real_face_and_the_round_trip_closes() {
    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature(P0, "Open Bear", 4, 4)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let write = |runner: &mut engine::game::scenario::GameRunner, down: bool| {
        runner
            .act(GameAction::Debug(DebugAction::SetFaceState {
                object_id: id,
                face_down: Some(down),
                transformed: None,
                flipped: None,
            }))
            .expect("the debug face-state write runs")
    };

    write(&mut runner, true);
    let obj = &runner.state().objects[&id];
    assert!(obj.face_down);
    assert_eq!(obj.name, "", "CR 708.2a: no name while face down");
    assert_eq!(
        (obj.base_power, obj.base_toughness),
        (Some(2), Some(2)),
        "CR 708.2a: a 2/2, not the printed 4/4"
    );
    assert!(
        obj.back_face.is_some(),
        "the real face is stashed, which is what makes the way back possible"
    );

    write(&mut runner, false);
    let obj = &runner.state().objects[&id];
    assert!(!obj.face_down);
    assert_eq!(obj.name, "Open Bear");
    assert_eq!((obj.base_power, obj.base_toughness), (Some(4), Some(4)));
}

/// CR 708.2b: "A face-down permanent can't be turned face down. If a spell or
/// ability attempts to turn a face-down permanent face down, nothing happens
/// and that effect doesn't change any of its characteristics or their copiable
/// values."
///
/// The stored face must survive a second face-down write, or the 2/2 would be
/// snapshotted over the real card and the permanent could never be restored.
///
/// What this row does NOT do: discriminate. It stays green with the face-down
/// arm removed, because the flag-only fallback is also harmless here. It pins
/// the guard so a future rewrite that drops `was_face_down` from the arm's
/// pattern turns it red.
#[test]
fn a_second_turn_face_down_leaves_the_stored_face_alone() {
    let (mut runner, id) = board();
    let stored = runner.state().objects[&id]
        .back_face
        .clone()
        .expect("setup: the real face is stashed");

    runner
        .act(GameAction::Debug(DebugAction::SetFaceState {
            object_id: id,
            face_down: Some(true),
            transformed: None,
            flipped: None,
        }))
        .expect("the debug face-state write runs");

    assert_eq!(
        runner.state().objects[&id].back_face,
        Some(stored),
        "CR 708.2b: nothing happens, so the stored face is untouched"
    );
}

/// Counter-direction: an object with no stored face keeps the plain flag write,
/// so the arm stays a debug tool for states the rules cannot reach.
#[test]
fn a_permanent_without_a_stored_face_keeps_the_plain_flag_write() {
    let mut scenario = GameScenario::new();
    let id = scenario.add_creature(P0, "Ordinary Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner.state_mut().objects.get_mut(&id).unwrap().face_down = true;

    runner
        .act(GameAction::Debug(DebugAction::SetFaceState {
            object_id: id,
            face_down: Some(false),
            transformed: None,
            flipped: None,
        }))
        .expect("the debug write runs");

    let obj = &runner.state().objects[&id];
    assert!(!obj.face_down);
    assert_eq!(obj.name, "Ordinary Bear");
}

// ── Review round 2: the direct-turn authority, not the entry profile ─────────

/// CR 708.2a + CR 613: the snapshot must come from the BASE face. The
/// battlefield-entry profile snapshots the LIVE face, so a permanent carrying a
/// continuous modification (here: a +1/+1 counter, live 5/5 on a printed 4/4)
/// came back from the round trip with the modification baked into its base —
/// and the still-present counter then inflated it AGAIN.
#[test]
fn a_modified_permanent_round_trips_to_its_base_face() {
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature(P0, "Open Bear", 4, 4)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    let down = runner
        .act(GameAction::Debug(DebugAction::SetFaceState {
            object_id: id,
            face_down: Some(true),
            transformed: None,
            flipped: None,
        }))
        .expect("the debug face-down write runs");
    assert!(
        down.events.iter().any(
            |event| matches!(event, GameEvent::TurnedFaceDown { object_id } if *object_id == id)
        ),
        "CR 603.2: the direct turn emits the event the turned-face-down triggers \
         observe (its own game action, distinct from transforming — CR 701.27b)"
    );
    assert_eq!(
        runner.state().objects[&id]
            .back_face
            .as_ref()
            .map(|face| face.power),
        Some(Some(4)),
        "the stash holds the PRINTED 4/4, not the counter-inflated live 5/5"
    );

    runner
        .act(GameAction::Debug(DebugAction::SetFaceState {
            object_id: id,
            face_down: Some(false),
            transformed: None,
            flipped: None,
        }))
        .expect("the debug face-up write runs");
    let obj = &runner.state().objects[&id];
    assert_eq!(
        (obj.base_power, obj.base_toughness),
        (Some(4), Some(4)),
        "CR 708.8: the restored base is the printed face"
    );
    assert_eq!(
        obj.power,
        Some(5),
        "the surviving counter applies ON TOP of the printed base — exactly once"
    );
}

/// CR 710.4 + CR 710.2: a flipped permanent's `back_face` slot already holds
/// its stashed NORMAL half. The direct turn must keep that stash (it is what
/// leaves the battlefield later), not overwrite it with a snapshot of the
/// flipped half.
#[test]
fn a_flipped_permanents_normal_half_survives_the_turn_face_down() {
    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature(P0, "Alternative Half", 4, 4)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();
    let normal = scenario.add_creature(P0, "Normal Half", 1, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let stash =
        engine::game::printed_cards::snapshot_object_base_face(&runner.state().objects[&normal]);
    {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.flipped = true;
        obj.back_face = Some(stash.clone());
    }

    runner
        .act(GameAction::Debug(DebugAction::SetFaceState {
            object_id: id,
            face_down: Some(true),
            transformed: None,
            flipped: None,
        }))
        .expect("the debug face-down write runs");
    assert_eq!(
        runner.state().objects[&id]
            .back_face
            .as_ref()
            .map(|face| face.name.as_str()),
        Some("Normal Half"),
        "the flip stash is the face that must reappear off the battlefield"
    );
}

/// CR 712.16 + CR 730.2j: double-faced and melded permanents can't be turned
/// face down. The sandbox reports the refusal instead of silently corrupting
/// the permanent, mirroring the face-up arm's error stance.
#[test]
fn a_melded_permanent_refuses_the_debug_turn_face_down() {
    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature(P0, "Melded Horror", 9, 10)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner.state_mut().objects.get_mut(&id).unwrap().merge_kind =
        Some(engine::game::game_object::MergeKind::Meld);

    let refused = runner.act(GameAction::Debug(DebugAction::SetFaceState {
        object_id: id,
        face_down: Some(true),
        transformed: None,
        flipped: None,
    }));
    assert!(
        refused.is_err(),
        "CR 730.2j: the tool must refuse, not corrupt"
    );
    let obj = &runner.state().objects[&id];
    assert!(!obj.face_down, "nothing happened");
    assert_eq!(obj.name, "Melded Horror", "characteristics unchanged");
}
