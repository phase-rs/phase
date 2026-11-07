//! Library of Leng sends a batch of discarded cards straight to the library
//! (CR 614.6 + CR 701.9c): nothing passes through the graveyard, no viewer
//! other than the owner learns the cards' names or ids from the discard, and Rest in
//! Peace's graveyard replacement has nothing to apply to.
//!
//! Verbatim Oracle text, as the committed fixture stores it:
//!   Mind Rot: "Target player discards two cards."
//!   Library of Leng: "You have no maximum hand size. / If an effect causes you
//!     to discard a card, discard it, but you may put it on top of your library
//!     instead of into your graveyard."
//!   Telepathy: "Your opponents play with their hands revealed."
//!   Rest in Peace: "When this enchantment enters, exile all graveyards. / If a
//!     card or token would be put into a graveyard from anywhere, exile it
//!     instead."
//!
//! Board: P0 controls Library of Leng and holds Grizzly Bears, Shock and Swamp
//! (three cards, so the two-card discard reaches `DiscardChoice`). P1 holds
//! Mind Rot with {2}{B} floating and casts it at P0 on P1's own turn.

use std::collections::BTreeSet;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ActionResult, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::log::LogSegment;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::{identifying_object_ids, shared_card_db as load_db, viewer_event_ids};

const BEARS: &str = "Grizzly Bears";
const SHOCK: &str = "Shock";
const SPECTATOR: PlayerId = PlayerId(99);

struct Board {
    runner: GameRunner,
    bears: ObjectId,
    shock: ObjectId,
    mind_rot: ObjectId,
}

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

/// The shared board plus any extra P1 permanents.
fn board(db: &CardDatabase, p1_permanents: &[&str]) -> Board {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in [P0, P1] {
        for _ in 0..4 {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    scenario.add_real_card(P0, "Library of Leng", Zone::Battlefield, db);
    for name in p1_permanents {
        scenario.add_real_card(P1, name, Zone::Battlefield, db);
    }
    let bears = scenario.add_real_card(P0, BEARS, Zone::Hand, db);
    let shock = scenario.add_real_card(P0, SHOCK, Zone::Hand, db);
    scenario.add_real_card(P0, "Swamp", Zone::Hand, db);
    let mind_rot = scenario.add_real_card(P1, "Mind Rot", Zone::Hand, db);
    scenario.with_mana_pool(
        P1,
        floating_mana(2, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(1, ManaType::Black))
            .collect(),
    );
    let mut runner = scenario.build();
    // P1's own main phase, so P1 holds priority to cast the sorcery.
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    Board {
        runner,
        bears,
        shock,
        mind_rot,
    }
}

/// Cast Mind Rot at P0, resolve it to P0's `DiscardChoice`, and select Bears and
/// Shock. Returns every `ActionResult` of the discard.
fn cast_and_select(board: &mut Board) -> Vec<ActionResult> {
    let runner = &mut board.runner;
    runner.cast(board.mind_rot).target_player(P0).commit();
    let mut results = Vec::new();
    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        results.push(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority resolves Mind Rot"),
        );
    }
    match &runner.state().waiting_for {
        WaitingFor::DiscardChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(cards.len(), 3, "P0 chooses two of three cards");
        }
        other => panic!("expected P0's discard choice, got {other:?}"),
    }
    results.push(
        runner
            .act(GameAction::SelectCards {
                cards: vec![board.bears, board.shock],
            })
            .expect("selecting the two discards"),
    );
    results
}

/// Answer every Library of Leng prompt (accept, or decline) and return the
/// results and the number of prompts answered.
fn answer_leng(runner: &mut GameRunner, accept: bool) -> (Vec<ActionResult>, usize) {
    let mut results = Vec::new();
    while let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    {
        assert_eq!(player, P0, "Leng's controller answers");
        let index = candidates
            .iter()
            .position(|candidate| (candidate.description != "Decline") == accept)
            .expect("Library of Leng offers both polarities");
        results.push(
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("answering Library of Leng"),
        );
        assert!(results.len() <= 4, "replacement prompts must terminate");
    }
    let answered = results.len();
    (results, answered)
}

fn refs(results: &[ActionResult]) -> Vec<&ActionResult> {
    results.iter().collect()
}

fn events_of<'a>(results: &[&'a ActionResult]) -> Vec<&'a GameEvent> {
    results.iter().flat_map(|r| r.events.iter()).collect()
}

fn discarded_count(results: &[&ActionResult]) -> usize {
    events_of(results)
        .iter()
        .filter(|event| matches!(event, GameEvent::Discarded { .. }))
        .count()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// CR 400.2 + CR 701.9c: for P1 and a spectator, the whole filtered event
/// stream and the public log of `results` contain none of `names` or `ids`.
/// Reach guard: each viewer's filtered stream still carries every `Discarded`
/// event, each with the placeholder `ObjectId(0)`. The owner keeps the real ids.
fn assert_no_hand_identities(
    runner: &GameRunner,
    results: &[&ActionResult],
    names: &[&str],
    ids: &[ObjectId],
) {
    let events: Vec<GameEvent> = events_of(results).into_iter().cloned().collect();
    let discards = discarded_count(results);
    for viewer in [P1, SPECTATOR] {
        let filtered =
            engine::game::visibility::filter_events_for_viewer(&events, runner.state(), viewer);
        let seen: Vec<ObjectId> = filtered
            .iter()
            .filter_map(|event| match event {
                GameEvent::Discarded { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            seen.len(),
            discards,
            "viewer {viewer:?} receives every Discarded"
        );
        assert!(
            seen.iter().all(|id| *id == ObjectId(0)),
            "viewer {viewer:?}: Discarded carries a real id: {seen:?}"
        );
        let wire = serde_json::to_string(&filtered).expect("serialize filtered events");
        for name in names {
            assert!(
                !wire.contains(name),
                "viewer {viewer:?}: filtered events name {name}: {wire}"
            );
        }
        let viewer_ids = viewer_event_ids(&events, runner.state(), viewer);
        for id in ids {
            assert!(
                !viewer_ids.contains(&id.0),
                "viewer {viewer:?}: filtered events carry the id of {id:?}"
            );
        }
    }
    let entries: Vec<_> = results.iter().flat_map(|r| r.log_entries.iter()).collect();
    let rendered = serde_json::to_string(&entries).expect("serialize log entries");
    for name in names {
        assert!(
            !rendered.contains(name),
            "public log names {name}: {rendered}"
        );
    }
    let log_ids = identifying_object_ids(&serde_json::to_value(&entries).expect("log value"));
    for id in ids {
        assert!(
            !log_ids.contains(&id.0),
            "public log carries the id of {id:?}"
        );
    }

    // Owner positive: P0's filtered `Discarded` events equal the authoritative ones.
    let authoritative: BTreeSet<(PlayerId, ObjectId)> = discarded_pairs(&events);
    let owner = engine::game::visibility::filter_events_for_viewer(&events, runner.state(), P0);
    assert_eq!(discarded_pairs(&owner), authoritative);
    let expected: BTreeSet<(PlayerId, ObjectId)> = ids.iter().map(|id| (P0, *id)).collect();
    assert_eq!(authoritative, expected, "reach: every discard is P0's");
    let owner_ids = viewer_event_ids(&events, runner.state(), P0);
    assert!(ids.iter().all(|id| owner_ids.contains(&id.0)));
}

fn discarded_pairs(events: &[GameEvent]) -> BTreeSet<(PlayerId, ObjectId)> {
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

fn concealed_discard_lines(results: &[&ActionResult]) -> usize {
    results
        .iter()
        .flat_map(|r| r.log_entries.iter())
        .filter(|entry| {
            matches!(
                entry.segments.as_slice(),
                [LogSegment::PlayerName { player_id, .. }, LogSegment::Text(text)]
                    if *player_id == P0 && text == " discards a card"
            )
        })
        .count()
}

fn named_discard_lines(results: &[&ActionResult], card: ObjectId) -> usize {
    results
        .iter()
        .flat_map(|r| r.log_entries.iter())
        .filter(|entry| {
            entry
                .segments
                .iter()
                .any(|s| matches!(s, LogSegment::Text(text) if text == " discards "))
                && entry.segments.iter().any(
                    |s| matches!(s, LogSegment::CardName { object_id, .. } if *object_id == card),
                )
        })
        .count()
}

/// CR 614.6 + CR 701.9c: no graveyard or exile stop for either card, no
/// follow-up placement effect, and one hand -> library move per card.
fn assert_direct_library_moves(results: &[&ActionResult], cards: &[ObjectId]) {
    let events = events_of(results);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                ..
            }
        )),
        "no follow-up library placement effect resolves"
    );
    for card in cards {
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, to: Zone::Graveyard | Zone::Exile, .. }
                    if object_id == card
            )),
            "{card:?} never stops in a public zone"
        );
        let direct = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        object_id,
                        from: Some(Zone::Hand),
                        to: Zone::Library,
                        ..
                    } if object_id == card
                )
            })
            .count();
        assert_eq!(direct, 1, "{card:?}: exactly one hand -> library move");
    }
}

fn library_top_set(runner: &GameRunner) -> BTreeSet<ObjectId> {
    runner.state().players[0]
        .library
        .iter()
        .take(2)
        .copied()
        .collect()
}

/// Run the accept-both flow and return every result of the discard.
fn accept_both(board: &mut Board) -> Vec<ActionResult> {
    let mut results = cast_and_select(board);
    let (answers, prompts) = answer_leng(&mut board.runner, true);
    assert_eq!(prompts, 2, "one Library of Leng prompt per discarded card");
    results.extend(answers);
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ) && board.runner.state().stack.is_empty(),
        "the discard completes: {:?}",
        board.runner.state().waiting_for
    );
    results
}

fn assert_accepted_batch(board: &Board, results: &[ActionResult]) {
    let all = refs(results);
    let cards = [board.bears, board.shock];
    assert_eq!(discarded_count(&all), 2);
    assert_direct_library_moves(&all, &cards);
    for card in cards {
        assert_eq!(zone_of(&board.runner, card), Zone::Library);
    }
    assert_eq!(
        library_top_set(&board.runner),
        BTreeSet::from(cards),
        "both discards are on top of P0's library (order is the player's)"
    );
    assert_eq!(
        board.runner.state().players[0].hand.len(),
        1,
        "the hand is reduced by two"
    );
    assert_no_hand_identities(&board.runner, &all, &[BEARS, SHOCK], &cards);
    assert_eq!(
        concealed_discard_lines(&all),
        2,
        "two `discards a card` lines"
    );
}

/// M1 (CR 614.6 + CR 701.9c): a two-card batch discard answered with Library of
/// Leng's library option moves both cards hand -> library and names neither.
#[test]
fn m1_batch_discard_into_the_library_is_direct_and_unnamed() {
    let Some(db) = load_db() else { return };
    let mut board = board(db, &[]);
    let results = accept_both(&mut board);
    assert_accepted_batch(&board, &results);
}

/// M1 decline sibling: declining both prompts sends both cards to the
/// graveyard, a public destination, and the log names them.
#[test]
fn m1_declined_batch_discard_goes_to_the_graveyard_and_is_named() {
    let Some(db) = load_db() else { return };
    let mut board = board(db, &[]);
    let mut results = cast_and_select(&mut board);
    let (answers, prompts) = answer_leng(&mut board.runner, false);
    assert_eq!(prompts, 2);
    results.extend(answers);

    let all = refs(&results);
    assert_eq!(discarded_count(&all), 2);
    for card in [board.bears, board.shock] {
        assert_eq!(zone_of(&board.runner, card), Zone::Graveyard);
        assert_eq!(named_discard_lines(&all, card), 1, "{card:?} is named");
    }
    assert_eq!(concealed_discard_lines(&all), 0);

    // Public destination: the real ids stay on the wire for P1 and a spectator.
    let events: Vec<GameEvent> = events_of(&all).into_iter().cloned().collect();
    let expected = BTreeSet::from([(P0, board.bears), (P0, board.shock)]);
    for viewer in [P1, SPECTATOR] {
        let filtered = engine::game::visibility::filter_events_for_viewer(
            &events,
            board.runner.state(),
            viewer,
        );
        assert_eq!(
            discarded_pairs(&filtered),
            expected,
            "viewer {viewer:?}: a graveyard discard keeps its ids"
        );
    }
    // Positive control for the log walk: it finds the named cards' ids.
    let entries: Vec<_> = all.iter().flat_map(|r| r.log_entries.iter()).collect();
    let log_ids = identifying_object_ids(&serde_json::to_value(&entries).expect("log value"));
    assert!(log_ids.contains(&board.bears.0) && log_ids.contains(&board.shock.0));
}

/// M2: the same batch with Telepathy revealing P0's hand to P1. The log is one
/// global stream, so the discard still names neither card.
#[test]
fn m2_currently_revealed_batch_discard_into_the_library_is_unnamed() {
    let Some(db) = load_db() else { return };
    let mut board = board(db, &["Telepathy"]);

    // Reach guard: Telepathy's reveal is live, so P1 sees both names in hand.
    engine::game::derived::sync_continuous_reveals(board.runner.state_mut());
    let p1_view = engine::game::visibility::filter_state_for_viewer(board.runner.state(), P1);
    assert_eq!(p1_view.objects[&board.bears].name, BEARS);
    assert_eq!(p1_view.objects[&board.shock].name, SHOCK);

    let results = accept_both(&mut board);
    assert_accepted_batch(&board, &results);
}

/// M3 (CR 614.6): with Rest in Peace on the battlefield, nothing goes to a
/// graveyard, so its replacement does not apply: neither card is exiled.
#[test]
fn m3_rest_in_peace_does_not_touch_cards_that_never_reach_a_graveyard() {
    let Some(db) = load_db() else { return };
    let mut board = board(db, &["Rest in Peace"]);
    let results = accept_both(&mut board);
    assert_accepted_batch(&board, &results);
}
