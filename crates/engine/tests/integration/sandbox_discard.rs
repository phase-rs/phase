//! Sandbox `DebugAction::Discard`: discard one hand card through the real
//! discard authority, as an effect-caused discard with no source object.
//!
//! Every row drives `GameRunner::act(GameAction::Debug(..))` (or `apply()` for
//! a non-priority seat) from `WaitingFor::Priority` in `PreCombatMain` with
//! `debug_mode` on, using `GameScenarioDbExt::add_real_card`.
//!
//! Verbatim Oracle text, as the committed fixture stores it:
//!   Geth's Grimoire: "Whenever an opponent discards a card, you may draw a card."
//!   Waste Not: "Whenever an opponent discards a creature card, create a 2/2
//!     black Zombie creature token. / Whenever an opponent discards a land
//!     card, add {B}{B}. / Whenever an opponent discards a noncreature,
//!     nonland card, draw a card."
//!   Rest in Peace: "When this enchantment enters, exile all graveyards. / If a
//!     card or token would be put into a graveyard from anywhere, exile it
//!     instead."
//!   Library of Leng: "You have no maximum hand size. / If an effect causes you
//!     to discard a card, discard it, but you may put it on top of your library
//!     instead of into your graveyard."
//!   Violent Eruption: "Violent Eruption deals 4 damage divided as you choose
//!     among any number of targets. / Madness {1}{R}{R} (If you discard this
//!     card, discard it into exile. When you do, cast it for its madness cost or
//!     put it into your graveyard.)"
//!   Loxodon Smiter: "This spell can't be countered. / If a spell or ability an
//!     opponent controls causes you to discard this card, put it onto the
//!     battlefield instead of putting it into your graveyard."

use std::collections::BTreeSet;

use engine::database::card_db::CardDatabase;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Library cards per seat, so no row can end on a draw from an empty library.
const LIBRARY_PER_SEAT: usize = 4;

fn table(db: &CardDatabase, setup: impl FnOnce(&mut GameScenario, &CardDatabase)) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in [P0, P1] {
        for _ in 0..LIBRARY_PER_SEAT {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    setup(&mut scenario, db);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
}

fn discard(object_id: ObjectId) -> GameAction {
    GameAction::Debug(DebugAction::Discard { object_id })
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn discarded_events(events: &[GameEvent]) -> Vec<(PlayerId, ObjectId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::Discarded {
                player_id,
                object_id,
                ..
            } => Some((*player_id, *object_id)),
            _ => None,
        })
        .collect()
}

fn has_debug_used(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::DebugActionUsed { .. }))
}

fn is_priority(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

fn digest(runner: &GameRunner) -> serde_json::Value {
    serde_json::to_value(runner.state()).expect("game state serializes")
}

fn count_named_on_battlefield(runner: &GameRunner, player: PlayerId, name: &str) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == player && o.name == name)
        })
        .count()
}

/// R1: a basic discard goes hand -> graveyard as exactly one discard.
#[test]
fn r1_basic_discard_goes_to_graveyard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert!(has_debug_used(&result.events));
    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(is_priority(&runner));
}

/// R2: an opponent's discard trigger fires exactly once; a player's own
/// discard does not trigger it.
#[test]
fn r2_opponent_discard_trigger_fires_once() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((grimoire, card));
    });
    let (grimoire, card) = ids.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(runner.state().stack.len(), 1, "one Grimoire trigger");
    assert_eq!(runner.state().stack[0].source_id, grimoire);
    assert!(is_priority(&runner));
}

#[test]
fn r2_own_discard_does_not_trigger_opponents_only_card() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        card = Some(s.add_real_card(P1, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    // Reach guard: the discard happened, by P1.
    assert_eq!(discarded_events(&result.events), vec![(P1, card)]);
    assert!(runner.state().stack.is_empty());
}

/// R3: the typed opponent trigger reads the discarded card.
#[test]
fn r3_waste_not_zombie_for_creature_card_only() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Waste Not", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();
    assert_eq!(count_named_on_battlefield(&runner, P1, "Zombie"), 0);

    runner.act(discard(card)).expect("discard succeeds");
    assert_eq!(runner.state().stack.len(), 1, "one Waste Not trigger");
    runner.resolve_top();

    assert_eq!(count_named_on_battlefield(&runner, P1, "Zombie"), 1);
}

#[test]
fn r3_waste_not_no_zombie_for_land_card() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Waste Not", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Swamp", Zone::Hand, db));
    });
    let card = card.unwrap();

    runner.act(discard(card)).expect("discard succeeds");
    // Reach guard: the land-card trigger (not the creature one) is on the stack.
    assert_eq!(runner.state().stack.len(), 1, "one Waste Not trigger");
    runner.resolve_top();

    assert_eq!(count_named_on_battlefield(&runner, P1, "Zombie"), 0);
}

/// R4: a redirected discard is still a discard (CR 701.9c per discard.rs).
#[test]
fn r4_rest_in_peace_redirect_is_still_a_discard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
        s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(runner.state().stack.len(), 1, "one Grimoire trigger");
}

/// R5: Madness offers its cast after the Madness trigger resolves.
#[test]
fn r5_madness_offers_cast_then_decline_goes_to_graveyard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        card = Some(s.add_real_card(P0, "Violent Eruption", Zone::Hand, db));
    });
    let card = card.unwrap();

    runner.act(discard(card)).expect("discard succeeds");
    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(runner.state().stack.len(), 1, "one Madness trigger");

    runner.resolve_top();
    match &runner.state().waiting_for {
        WaitingFor::CastOffer {
            player,
            kind: CastOfferKind::Madness { object_id, .. },
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*object_id, card);
        }
        other => panic!("expected the Madness cast offer, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the Madness cast succeeds");
    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
}

/// R6: the cause is Effect, so Library of Leng's replacement applies.
#[test]
fn r6_library_of_leng_applies_to_sandbox_discard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    runner.act(discard(card)).expect("discard succeeds");
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P0),
        other => panic!("expected the Library of Leng prompt, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choosing the library option succeeds");
    let state = runner.state();
    let top = state
        .players
        .iter()
        .find(|p| p.id == P0)
        .and_then(|p| p.library.front().copied());
    assert_eq!(top, Some(card));
    assert_eq!(zone_of(&runner, card), Zone::Library);
}

/// R7: a parked choice refuses a second debug discard; the graveyard option
/// completes the first.
#[test]
fn r7_parked_choice_refuses_second_debug_discard() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let first = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let second = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((first, second));
    });
    let (first, second) = ids.unwrap();

    runner.act(discard(first)).expect("discard parks a choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(runner.state().stack.is_empty());

    let before = digest(&runner);
    assert!(runner.act(discard(second)).is_err());
    assert_eq!(digest(&runner), before, "a refused action changes nothing");

    // Pre-existing upstream gap: answering a Library of Leng choice for an
    // effect-caused discard moves the card but emits no `Discarded` event, so
    // discard triggers do not fire; no trigger is asserted after the answer.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("choosing the graveyard option succeeds");
    assert_eq!(zone_of(&runner, first), Zone::Graveyard);
}

/// R8: no source object, so "a spell or ability an opponent controls causes you
/// to discard this card" does not apply (deliberate boundary).
#[test]
fn r8_no_source_means_loxodon_smiter_goes_to_graveyard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        card = Some(s.add_real_card(P0, "Loxodon Smiter", Zone::Hand, db));
    });
    let card = card.unwrap();

    let result = runner.act(discard(card)).expect("discard succeeds");

    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
}

/// R9: the discarding player is the card's owner, not the submitting seat.
#[test]
fn r9_discarder_is_owner_not_submitter() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let card = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((grimoire, card));
    });
    let (grimoire, card) = ids.unwrap();
    runner.state_mut().debug_permitted = BTreeSet::from([P0, P1]);

    let result = apply(runner.state_mut(), P1, discard(card)).expect("P1 may submit");

    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    assert_eq!(runner.state().stack.len(), 1, "one Grimoire trigger");
    assert_eq!(runner.state().stack[0].source_id, grimoire);
}

/// R10: rejections leave state unchanged.
#[test]
fn r10_rejections_leave_state_unchanged() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        let hand = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let battlefield = s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        let graveyard = s.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
        let library = s.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
        ids = Some((hand, battlefield, graveyard, library));
    });
    let (hand, battlefield, graveyard, library) = ids.unwrap();

    for (label, id) in [
        ("unknown id", ObjectId(9_999)),
        ("battlefield card", battlefield),
        ("graveyard card", graveyard),
        ("library card", library),
    ] {
        let before = digest(&runner);
        assert!(runner.act(discard(id)).is_err(), "{label} must be rejected");
        assert_eq!(digest(&runner), before, "{label} must change nothing");
    }

    // Debug mode off.
    runner.state_mut().debug_mode = false;
    let before = digest(&runner);
    assert!(runner.act(discard(hand)).is_err());
    assert_eq!(digest(&runner), before);
    runner.state_mut().debug_mode = true;

    // Owner no longer in the game.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .is_eliminated = true;
    // P1 submits, so only the new owner check can reject the action.
    runner.state_mut().debug_permitted = BTreeSet::from([P0, P1]);
    let before = digest(&runner);
    assert!(apply(runner.state_mut(), P1, discard(hand)).is_err());
    assert_eq!(digest(&runner), before);
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .is_eliminated = false;

    // Reach guard: with every gate cleared, the same action is accepted.
    runner.act(discard(hand)).expect("a hand card is accepted");
    assert_eq!(zone_of(&runner, hand), Zone::Graveyard);
}

#[test]
fn r10_non_priority_window_rejects_discard() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        let first = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let second = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((first, second));
    });
    let (first, second) = ids.unwrap();

    runner.act(discard(first)).expect("discard parks a choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let before = digest(&runner);
    assert!(runner.act(discard(second)).is_err());
    assert_eq!(digest(&runner), before);
}
