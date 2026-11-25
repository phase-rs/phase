//! CR 702.16p + CR 733.1: canceling an unfinished cast restores what its
//! payments changed, and a restored permanent keeps the protection start
//! snapshot it had. The snapshot is fixed "when the protection effect starts
//! to apply", not when the cancellation rebuilds layers.
//!
//! The board, every card printed and loaded from the card data:
//! - Painter's Servant ("As this creature enters, choose a color. All cards
//!   that aren't on the battlefield, spells, and permanents are the chosen
//!   color in addition to their other colors.") names white.
//! - Bonesplitter ("Equipped creature gets +2/+0. Equip {1}") equips Grizzly
//!   Bears, so it is a white Equipment its controller controls.
//! - Benevolent Blessing ("Enchanted creature has protection from the chosen
//!   color. This effect doesn't remove Auras and Equipment you control that
//!   are already attached to it.") enchants the Bears, choosing white. Its
//!   start snapshot holds Bonesplitter.
//! - Lightning Bolt ("Lightning Bolt deals 3 damage to any target.") kills the
//!   Servant, so Bonesplitter is colorless again.
//! - Thrill of Possibility ("As an additional cost to cast this spell, discard
//!   a card. Draw two cards.") is cast and its discard paid. At its mana
//!   payment the Bears are sacrificed to Ashnod's Altar ("Sacrifice a
//!   creature: Add {C}{C}."), which ends Blessing's effect on them and drops
//!   its snapshot. The game stops there.
//!
//! Canceling Thrill returns the Bears, and with them Blessing's snapshot. A
//! second Painter's Servant then names white: Bonesplitter is white again, and
//! stays attached only if Blessing still holds the snapshot from when its
//! effect started. A snapshot taken at the cancellation would not hold the
//! then-colorless Bonesplitter, and the state-based action would unattach it
//! (CR 704.5n).

use std::io::Read;

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

/// The save upstream main's writer at `917542cd0` (release v0.123.0, which
/// predates the occurrence keys) produced at Thrill of
/// Possibility's mana payment on the board above, with three players. It was
/// made by running this file's `play_to_the_payment_window` in a checkout of
/// that commit (with Benevolent Blessing's printed record added to its card
/// fixture) and writing `PersistedGameState::capture` of the result. Its saved
/// inverse of the Altar's sacrifice stores Blessing's snapshot under the older
/// `protection_start_exempt_attachments` slot key.
const PREDECESSOR_SAVE: &[u8] =
    include_bytes!("../fixtures/protection_start_snapshot_cast_undo_predecessor.json.gz");

fn fund(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    for mana in [ManaType::White, ManaType::Red, ManaType::Colorless] {
        for _ in 0..10 {
            state
                .add_mana_to_pool(player, ManaUnit::new(mana, ObjectId(0), false, vec![]))
                .unwrap();
        }
    }
}

/// Answer the "choose a color" prompt the last cast must have left.
fn choose_white(runner: &mut GameRunner) {
    let WaitingFor::NamedChoice {
        player, options, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the permanent must ask for a color, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "its controller chooses");
    let choice = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("white"))
        .expect("white is offered")
        .clone();
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("the color choice resolves");
    runner.advance_until_stack_empty();
}

fn find(state: &GameState, name: &str, zone: Zone) -> ObjectId {
    let mut found = state
        .objects
        .values()
        .filter(|object| object.name == name && object.zone == zone);
    let object = found
        .next()
        .unwrap_or_else(|| panic!("{name} must be in {zone:?}"));
    assert!(found.next().is_none(), "one {name} in {zone:?}");
    object.id
}

/// Play the board to Thrill of Possibility's mana payment, with its discard
/// paid and the Bears sacrificed to Ashnod's Altar.
fn play_to_the_payment_window() -> GameRunner {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bonesplitter = scenario.add_real_card(P0, "Bonesplitter", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let painter = scenario.add_real_card(P0, "Painter's Servant", Zone::Hand, db);
    let blessing = scenario.add_real_card(P0, "Benevolent Blessing", Zone::Hand, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let thrill = scenario.add_real_card(P0, "Thrill of Possibility", Zone::Hand, db);
    let discarded = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.add_real_card(P0, "Painter's Servant", Zone::Hand, db);
    // Two untapped Mountains can pay Thrill, so its announcement is legal.
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();

    fund(&mut runner, P0);
    runner.cast(painter).resolve();
    choose_white(&mut runner);
    fund(&mut runner, P0);
    runner
        .activate(bonesplitter, 0)
        .target_object(bears)
        .resolve();
    runner.advance_until_stack_empty();
    fund(&mut runner, P0);
    runner.cast(blessing).target_object(bears).resolve();
    choose_white(&mut runner);
    fund(&mut runner, P0);
    runner.cast(bolt).target_object(painter).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&painter].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&bonesplitter].attached_to,
        Some(bears.into())
    );
    assert_eq!(
        runner.state().objects[&blessing].attached_to,
        Some(bears.into())
    );

    // The pool is emptied so the payment window stays open.
    let state = runner.state_mut();
    state.players[0].mana_pool = Default::default();
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    let card_id = runner.state().objects[&thrill].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: thrill,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Thrill of Possibility is announced");
    for step in 0.. {
        assert!(step < 8, "the cast reaches its mana payment");
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => break,
            WaitingFor::PayCost { .. } => GameAction::SelectCards {
                cards: vec![discarded],
            },
            other => panic!("unexpected step while casting Thrill of Possibility: {other:?}"),
        };
        runner.act(action).expect("the casting step is accepted");
    }
    assert_eq!(
        runner.state().objects[&discarded].zone,
        Zone::Graveyard,
        "reach guard: the discard is paid before the mana payment"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Ashnod's Altar's mana ability is activated at the payment window");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "the Altar asks for its sacrifice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards { cards: vec![bears] })
        .expect("the Bears are sacrificed");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the game stops at the mana payment, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    runner
}

fn restore(wire: serde_json::Value) -> GameRunner {
    let state = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the save decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("the save is admitted")
        .finalize_immediately()
        .expect("the save finalizes");
    GameRunner::from_state(state)
}

/// Cancel Thrill of Possibility (after `before_cancel`), then turn Bonesplitter
/// white again with the second Painter's Servant.
fn cancel_then_repaint(mut runner: GameRunner, before_cancel: Option<GameAction>) {
    let bears = find(runner.state(), "Grizzly Bears", Zone::Graveyard);
    let bonesplitter = find(runner.state(), "Bonesplitter", Zone::Battlefield);
    let blessing = find(runner.state(), "Benevolent Blessing", Zone::Battlefield);
    let painter = find(runner.state(), "Painter's Servant", Zone::Hand);
    assert!(
        !runner.state().objects[&bonesplitter]
            .color
            .contains(&ManaColor::White),
        "reach guard: Bonesplitter is colorless while no Painter's Servant is out"
    );

    assert!(
        runner.state().objects[&blessing]
            .protection_start_snapshots
            .is_empty(),
        "reach guard: the Bears left, so Blessing holds no snapshot until the cancellation"
    );

    if let Some(action) = before_cancel {
        runner.act(action).expect("the independent action applies");
        let reversal =
            serde_json::to_value(runner.state()).unwrap()["spell_cast_reversal"].to_string();
        assert!(
            reversal.contains("protection_start_snapshots")
                && !reversal.contains("protection_start_exempt_attachments"),
            "reach guard: the action rebased the saved inverse, which now names the snapshot by its occurrence key"
        );
    }
    runner
        .act(GameAction::CancelCast)
        .expect("the cast can be canceled");
    let state = runner.state();
    assert_eq!(state.objects[&bears].zone, Zone::Battlefield);
    assert_eq!(state.objects[&blessing].zone, Zone::Battlefield);
    assert_eq!(state.objects[&blessing].attached_to, Some(bears.into()));
    assert_eq!(
        state.objects[&bonesplitter].attached_to,
        Some(bears.into()),
        "reach guard: the cancellation returns Bonesplitter to the Bears"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        state.waiting_for
    );

    fund(&mut runner, P0);
    runner.cast(painter).resolve();
    choose_white(&mut runner);
    let state = runner.state();
    assert!(
        state.objects[&bonesplitter]
            .color
            .contains(&ManaColor::White),
        "reach guard: the second Painter's Servant made Bonesplitter white"
    );
    assert!(
        state.objects[&bears]
            .keywords
            .iter()
            .any(|keyword| matches!(
                keyword,
                engine::types::keywords::Keyword::Protection(
                    engine::types::keywords::ProtectionTarget::Color(ManaColor::White)
                )
            )),
        "reach guard: the Bears have protection from white again"
    );
    assert_eq!(
        state.objects[&bonesplitter].attached_to,
        Some(bears.into()),
        "Bonesplitter was white and attached when Blessing's effect started, so it stays"
    );
}

/// The cast is canceled in the game that started it.
#[test]
fn canceling_a_cast_keeps_the_start_snapshot() {
    cancel_then_repaint(play_to_the_payment_window(), None);
}

/// The game is saved at the mana payment by this build and restored first.
#[test]
fn canceling_a_restored_cast_keeps_the_start_snapshot() {
    let runner = play_to_the_payment_window();
    let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
    cancel_then_repaint(restore(wire), None);
}

fn predecessor_save() -> serde_json::Value {
    let mut json = String::new();
    flate2::read::GzDecoder::new(PREDECESSOR_SAVE)
        .read_to_string(&mut json)
        .expect("the fixture inflates to UTF-8 JSON");
    assert!(
        json.contains("protection_start_exempt_attachments"),
        "reach guard: the save stores a snapshot under the older slot key"
    );
    assert!(
        !json.contains("protection_start_snapshots"),
        "reach guard: the save predates the occurrence keys"
    );
    serde_json::from_str(&json).expect("the fixture is JSON")
}

/// The save was written by the older build, whose inverse of the
/// Altar's sacrifice stores Blessing's snapshot under the older slot key.
#[test]
fn canceling_a_predecessors_saved_cast_keeps_the_start_snapshot() {
    cancel_then_repaint(restore(predecessor_save()), None);
}

/// CR 800.4a: another player concedes between the restore and the cancel, so
/// the saved inverse is rebased through the same reconstruction first.
#[test]
fn a_concession_before_canceling_a_predecessors_saved_cast_keeps_the_start_snapshot() {
    cancel_then_repaint(
        restore(predecessor_save()),
        Some(GameAction::Concede { player_id: P2 }),
    );
}
