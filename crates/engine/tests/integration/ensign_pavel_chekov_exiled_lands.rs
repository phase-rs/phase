//! Ensign Pavel Chekov — "Strange new worlds — When Ensign Chekov enters, each
//! opponent exiles the top X cards of their library, where X is the number of
//! differently named lands you control. For each opponent, choose a land card
//! that player owns exiled this way. You may play the chosen cards for as long
//! as they remain exiled." (Oracle text verified against
//! `client/public/card-data.json`.)
//!
//! Covers the building blocks the card composes, end to end through the
//! production cast → ETB trigger → resolution path:
//!   * CR 201.2b + CR 608.2c: X counts differently named lands, so duplicates
//!     count once.
//!   * CR 101.4 + CR 608.2c + CR 608.2d: the per-opponent choice reads only the
//!     cards that opponent owns among the cards exiled this way, and each
//!     player's pool stays pinned to that set while earlier picks rebind the
//!     chain's tracked set.
//!   * CR 305.1 + CR 305.2 + CR 701.18b: "play" covers playing a land, which
//!     uses the land drop; the permission lasts across turns while the card
//!     remains exiled and ends when it leaves exile (CR 400.7).

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::{WaitingFor, ZoneOpponentChooserPurpose};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const CHEKOV_NAME: &str = "Ensign Pavel Chekov";

/// Ensign Pavel Chekov, verbatim (`client/public/card-data.json`).
const CHEKOV: &str = "Strange new worlds — When Ensign Chekov enters, each opponent exiles the \
top X cards of their library, where X is the number of differently named lands you control. For \
each opponent, choose a land card that player owns exiled this way. You may play the chosen cards \
for as long as they remain exiled.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn can_play_land(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == id))
}

/// Pass priority until a non-priority prompt opens or the stack is empty.
fn settle(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("stack did not settle: {:?}", runner.state().waiting_for);
}

/// Answer the controller's per-opponent pool prompt with `pick`, asserting the
/// offered pool is exactly `expected_pool`.
fn pick_from_pool(runner: &mut GameRunner, expected_pool: &[ObjectId], pick: ObjectId) {
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the per-opponent pool prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "Chekov's controller makes every choice");
    assert_eq!(count, 1, "one land card per opponent");
    let mut offered = cards.clone();
    offered.sort();
    let mut expected = expected_pool.to_vec();
    expected.sort();
    assert_eq!(
        offered, expected,
        "the pool is that opponent's exiled land cards"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![pick] })
        .expect("a pooled land card is a legal pick");
}

/// Advance through turns until P0's next precombat main phase with priority.
fn advance_to_controllers_next_main(runner: &mut GameRunner) {
    let start_turn = runner.state().turn_number;
    for _ in 0..2000 {
        let state = runner.state();
        if state.turn_number > start_turn
            && state.active_player == P0
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            return;
        }
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected prompt while passing turns: {other:?}"),
        };
        runner.act(action).expect("turn passes");
    }
    panic!("did not reach the controller's next main phase");
}

struct Staged {
    runner: GameRunner,
    p1_land: ObjectId,
    p1_spell: ObjectId,
    p2_land_a: ObjectId,
    p2_land_b: ObjectId,
    p2_unexiled_land: ObjectId,
}

/// Three players. P0 controls Forest, Forest and Island (X = 2 — the duplicate
/// Forest counts once). P1's top two cards are a land and a nonland card; P2's
/// top two are both lands, with a third land beneath them that X does not
/// reach. Casts Chekov and resolves its ETB trigger up to the first choice.
fn cast_chekov() -> Staged {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Blue);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Filler 1", "Filler 2", "Filler 3", "Filler 4"]);
    }
    let p1_spell = scenario
        .add_spell_to_library_top(P1, "P1 Sorcery", false)
        .id();
    let p1_land = scenario.add_land_to_library_top(P1, "P1 Land").id();
    let p2_unexiled_land = scenario.add_land_to_library_top(P2, "P2 Deep Land").id();
    let p2_land_b = scenario.add_land_to_library_top(P2, "P2 Land B").id();
    let p2_land_a = scenario.add_land_to_library_top(P2, "P2 Land A").id();
    let chekov = scenario
        .add_creature_to_hand_from_oracle(P0, CHEKOV_NAME, 2, 2, CHEKOV)
        .id();
    let mut runner = scenario.build();

    runner.cast(chekov).resolve();
    settle(&mut runner);
    assert_eq!(zone_of(&runner, chekov), Zone::Battlefield);

    Staged {
        runner,
        p1_land,
        p1_spell,
        p2_land_a,
        p2_land_b,
        p2_unexiled_land,
    }
}

/// CR 201.2b + CR 101.4c + CR 608.2d + CR 305.1: each opponent exiles exactly
/// two cards; the controller orders the per-opponent choices, picks one land
/// per opponent from that opponent's own exiled cards, and may play exactly the
/// chosen cards.
#[test]
fn chekov_chooses_one_exiled_land_per_opponent_and_may_play_only_those() {
    let Staged {
        mut runner,
        p1_land,
        p1_spell,
        p2_land_a,
        p2_land_b,
        p2_unexiled_land,
    } = cast_chekov();

    // CR 201.2b: two differently named lands → the top two cards of each
    // opponent's library are exiled, and the third card of P2's is not.
    for exiled in [p1_land, p1_spell, p2_land_a, p2_land_b] {
        assert_eq!(zone_of(&runner, exiled), Zone::Exile, "{exiled:?}");
    }
    assert_eq!(zone_of(&runner, p2_unexiled_land), Zone::Library);

    // CR 101.4c: one player makes both choices, so that player orders them.
    // Choose P2 first (not APNAP) so P1's pool is read after P2's pick has
    // rebound the chain's tracked set.
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            player,
            candidates,
            purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
            ..
        } => {
            assert_eq!(*player, P0);
            let mut offered = candidates.clone();
            offered.sort();
            assert_eq!(offered, vec![P1, P2], "each opponent is chosen for");
        }
        other => panic!("expected the controller's order prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P2 })
        .expect("ordering P2 first is legal");
    pick_from_pool(&mut runner, &[p2_land_a, p2_land_b], p2_land_b);
    // CR 608.2c: P1's pool is still the cards exiled this way that P1 owns —
    // its land card only (the sorcery is not a land card).
    pick_from_pool(&mut runner, &[p1_land], p1_land);
    settle(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the trigger finished resolving: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());

    // CR 305.1: the chosen lands may be played from exile; the unchosen land
    // and the nonland card may not.
    assert!(can_play_land(&runner, p1_land), "P1's chosen land");
    assert!(can_play_land(&runner, p2_land_b), "P2's chosen land");
    assert!(!can_play_land(&runner, p2_land_a), "P2's unchosen land");

    // CR 305.2: playing one uses the turn's land drop, so the other waits.
    runner
        .act(GameAction::PlayLand {
            object_id: p2_land_b,
            card_id: runner.state().objects[&p2_land_b].card_id,
        })
        .expect("play the chosen land from exile");
    assert_eq!(zone_of(&runner, p2_land_b), Zone::Battlefield);
    assert_eq!(runner.state().objects[&p2_land_b].controller, P0);
    assert!(
        !can_play_land(&runner, p1_land),
        "CR 305.2: the land drop is spent this turn"
    );

    // CR 611.2a: the permission lasts "for as long as they remain exiled", so
    // the other chosen land is still playable on the controller's next turn.
    advance_to_controllers_next_main(&mut runner);
    assert_eq!(zone_of(&runner, p1_land), Zone::Exile);
    assert!(
        can_play_land(&runner, p1_land),
        "the remaining chosen land is playable on a later turn"
    );
    assert!(
        !can_play_land(&runner, p2_land_a),
        "never the unchosen land"
    );
    runner
        .act(GameAction::PlayLand {
            object_id: p1_land,
            card_id: runner.state().objects[&p1_land].card_id,
        })
        .expect("play the remaining chosen land");
    assert_eq!(zone_of(&runner, p1_land), Zone::Battlefield);
}

/// CR 400.7 + CR 611.2a: a chosen card that leaves exile is a new object; when
/// it is exiled again the permission does not come back.
#[test]
fn chekov_permission_ends_when_the_chosen_card_leaves_exile() {
    let Staged {
        mut runner,
        p1_land,
        p2_land_a,
        ..
    } = cast_chekov();
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P1 })
        .expect("ordering P1 first is legal");
    pick_from_pool(&mut runner, &[p1_land], p1_land);
    let p2_pool = match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => cards.clone(),
        other => panic!("expected P2's pool prompt, got {other:?}"),
    };
    pick_from_pool(&mut runner, &p2_pool, p2_land_a);
    settle(&mut runner);
    assert!(can_play_land(&runner, p1_land));

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), p1_land, Zone::Graveyard, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), p1_land, Zone::Exile, &mut events);
    assert_eq!(zone_of(&runner, p1_land), Zone::Exile);
    assert!(
        !can_play_land(&runner, p1_land),
        "the re-exiled card is a new object without the permission"
    );
    assert!(
        can_play_land(&runner, p2_land_a),
        "the other grant is untouched"
    );
}
