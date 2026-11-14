//! Regression for issue #7591: a permanent that becomes a copy of a Room must
//! show the COPIED halves through its OWN unlocked designations.
//!
//! https://github.com/phase-rs/phase/issues/7591
//!
//! CR 709.5d: "A permanent with a shared type line is given the 'left half
//! unlocked' designation as it enters the battlefield if its left half was cast
//! as a spell. […] If it's entering the battlefield and neither half was cast
//! as a spell, it enters with neither unlocked designation."
//!
//! Both entry seams used to answer a different question. The ordinary
//! resolution tail re-derived the door from the object's post-entry form, and
//! the replacement-choice resume path granted `RoomDoor::Left` unconditionally
//! — so a Copy Enchantment entering as a copy of a Room was handed a
//! designation although none of its own halves was ever cast.
//!
//! These drive the real cast path from real card data. The `printed_cards`
//! unit tests build their Room fixtures directly, so a defect in the
//! cast/entry install path is invisible to them.

use engine::game::game_object::RoomDoor;
use engine::game::room::eligible_doors;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{DoorLockOp, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// `Greenhouse {2}{G} // Rickety Gazebo {3}{G}` (DSK) is the Room under test:
/// Greenhouse is the printed LEFT half and carries a visible static ability,
/// Rickety Gazebo the right one. `Copy Enchantment {2}{U}` and
/// `Mirrormade {1}{U}{U}` are the two enter-as-copy sources.
fn issue_7591_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_7591_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("card-data export must load")
    })
}

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn green(runner: &mut GameRunner) {
    add_mana(
        runner,
        &[
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
}

fn blue(runner: &mut GameRunner) {
    add_mana(
        runner,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
}

/// `back_face`: which printed half to cast when the Room asks (CR 709.5d —
/// casting a half unlocks THAT door on entry). `true` = the right half.
fn drive_until_stack_empty(runner: &mut GameRunner, back_face: bool) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ModalFaceChoice { .. } => {
                runner
                    .act(GameAction::ChooseModalFace { back_face })
                    .expect("choose the Room half to cast");
            }
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept enter-as-copy replacement");
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                let target = valid_targets[0];
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose copy target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional copy");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner.choose_first_legal_target().expect("choose target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for while resolving: {other:?}"),
        }
    }
    panic!("resolution loop exhausted");
}

fn cast(runner: &mut GameRunner, object_id: ObjectId, back_face: bool) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast spell");
    drive_until_stack_empty(runner, back_face);
}

/// The Room card is added by its printed FRONT half ("Greenhouse"), which is
/// how the card exists; `cast(.., back_face)` then picks the half to cast.
fn room_and_copier(copier: &str) -> (GameRunner, ObjectId, ObjectId) {
    let db = issue_7591_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let room_card = scenario.add_real_card(P0, "Greenhouse", Zone::Hand, db);
    let copier = scenario.add_real_card(P0, copier, Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner, room_card, copier)
}

fn newest_non(runner: &GameRunner, known: &[ObjectId]) -> ObjectId {
    *runner
        .state()
        .battlefield
        .iter()
        .find(|id| !known.contains(id))
        .expect("the new permanent is on the battlefield")
}

fn offered(runner: &GameRunner, id: ObjectId) -> Vec<RoomDoor> {
    eligible_doors(runner.state(), id, DoorLockOp::Unlock)
        .into_iter()
        .map(|(_, door)| door)
        .collect()
}

/// CR 709.5d + CR 709.5: casting a Room's RIGHT half unlocks that door on
/// entry, and an unlocked half HAS its name.
///
/// This test and its left-half twin do NOT fail when either entry seam is
/// reverted: an ordinarily cast Room's printed type line always carries `Room`,
/// so the new reader returns exactly what the old expression did. They are the
/// counter-direction pins — measured red only when the designation is refused
/// wholesale rather than only for uncast halves, which also fails the existing
/// `issue_4248_room_cast_both_halves` test.
#[test]
fn casting_a_rooms_right_half_unlocks_that_door_only() {
    let (mut runner, room_card, _) = room_and_copier("Copy Enchantment");
    green(&mut runner);
    cast(&mut runner, room_card, true);

    assert_eq!(
        runner.state().objects[&room_card].name,
        "Rickety Gazebo",
        "the cast half is unlocked, so the permanent has its name"
    );
    assert_eq!(
        offered(&runner, room_card),
        vec![RoomDoor::Left],
        "CR 709.5e: an unlock cost may only be paid for a LOCKED half"
    );
}

/// The other orientation of the test above. Same standing: it pins
/// over-suppression, not the fix itself (see that test's note).
#[test]
fn casting_a_rooms_left_half_unlocks_that_door_only() {
    let (mut runner, room_card, _) = room_and_copier("Copy Enchantment");
    green(&mut runner);
    cast(&mut runner, room_card, false);

    assert_eq!(
        runner.state().objects[&room_card].name,
        "Greenhouse",
        "the cast half is unlocked, so the permanent has its name"
    );
    assert_eq!(
        offered(&runner, room_card),
        vec![RoomDoor::Right],
        "CR 709.5e: an unlock cost may only be paid for a LOCKED half"
    );
}

/// CR 709.5d last sentence + CR 709.5c: a Copy Enchantment entering as a copy
/// of a Room had NEITHER of its halves cast — it has none. It enters with
/// neither designation, therefore with no name (CR 709.5), and both copied
/// halves are offered for unlocking.
///
/// Revert-failing assertions, measured: reinstating the unconditional
/// `RoomDoor::Left` in the replacement-choice resume path, or reading the
/// current instead of the printed type line, turns this test red — the copy
/// then enters named "Greenhouse" with only its right door offered, and
/// Greenhouse's static ability functions on a permanent that unlocked nothing.
/// Reverting the AI door enumeration turns its last assertion red.
#[test]
fn a_copy_of_a_room_enters_with_neither_designation() {
    let (mut runner, room_card, copier) = room_and_copier("Copy Enchantment");
    green(&mut runner);
    cast(&mut runner, room_card, true);
    blue(&mut runner);
    cast(&mut runner, copier, false);

    let copy = newest_non(&runner, &[room_card]);
    let copy_obj = &runner.state().objects[&copy];

    assert_eq!(
        copy_obj.name, "",
        "CR 709.5: both halves locked means neither half's name — no name at all"
    );
    assert_eq!(
        copy_obj.room_unlocks.unwrap_or_default(),
        Default::default(),
        "CR 709.5d: neither half was cast, so neither designation is given"
    );
    assert_eq!(
        offered(&runner, copy),
        vec![RoomDoor::Left, RoomDoor::Right],
        "CR 709.5e + CR 709.5j: both copied halves exist and both are locked"
    );

    // CR 709.5c: the ORIGINAL's designations are its own status and are
    // untouched by anything the copy does.
    assert_eq!(
        runner.state().objects[&room_card].name,
        "Rickety Gazebo",
        "the copied-from Room keeps its own designation"
    );

    // CR 709.5b + CR 707.2: the door list a BOT sees comes from the same
    // authority. The AI candidate generator enumerated the doors itself from
    // `back_face`, which an enter-as-copy recipient does not have — the right
    // door exists only through the COPIED halves, so a bot could never unlock
    // it.
    // The candidate list is cost-filtered, so fund both copied unlock costs
    // ({2}{G} and {3}{G}) before asking which doors it offers.
    green(&mut runner);
    green(&mut runner);
    let bot_doors: Vec<RoomDoor> = engine::ai_support::legal_actions(runner.state())
        .iter()
        .filter_map(|action| match action {
            GameAction::UnlockRoomDoor { object_id, door } if *object_id == copy => Some(*door),
            _ => None,
        })
        .collect();
    assert_eq!(
        bot_doors,
        vec![RoomDoor::Left, RoomDoor::Right],
        "the AI candidate list offers the same two doors as the human offer"
    );
}

/// CR 709.5e + CR 707.2: unlocking a door of the copy pays the COPIED half's
/// mana cost and gives the copy that half's name — Greenhouse {2}{G}, not the
/// recipient's printed Copy Enchantment {2}{U}.
#[test]
fn unlocking_a_copied_half_uses_the_copied_cost_and_name() {
    let (mut runner, room_card, copier) = room_and_copier("Copy Enchantment");
    green(&mut runner);
    cast(&mut runner, room_card, true);
    blue(&mut runner);
    cast(&mut runner, copier, false);
    let copy = newest_non(&runner, &[room_card]);

    green(&mut runner);
    let before = runner.state().players[0].mana_pool.total();
    runner
        .act(GameAction::UnlockRoomDoor {
            object_id: copy,
            door: RoomDoor::Left,
        })
        .expect("unlock the copied Greenhouse half");
    drive_until_stack_empty(&mut runner, false);

    assert_eq!(
        runner.state().objects[&copy].name,
        "Greenhouse",
        "the newly unlocked half is the COPIED left half"
    );
    assert_eq!(
        before - runner.state().players[0].mana_pool.total(),
        3,
        "CR 709.5e: the unlock cost is the copied half's {{2}}{{G}}"
    );
}

/// CR 707.3 + CR 709.5d: a copy of an already-copied Room is still a permanent
/// none of whose own halves was cast, so it too enters fully locked — and it
/// copies the ROOM's halves, not the printed `Copy Enchantment`.
#[test]
fn a_copy_of_a_copied_room_also_enters_fully_locked() {
    let db = issue_7591_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let room_card = scenario.add_real_card(P0, "Greenhouse", Zone::Hand, db);
    let copier = scenario.add_real_card(P0, "Copy Enchantment", Zone::Hand, db);
    let second = scenario.add_real_card(P0, "Mirrormade", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    green(&mut runner);
    cast(&mut runner, room_card, true);
    blue(&mut runner);
    cast(&mut runner, copier, false);
    let copy = newest_non(&runner, &[room_card]);

    blue(&mut runner);
    cast(&mut runner, second, false);
    let copy_of_copy = newest_non(&runner, &[room_card, copy]);

    assert_eq!(
        runner.state().objects[&copy_of_copy].name,
        "",
        "CR 709.5d: neither of ITS halves was cast either"
    );
    assert_eq!(
        offered(&runner, copy_of_copy),
        vec![RoomDoor::Left, RoomDoor::Right],
        "CR 707.3: it snapshotted the copied Room's two halves, not Copy Enchantment"
    );
}
