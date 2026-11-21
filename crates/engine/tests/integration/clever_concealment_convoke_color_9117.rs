//! Issue #9117: Clever Concealment ({2}{W}{W}, Convoke) with Arcane Signet, a Swamp and two
//! creatures.
//!
//! CR 702.51a: each creature tapped for convoke pays for {1} or for one mana of that
//! creature's color, so a white creature can pay a {W} pip while a non-white creature pays
//! only generic mana. Arcane Signet adds one mana of any color in its controller's
//! commander's color identity, and nothing at all without a commander (CR 903.4f). The
//! castability offer and the payment must agree on both rules: two black creatures can't
//! supply the second {W}, but one white creature plus a white-producing Signet can.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{ConvokeMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    signet: ObjectId,
    swamp: ObjectId,
    creatures: Vec<ObjectId>,
}

/// P0 in main phase with Arcane Signet, a Swamp, one creature per entry of
/// `creature_colors`, and Clever Concealment in hand. `commander_colors` places a
/// commander of those colors in the command zone; `None` means P0 has no commander.
fn setup(commander_colors: Option<Vec<ManaColor>>, creature_colors: &[ManaColor]) -> Board {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if let Some(colors) = commander_colors {
        let commander = scenario
            .add_creature(P0, "Commander", 2, 2)
            .with_color(colors)
            .as_legendary()
            .id();
        scenario.with_commander(commander);
    }
    let signet = scenario.add_real_card(P0, "Arcane Signet", Zone::Battlefield, db);
    let swamp = scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    let creatures = creature_colors
        .iter()
        .map(|&color| {
            scenario
                .add_creature(P0, "Helper", 1, 1)
                .with_color(vec![color])
                .id()
        })
        .collect();
    let spell = scenario.add_real_card(P0, "Clever Concealment", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    Board {
        runner,
        spell,
        signet,
        swamp,
        creatures,
    }
}

fn white_black() -> Option<Vec<ManaColor>> {
    Some(vec![ManaColor::White, ManaColor::Black])
}

fn cast_offered(board: &Board) -> bool {
    legal_actions(board.runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == board.spell),
    )
}

/// Announce the spell with no targets ("any number of target ...", CR 601.2c) and stop at
/// the convoke payment step.
fn begin_cast(board: &mut Board) {
    let card_id = board.runner.state().objects[&board.spell].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: board.spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("the cast is announced");
    while matches!(
        board.runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        board
            .runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("a target slot of \"any number of targets\" can be left empty");
    }
    let WaitingFor::ManaPayment { convoke_mode, .. } = &board.runner.state().waiting_for else {
        panic!(
            "expected the convoke payment step, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*convoke_mode, Some(ConvokeMode::Convoke));
}

fn tap_for_convoke(board: &mut Board, creature: ObjectId, mana_type: ManaType) -> bool {
    board
        .runner
        .act(GameAction::TapForConvoke {
            object_id: creature,
            mana_type,
        })
        .is_ok()
}

fn finish_payment(board: &mut Board) -> bool {
    board.runner.act(GameAction::PassPriority).is_ok()
}

fn assert_cast_completed(board: &Board) {
    let state = board.runner.state();
    assert_eq!(state.objects[&board.spell].zone, Zone::Stack);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        state.objects[&board.signet].tapped && state.objects[&board.swamp].tapped,
        "Arcane Signet and the Swamp pay the rest of the cost"
    );
    assert_eq!(
        state.objects[&board.spell].convoked_creatures,
        board.creatures
    );
}

fn assert_cast_stuck_in_payment(board: &Board) {
    let state = board.runner.state();
    assert_eq!(state.objects[&board.spell].zone, Zone::Hand);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ManaPayment {
            convoke_mode: Some(ConvokeMode::Convoke),
            ..
        }
    ));
}

#[test]
fn two_black_creatures_cannot_pay_the_second_white_pip() {
    let mut board = setup(white_black(), &[ManaColor::Black, ManaColor::Black]);
    // Signet makes one {W}; the Swamp and the two black creatures make only {B} or {1}.
    assert!(!cast_offered(&board), "only one {{W}} is available");

    begin_cast(&mut board);
    let [first, second] = board.creatures[..] else {
        unreachable!()
    };
    assert!(
        !tap_for_convoke(&mut board, first, ManaType::White),
        "a black creature can't convoke for {{W}}"
    );
    assert!(tap_for_convoke(&mut board, first, ManaType::Colorless));
    assert!(tap_for_convoke(&mut board, second, ManaType::Colorless));
    assert!(!finish_payment(&mut board), "{{W}} is still unpaid");
    assert_cast_stuck_in_payment(&board);
}

#[test]
fn one_white_creature_and_the_signet_pay_both_white_pips() {
    let mut board = setup(white_black(), &[ManaColor::White, ManaColor::Black]);
    assert!(cast_offered(&board));

    begin_cast(&mut board);
    let [white, black] = board.creatures[..] else {
        unreachable!()
    };
    assert!(tap_for_convoke(&mut board, white, ManaType::White));
    assert!(tap_for_convoke(&mut board, black, ManaType::Colorless));
    // The remaining {1}{W} is paid by the Signet's {W} and the Swamp's {B}.
    assert!(finish_payment(&mut board));
    assert_cast_completed(&board);
}

#[test]
fn two_white_creatures_pay_both_white_pips_when_the_signet_makes_only_black() {
    let mut board = setup(
        Some(vec![ManaColor::Black]),
        &[ManaColor::White, ManaColor::White],
    );
    assert!(cast_offered(&board));

    begin_cast(&mut board);
    for creature in board.creatures.clone() {
        assert!(tap_for_convoke(&mut board, creature, ManaType::White));
    }
    assert!(finish_payment(&mut board));
    assert_cast_completed(&board);
}

#[test]
fn without_a_commander_the_signet_adds_nothing() {
    let mut board = setup(None, &[ManaColor::White, ManaColor::White]);
    // CR 903.4f: the creatures pay {W}{W} and the Swamp {1}; the Signet can't pay the last {1}.
    assert!(!cast_offered(&board));

    begin_cast(&mut board);
    for creature in board.creatures.clone() {
        assert!(tap_for_convoke(&mut board, creature, ManaType::White));
    }
    assert!(!finish_payment(&mut board));
    assert_cast_stuck_in_payment(&board);
}
