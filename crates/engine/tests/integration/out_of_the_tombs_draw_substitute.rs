//! Issue #11155: Out of the Tombs' draw substitute must return a creature card
//! from its controller's graveyard before its "if you can't" loss is checked.
//!
//! > If you would draw a card while your library has no cards in it, instead
//! > return a creature card from your graveyard to the battlefield. If you
//! > can't, you lose the game.
//!
//! The replacement compiles to `ChangeZone(Graveyard → Battlefield, creature
//! you own) → LoseTheGame [unless ZoneChangedThisWay]`. The mandatory stash
//! walked past the `ChangeZone` head as if it were a zone-redirect modifier of
//! the draw, so only the loss ran. A proposed draw has no destination for a
//! `ChangeZone` to redirect: when the draw is substituted away (CR 614.6), the
//! whole chain is the substitute.
//!
//! CR 614.6: the replaced draw never happens; the modified event happens
//! instead. CR 614.11: a draw replacement applies even with an empty library.
//! CR 615.5: Swans of Bryn Argoll's prevention follow-up draw is an ordinary
//! draw, so Out of the Tombs replaces it too.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Out of the Tombs (40K #46) Oracle text, verbatim from Scryfall / MTGJSON.
const OUT_OF_THE_TOMBS: &str = "At the beginning of your upkeep, put two eon counters on this \
     enchantment, then mill cards equal to the number of eon counters on it.\n\
     If you would draw a card while your library has no cards in it, instead return a creature \
     card from your graveyard to the battlefield. If you can't, you lose the game.";

struct Board {
    runner: GameRunner,
    tombs: ObjectId,
    island: ObjectId,
    mists: ObjectId,
    /// P1's Soul Warden: an independent witness that a creature entered.
    warden: ObjectId,
}

/// P0 controls Out of the Tombs and an Island, holds Reach Through Mists, and
/// has `library` on top of an otherwise empty library and `graveyard` (real
/// cards) in its graveyard. P1 controls Soul Warden and has a library.
fn board(db: &CardDatabase, library: &[&str], graveyard: &[&str]) -> (Board, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let tombs = scenario
        .add_enchantment_from_oracle(P0, "Out of the Tombs", OUT_OF_THE_TOMBS)
        .id();
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let mists = scenario.add_real_card(P0, "Reach Through Mists", Zone::Hand, db);
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    if !library.is_empty() {
        scenario.with_library_top(P0, library);
    }
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B", "P1 Card C"]);
    let graveyard = graveyard
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Graveyard, db))
        .collect();
    let runner = scenario.build();
    (
        Board {
            runner,
            tombs,
            island,
            mists,
            warden,
        },
        graveyard,
    )
}

fn tombs_replaced_a_draw(events: &[GameEvent], tombs: ObjectId) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::ReplacementApplied { source_id, event_type }
                if *source_id == tombs && event_type == "Draw"
        )
    })
}

fn drew_a_card(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == P0))
}

fn lost(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::PlayerLost { player_id, .. } if *player_id == P0))
}

/// Pass priority while something is on the stack, stopping at the first
/// non-priority prompt (or game over).
fn settle(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..16 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            return;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority resolves the top of the stack")
                .events,
        );
    }
    panic!("the stack never settled");
}

/// Cast Reach Through Mists ("Draw a card.") with the Island and pass priority
/// until it leaves the stack or a non-priority prompt opens.
fn cast_mists(board: &mut Board) -> Vec<GameEvent> {
    let commit = board.runner.cast(board.mists).commit();
    let mut events = commit.events().to_vec();
    assert!(
        board.runner.state().objects[&board.island].tapped,
        "the Island paid Reach Through Mists' {{U}}"
    );
    settle(&mut board.runner, &mut events);
    events
}

fn life(runner: &GameRunner, player: engine::types::player::PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

/// Every return ends with Reach Through Mists resolved, Soul Warden's
/// trigger resolved, an empty stack, and P0 holding priority, alive.
fn assert_returned_without_loss(
    board: &Board,
    events: &[GameEvent],
    hand_before: usize,
    warden_life_before: i32,
) {
    let runner = &board.runner;
    assert!(tombs_replaced_a_draw(events, board.tombs));
    assert!(
        !drew_a_card(events),
        "CR 614.6: the replaced draw never happens"
    );
    assert_eq!(hand_len(runner), hand_before, "no card was drawn");
    assert!(
        !lost(events),
        "a creature was returned, so P0 does not lose"
    );
    assert!(!runner.state().players[P0.0 as usize].is_eliminated);
    assert_eq!(runner.state().objects[&board.mists].zone, Zone::Graveyard);
    assert_eq!(
        life(runner, P1),
        warden_life_before + 1,
        "P1's Soul Warden saw the returned creature enter"
    );
    assert!(runner.state().objects[&board.warden].zone == Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}

/// CR 614.6 + CR 608.2d: with two creature cards in the graveyard, P0 chooses
/// which one returns while the substitute is applied; the other stays.
#[test]
fn empty_library_draw_returns_the_chosen_creature_instead() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut board, graveyard) = board(db, &[], &["Zellix, Sanity Flayer", "Runeclaw Bear"]);
    let [zellix, bear] = graveyard[..] else {
        unreachable!()
    };
    let hand_before = hand_len(&board.runner) - 1;
    let warden_life = life(&board.runner, P1);

    let mut events = cast_mists(&mut board);

    let WaitingFor::EffectZoneChoice {
        player,
        ref cards,
        count,
        zone,
        destination,
        ..
    } = board.runner.state().waiting_for
    else {
        panic!(
            "Out of the Tombs must ask which creature returns, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(count, 1);
    assert_eq!(zone, Zone::Graveyard);
    assert_eq!(destination, Some(Zone::Battlefield));
    let mut offered = cards.clone();
    offered.sort();
    let mut expected = vec![zellix, bear];
    expected.sort();
    assert_eq!(offered, expected, "both creature cards are eligible");
    assert!(tombs_replaced_a_draw(&events, board.tombs));
    assert!(!lost(&events), "no loss before the return is chosen");

    // The prompt lists Runeclaw Bear first; choose the other card.
    events.extend(
        board
            .runner
            .act(GameAction::SelectCards {
                cards: vec![zellix],
            })
            .expect("P0 chooses Zellix")
            .events,
    );
    settle(&mut board.runner, &mut events);

    assert_eq!(
        board.runner.state().objects[&zellix].zone,
        Zone::Battlefield
    );
    assert_eq!(board.runner.state().objects[&bear].zone, Zone::Graveyard);
    assert_returned_without_loss(&board, &events, hand_before, warden_life);
}

/// The sole eligible creature returns; P0 neither draws nor loses.
#[test]
fn empty_library_draw_returns_the_only_creature_instead() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut board, graveyard) = board(db, &[], &["Zellix, Sanity Flayer", "Island"]);
    let [zellix, island_card] = graveyard[..] else {
        unreachable!()
    };
    let hand_before = hand_len(&board.runner) - 1;
    let warden_life = life(&board.runner, P1);

    let events = cast_mists(&mut board);
    // CR 608.2d: one eligible card leaves nothing to choose, so no prompt opens.
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "a single eligible creature returns without a choice prompt, got {:?}",
        board.runner.state().waiting_for
    );

    assert_eq!(
        board.runner.state().objects[&zellix].zone,
        Zone::Battlefield
    );
    assert_eq!(
        board.runner.state().objects[&island_card].zone,
        Zone::Graveyard,
        "a noncreature card is not returned"
    );
    assert_returned_without_loss(&board, &events, hand_before, warden_life);
}

/// "If you can't, you lose the game": with no creature card to return, the
/// replacement still applies (CR 614.11) and P0 loses to Out of the Tombs.
#[test]
fn empty_library_draw_without_a_creature_loses() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut board, graveyard) = board(db, &[], &["Island"]);
    let warden_life = life(&board.runner, P1);

    let events = cast_mists(&mut board);

    assert!(tombs_replaced_a_draw(&events, board.tombs));
    assert!(!drew_a_card(&events));
    assert!(lost(&events), "nothing could be returned, so P0 loses");
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::LoseTheGame, source_id, .. }
            if *source_id == board.tombs
    )));
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(winner) } if winner == P1
    ));
    assert_eq!(life(&board.runner, P1), warden_life, "nothing entered");
    assert!(!events.iter().any(|e| matches!(
        e,
        GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == graveyard[0]
    )));
}

/// The replacement only applies while the library is empty: with a card in
/// it, Reach Through Mists draws normally and the graveyard is untouched.
#[test]
fn nonempty_library_draws_normally() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut board, graveyard) = board(db, &["Library Card"], &["Zellix, Sanity Flayer"]);
    let zellix = graveyard[0];
    let hand_before = hand_len(&board.runner) - 1;

    let events = cast_mists(&mut board);

    assert!(!tombs_replaced_a_draw(&events, board.tombs));
    assert!(drew_a_card(&events));
    assert_eq!(hand_len(&board.runner), hand_before + 1);
    assert_eq!(board.runner.state().objects[&zellix].zone, Zone::Graveyard);
    assert!(!lost(&events));
    assert!(board.runner.state().stack.is_empty());
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}

/// CR 615.5: Swans of Bryn Argoll's prevention follow-up makes the attacking
/// player draw; with an empty library Out of the Tombs replaces that draw too,
/// returning the creature card rather than ending the game.
#[test]
fn swans_prevention_draw_is_replaced_by_the_return() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let tombs = scenario
        .add_enchantment_from_oracle(P0, "Out of the Tombs", OUT_OF_THE_TOMBS)
        .id();
    let hawk = scenario.add_real_card(P0, "Suntail Hawk", Zone::Battlefield, db);
    let swans = scenario.add_real_card(P1, "Swans of Bryn Argoll", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B"]);
    let zellix = scenario.add_real_card(P0, "Zellix, Sanity Flayer", Zone::Graveyard, db);
    let mut runner = scenario.build();
    let warden_life = life(&runner, P1);
    let hand_before = hand_len(&runner);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hawk, AttackTarget::Player(P1))])
        .expect("P0 attacks with Suntail Hawk");
    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            ref other => panic!("expected DeclareBlockers, got {other:?}"),
        }
    }
    runner
        .declare_blockers(&[(swans, hawk)])
        .expect("Swans blocks Suntail Hawk");

    let mut events = Vec::new();
    // Pass out of declare blockers into combat damage, stopping at the first
    // prompt or once the damage step has dealt its damage.
    for _ in 0..16 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || !matches!(
                runner.state().phase,
                Phase::DeclareBlockers | Phase::BeginCombat | Phase::DeclareAttackers
            )
        {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).expect("pass").events);
    }
    assert_eq!(runner.state().phase, Phase::CombatDamage);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the sole eligible creature returns without a choice prompt, got {:?}",
        runner.state().waiting_for
    );
    settle(&mut runner, &mut events);

    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::ReplacementApplied { source_id, event_type }
            if *source_id == swans && event_type == "DamageDone"
    )));
    assert!(tombs_replaced_a_draw(&events, tombs));
    assert!(!drew_a_card(&events));
    assert_eq!(hand_len(&runner), hand_before);
    assert!(!lost(&events));
    assert_eq!(runner.state().objects[&zellix].zone, Zone::Battlefield);
    assert_eq!(life(&runner, P1), warden_life + 1);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
