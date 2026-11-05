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
use engine::types::game_state::{ActionResult, CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::log::LogSegment;
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

/// Index of the replacement candidate whose description satisfies `pick`,
/// asserting that `player` is the one being asked.
fn choice_index(
    runner: &GameRunner,
    player: PlayerId,
    what: &str,
    pick: impl Fn(&str) -> bool,
) -> usize {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player: chooser,
            candidates,
            ..
        } => {
            assert_eq!(*chooser, player, "{what}: wrong chooser");
            candidates
                .iter()
                .position(|candidate| pick(&candidate.description))
                .unwrap_or_else(|| panic!("{what}: no matching candidate in {candidates:?}"))
        }
        other => panic!("expected the {what}, got {other:?}"),
    }
}

fn is_priority(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

/// What a rejected action must leave exactly as it was: every zone's contents,
/// life totals, the prompt, the stack and the state revision. The whole
/// `GameState` is not compared, because the action boundary also records the
/// rejected attempt itself (the same rollback-set approach as `engine_debug`'s
/// rejection tests).
fn digest(runner: &GameRunner) -> serde_json::Value {
    let state = runner.state();
    let players: Vec<_> = state
        .players
        .iter()
        .map(|p| (p.life, &p.hand, &p.library, &p.graveyard))
        .collect();
    serde_json::json!({
        "players": players,
        "battlefield": &state.battlefield,
        "exile": &state.exile,
        "stack_len": state.stack.len(),
        "waiting_for": &state.waiting_for,
        "state_revision": state.state_revision,
    })
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

/// R6: the cause is Effect, so Library of Leng's replacement applies. The
/// discard is recorded once, at the answer (not when the prompt is parked).
#[test]
fn r6_library_of_leng_applies_to_sandbox_discard() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db));
    });
    let card = card.unwrap();

    let first = runner.act(discard(card)).expect("discard succeeds");
    assert!(
        discarded_events(&first.events).is_empty(),
        "parking the prompt records no discard"
    );
    let accept = choice_index(&runner, P0, "Library of Leng prompt", |d| d != "Decline");

    let result = runner
        .act(GameAction::ChooseReplacement { index: accept })
        .expect("choosing the library option succeeds");
    assert_eq!(discarded_events(&result.events), vec![(P0, card)]);
    let state = runner.state();
    assert_eq!(
        state
            .cards_discarded_this_turn_by_player
            .get(&P0)
            .copied()
            .unwrap_or(0),
        1
    );
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
        let grimoire = s.add_real_card(P1, "Geth's Grimoire", Zone::Battlefield, db);
        let first = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        let second = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
        ids = Some((first, second, grimoire));
    });
    let (first, second, grimoire) = ids.unwrap();

    let parked = runner.act(discard(first)).expect("discard parks a choice");
    assert!(
        discarded_events(&parked.events).is_empty(),
        "parking the prompt records no discard"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(runner.state().stack.is_empty());

    let before = digest(&runner);
    assert!(runner.act(discard(second)).is_err());
    assert_eq!(digest(&runner), before, "a refused action changes nothing");

    let decline = choice_index(&runner, P0, "Library of Leng prompt", |d| d == "Decline");
    let result = runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("choosing the graveyard option succeeds");
    assert_eq!(zone_of(&runner, first), Zone::Graveyard);
    assert_eq!(discarded_events(&result.events), vec![(P0, first)]);
    assert_eq!(
        runner
            .state()
            .cards_discarded_this_turn_by_player
            .get(&P0)
            .copied()
            .unwrap_or(0),
        1
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|e| e.source_id == grimoire)
            .count(),
        1,
        "the opponent's Grimoire triggers exactly once"
    );
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

/// Viewers that are not the discarding seat: the opponent and an unseated
/// spectator (`filter_events_for_viewer` treats an id with no seat as one).
const SPECTATOR: PlayerId = PlayerId(99);

/// CR 400.2: the distinctive hand card for the privacy rows.
const SECRET_NAME: &str = "Grizzly Bears";

/// CR 400.2 + CR 701.9c: a hand card discarded into a hidden zone unrevealed
/// must not be named anywhere in the public stream. For the opponent and a
/// spectator, the whole filtered event stream and the public audit text
/// (`log_entries`, as `apply` produced them) of every result must be free of
/// the card's name, while the `DebugActionUsed` event is still delivered.
fn assert_no_card_identity(runner: &GameRunner, results: &[&ActionResult]) {
    let events: Vec<GameEvent> = results
        .iter()
        .flat_map(|result| result.events.iter().cloned())
        .collect();
    for viewer in [P1, SPECTATOR] {
        let filtered =
            engine::game::visibility::filter_events_for_viewer(&events, runner.state(), viewer);
        assert!(
            filtered
                .iter()
                .any(|event| matches!(event, GameEvent::DebugActionUsed { .. })),
            "viewer {viewer:?} must receive the DebugActionUsed event"
        );
        let wire = serde_json::to_string(&filtered).expect("serialize filtered events");
        assert!(
            !wire.contains(SECRET_NAME),
            "viewer {viewer:?}: filtered events name the hand card: {wire}"
        );
    }
    let entries: Vec<_> = results
        .iter()
        .flat_map(|result| result.log_entries.iter())
        .collect();
    assert!(!entries.is_empty(), "the action must be logged");
    let rendered = serde_json::to_string(&entries).expect("serialize log entries");
    assert!(
        !rendered.contains(SECRET_NAME),
        "public audit text names the hand card: {rendered}"
    );
}

/// P0's engine-projected `debug_discardable_hand_cards`, read through the
/// public filtered-view path the client receives.
fn projected_discardable(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    let filtered = engine::game::visibility::filter_state_for_viewer(state, P0);
    engine::game::derived_views::derive_filtered_views(state, &filtered, Some(P0))
        .debug_discardable_hand_cards
}

/// R11 (CR 400.2): a parked Library of Leng prompt has not revealed the hand
/// card, so the broadcast debug description must not name it.
#[test]
fn r11_parked_discard_description_does_not_name_the_hand_card() {
    let Some(db) = load_db() else { return };
    let mut ids = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        let card = s.add_real_card(P0, SECRET_NAME, Zone::Hand, db);
        let other = s.add_real_card(P0, "Shock", Zone::Hand, db);
        ids = Some((card, other));
    });
    let (card, other) = ids.unwrap();
    assert_eq!(
        projected_discardable(&runner),
        vec![card, other],
        "both hand cards are discardable at a settled Priority window"
    );

    let parked = runner.act(discard(card)).expect("discard parks a choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));

    assert_eq!(zone_of(&runner, other), Zone::Hand);
    assert!(
        projected_discardable(&runner).is_empty(),
        "the parked Leng choice withdraws discard availability for the other hand card"
    );

    assert_no_card_identity(&runner, &[&parked]);
}

/// R12 (CR 400.2): after the Leng answer puts the card on the library
/// unrevealed, the `DebugActionUsed` events and their public log entries still
/// do not name it. The whole-stream check of R11 is not asserted after the
/// library answer because the Leng replacement currently routes the card
/// through the graveyard before the library (pre-existing; reported separately).
#[test]
fn r12_library_answer_discard_description_does_not_name_the_hand_card() {
    let Some(db) = load_db() else { return };
    let mut card = None;
    let mut runner = table(db, |s, db| {
        s.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
        card = Some(s.add_real_card(P0, SECRET_NAME, Zone::Hand, db));
    });
    let card = card.unwrap();

    let parked = runner.act(discard(card)).expect("discard parks a choice");
    let accept = choice_index(&runner, P0, "Library of Leng prompt", |d| d != "Decline");
    let answered = runner
        .act(GameAction::ChooseReplacement { index: accept })
        .expect("choosing the library option succeeds");
    assert_eq!(zone_of(&runner, card), Zone::Library);

    let events: Vec<GameEvent> = [&parked, &answered]
        .iter()
        .flat_map(|result| result.events.iter().cloned())
        .collect();
    for viewer in [P1, SPECTATOR] {
        let debug_events: Vec<GameEvent> =
            engine::game::visibility::filter_events_for_viewer(&events, runner.state(), viewer)
                .into_iter()
                .filter(|event| matches!(event, GameEvent::DebugActionUsed { .. }))
                .collect();
        assert!(
            !debug_events.is_empty(),
            "viewer {viewer:?} must receive the DebugActionUsed event"
        );
        let wire = serde_json::to_string(&debug_events).expect("serialize debug events");
        assert!(
            !wire.contains(SECRET_NAME),
            "viewer {viewer:?}: DebugActionUsed names the hand card: {wire}"
        );
    }

    let debug_entries: Vec<_> = [&parked, &answered]
        .iter()
        .flat_map(|result| result.log_entries.iter())
        .filter(|entry| {
            entry
                .segments
                .iter()
                .any(|segment| matches!(segment, LogSegment::Text(t) if t.contains("used debug")))
        })
        .collect();
    assert!(
        !debug_entries.is_empty(),
        "the debug action must be logged publicly"
    );
    let rendered = serde_json::to_string(&debug_entries).expect("serialize log entries");
    assert!(
        !rendered.contains(SECRET_NAME),
        "public debug log entry names the hand card: {rendered}"
    );
}
