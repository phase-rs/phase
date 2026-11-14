//! CR 116.2m + CR 605.4a + CR 608.2d: paying a Room's unlock cost must settle a
//! mandatory delayed-mana choice BEFORE spending, and the door stays locked
//! until the payment completes.
//!
//! Public-path regression for the resumable unlock seam. The Room is
//! Greenhouse // Rickety Gazebo: casting the right half (Rickety Gazebo)
//! unlocks the right door, leaving Greenhouse locked at {2}{G} (CR 709.5d-e).
//! The only mana source is Simic Growth Chamber ({T}: Add {G}{U}), so the base
//! tap is one mana short and Selesnya Loft Gardens' chaos bonus must fund the
//! difference. Before the typed continuation, the unlock spent the base {G}{U}
//! and only then hit the unresolved bonus; now the choice is offered with
//! nothing spent and the designation is committed only after the answer.

use engine::game::game_object::RoomDoor;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::process_triggers;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const SELESNYA_ORACLE: &str = "If an effect would create one or more tokens, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent, it puts twice that many of those counters on that permanent instead.\nWhenever chaos ensues, until end of turn, whenever you tap a land for mana, add one mana of any type that land produced.";
const CHAMBER_ORACLE: &str =
    "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {G}{U}.";

fn count(runner: &GameRunner, color: ManaType) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .count_color(color)
}

fn clear_pool(runner: &mut GameRunner) {
    runner.state_mut().players[P0.0 as usize].mana_pool.clear();
}

/// Selesnya live, one untapped Simic Growth Chamber, and a battlefield
/// Greenhouse // Rickety Gazebo whose RIGHT half was cast — so only the
/// Greenhouse door ({2}{G}) is locked.
fn runner_with_locked_greenhouse() -> (GameRunner, ObjectId, ObjectId) {
    let db = shared_card_db().expect("the committed card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selesnya = scenario
        .add_land_from_oracle(P0, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    let chamber = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let room = scenario.add_real_card(P0, "Greenhouse", Zone::Hand, db);
    // Cast Rickety Gazebo ({3}{G}) without consuming the Chamber under test.
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Green, room, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.cast(room).modal_back_face(true).resolve();
    clear_pool(&mut runner);

    let obj = runner
        .state()
        .objects
        .get(&room)
        .expect("the Room resolved onto the battlefield");
    assert_eq!(obj.zone, Zone::Battlefield);
    let unlocks = obj.room_unlocks.expect("a Room tracks its doors");
    assert!(
        unlocks.right_unlocked,
        "premise: casting Rickety Gazebo unlocks the right door"
    );
    assert!(
        !unlocks.left_unlocked,
        "premise: the Greenhouse door stays locked at its own cost"
    );

    // CR 311.7 / CR 901.9b: synthesize the chaos event keyed by Selesnya's
    // object id; the real plane-identity binding installs the delayed trigger.
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    while runner.state().delayed_triggers.is_empty() && !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolving the chaos trigger must succeed");
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "chaos must install exactly one Selesnya delayed trigger"
    );
    (runner, room, chamber)
}

/// CR 116.2m + CR 709.5e + CR 605.4a + CR 608.2d: the unlock payment pauses on
/// the mandatory bonus choice with the door still locked and no mana of the
/// cost spent; the answer resumes the exact unlock and commits it.
#[test]
fn unlock_room_door_requires_bonus_choice_before_spending() {
    let (mut runner, room, chamber) = runner_with_locked_greenhouse();

    runner
        .act(GameAction::UnlockRoomDoor {
            object_id: room,
            door: RoomDoor::Left,
        })
        .expect("the unlock special action must reach the delayed-mana choice");

    // The bonus must NOT resolve silently or after a partial spend: base {G}{U}
    // is in the pool and the choice is pending with the door still locked.
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "the choice belongs to the unlocking player");
            assert_eq!(
                options.len(),
                2,
                "the choice must offer exactly the chamber's produced types; got {options:?}"
            );
            assert!(
                options.contains(&ManaType::Green) && options.contains(&ManaType::Blue),
                "the choice must offer Green and Blue; got {options:?}"
            );
        }
        other => panic!("expected the Selesnya echo choice, got {other:?}"),
    }
    assert!(
        !runner.state().objects[&room]
            .room_unlocks
            .expect("a Room tracks its doors")
            .left_unlocked,
        "the door stays locked until the whole unlock cost is paid"
    );
    assert_eq!(
        count(&runner, ManaType::Green),
        1,
        "the chamber's base {{G}} is still unspent while the choice is pending"
    );
    assert_eq!(
        count(&runner, ManaType::Blue),
        1,
        "the chamber's base {{U}} is still unspent while the choice is pending"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the echo resolves inline, not on the stack"
    );
    assert!(
        runner.state().objects[&chamber].tapped,
        "the Chamber is the source whose tap owes the choice"
    );

    let resumed = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering the bonus must resume the unlock payment");

    let obj = &runner.state().objects[&room];
    assert!(
        obj.room_unlocks
            .expect("a Room tracks its doors")
            .left_unlocked,
        "the resumed payment commits the Greenhouse door"
    );
    assert_eq!(
        count(&runner, ManaType::Green) + count(&runner, ManaType::Blue),
        0,
        "base {{G}}{{U}} plus the chosen bonus exactly pays Greenhouse's {{2}}{{G}}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the completed special action returns priority to its controller, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        resumed.events.iter().any(|event| matches!(
            event,
            GameEvent::RoomDoorUnlocked {
                object_id,
                door: RoomDoor::Left,
                ..
            } if *object_id == room
        )),
        "the designation change is emitted exactly when the payment completes"
    );
}
