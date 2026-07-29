//! Runtime pipeline coverage for Notion Thief's draw redirection.
//!
//! Notion Thief:
//!   "If an opponent would draw a card except the first one they draw in each
//!    of their draw steps, instead that player skips that draw and you draw a
//!    card."
//!
//! CR 614.1a: the "instead" makes this a replacement effect. CR 614.6: the
//! replaced draw never happens — the opponent does NOT draw. CR 121.1 + CR
//! 504.1: the exception is the active player's first draw of their own draw
//! step (the turn-based draw), which Notion Thief must leave alone.
//!
//! The card's parsed `execute` chain is a two-link ability: the head is an
//! `Effect::Unimplemented { name: "draw" }` standing for the "that player skips
//! that draw" clause, and `sub_ability` is `Effect::Draw { count: Fixed(1),
//! target: Controller }` for "and you draw a card". The head being non-`Draw`
//! is what makes `draw_is_substituted_away` (`game/replacement.rs`) zero the
//! opponent's draw count, so the head is load-bearing for the suppression half.
//!
//! The parser-level test in `oracle_replacement.rs` asserts only that
//! `execute.is_some()` — true of any non-empty chain, including one whose
//! no-op head never reaches the `sub_ability`. These tests drive the real draw
//! pipeline through `GameAction`s to prove BOTH halves at runtime:
//!
//!   1. Control (no Notion Thief): the opponent's draw proceeds normally, so
//!      the fixture demonstrably reaches a real draw. Without this the deltas
//!      in (2) could be produced by a draw that never happened at all.
//!   2. With Notion Thief: the opponent's hand and library are UNCHANGED (their
//!      draw was skipped) and the Notion Thief controller's hand is +1, taken
//!      from the controller's own library. Fails if the chain stops at the
//!      no-op `Unimplemented` head and never reaches `sub_ability` — the
//!      opponent would lose the draw and the controller would gain nothing.
//!   3. The draw-step exception: the active player's FIRST draw in their own
//!      draw step is untouched, and the SECOND draw in that same step is
//!      redirected. This proves (2) is not sitting in the excluded regime and
//!      that the `ExceptFirstDrawInDrawStep` gate discriminates in both
//!      directions.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// `P0`'s library, top-first. `P0` is the Notion Thief controller, so a
/// redirected draw must pull `P0`'s own top card (CR 121.1).
const P0_LIBRARY: [&str; 4] = ["Sol Ring", "Hill Giant", "Plains", "Forest"];
/// `P1`'s library, top-first. A skipped draw must leave all of these in place.
const P1_LIBRARY: [&str; 4] = ["Grizzly Bears", "Mountain", "Island", "Swamp"];

/// Build a two-player board with `P0` optionally controlling Notion Thief, and
/// deterministic libraries for both players. `add_real_card` `push_back`s and
/// the engine treats `library.front()` as the top, so `P0_LIBRARY[0]` /
/// `P1_LIBRARY[0]` are the respective top cards.
fn scenario(db: &CardDatabase, with_notion_thief: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_notion_thief {
        scenario.add_real_card(P0, "Notion Thief", Zone::Battlefield, db);
    }
    for name in P0_LIBRARY {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    for name in P1_LIBRARY {
        scenario.add_real_card(P1, name, Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;
    runner
}

fn zone_names(state: &GameState, player: PlayerId, zone: Zone) -> Vec<String> {
    let player_state = &state.players[player.0 as usize];
    let ids = match zone {
        Zone::Hand => &player_state.hand,
        Zone::Library => &player_state.library,
        other => panic!("zone_names only serves Hand/Library; got {other:?}"),
    };
    ids.iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect()
}

/// The Notion Thief on the battlefield.
fn notion_thief_id(state: &GameState) -> engine::types::identifiers::ObjectId {
    state
        .battlefield
        .iter()
        .copied()
        .find(|id| state.objects[id].name == "Notion Thief")
        .expect("Notion Thief must be on the battlefield")
}

fn draw_one(runner: &mut GameRunner, player: PlayerId) {
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: player,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();
}

/// Non-vacuity control: with no Notion Thief on the battlefield, `P1`'s draw in
/// `P0`'s main phase proceeds normally (CR 121.1) and `P0` draws nothing. This
/// pins the baseline the redirect test measures against — if the fixture could
/// not reach a real draw at all, this test fails first.
#[test]
fn without_notion_thief_opponent_draw_proceeds_normally() {
    let Some(db) = load_db() else {
        return;
    };
    let mut runner = scenario(db, false);

    draw_one(&mut runner, P1);

    assert_eq!(
        zone_names(runner.state(), P1, Zone::Hand),
        vec!["Grizzly Bears".to_string()],
        "control: P1 draws their own top card"
    );
    assert!(
        zone_names(runner.state(), P0, Zone::Hand).is_empty(),
        "control: P0 draws nothing; got {:?}",
        zone_names(runner.state(), P0, Zone::Hand)
    );
}

/// CR 614.1a + CR 614.6: an opponent's draw outside their draw step is replaced
/// — that player skips the draw (hand AND library unchanged) and the Notion
/// Thief controller draws a card from their own library instead.
///
/// This is the load-bearing test. The suppression half (P1 +0) and the
/// acquisition half (P0 +1) are asserted independently: if the parsed chain
/// stops at its no-op `Effect::Unimplemented` head and never reaches
/// `sub_ability`, P1 still loses the draw but P0 gains nothing — strictly worse
/// than the card not existing — and the P0 assertions fail while the P1
/// assertions still pass.
#[test]
fn notion_thief_redirects_opponent_draw_to_its_controller() {
    let Some(db) = load_db() else {
        return;
    };
    let mut runner = scenario(db, true);

    // Precondition (CR 504.1): this draw is NOT the active player's first draw
    // of their draw step, so the card's exception clause does not apply. Assert
    // it rather than assume it — a draw inside the excluded window would make
    // the whole test vacuous.
    assert_ne!(
        runner.state().phase,
        Phase::Draw,
        "the redirected draw must occur outside any draw step"
    );
    assert_eq!(
        runner.state().active_player,
        P0,
        "P1 is the non-active player here, so no draw of theirs is a draw-step draw"
    );

    draw_one(&mut runner, P1);

    // ── Half 1: the opponent's draw is skipped (CR 614.6). ──────────────────
    assert!(
        zone_names(runner.state(), P1, Zone::Hand).is_empty(),
        "P1's draw must be skipped — hand stays empty; got {:?}",
        zone_names(runner.state(), P1, Zone::Hand)
    );
    assert_eq!(
        zone_names(runner.state(), P1, Zone::Library),
        P1_LIBRARY.map(String::from).to_vec(),
        "P1's library must be untouched — the skipped draw moves no card"
    );

    // ── Half 2: the controller draws instead. ───────────────────────────────
    // This is what the parser-level `execute.is_some()` assertion cannot see.
    assert_eq!(
        zone_names(runner.state(), P0, Zone::Hand),
        vec!["Sol Ring".to_string()],
        "P0 must draw exactly one card — their OWN top card (Sol Ring) — in place \
         of P1's skipped draw; an empty hand means the execute chain stopped at \
         its no-op Unimplemented head and never reached sub_ability"
    );
    assert_eq!(
        zone_names(runner.state(), P0, Zone::Library),
        P0_LIBRARY[1..]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "P0's redirected draw must come off the top of P0's own library"
    );
}

/// CR 121.1 + CR 504.1: the card exempts "the first one they draw in each of
/// their draw steps". Drives both sides of that gate in one turn: the active
/// player's first draw-step draw is untouched, and the second draw in the same
/// step is redirected. Fails if the `ExceptFirstDrawInDrawStep` condition is
/// dropped (the first draw would be stolen) or inverted.
#[test]
fn notion_thief_exempts_only_the_first_draw_of_the_opponents_draw_step() {
    let Some(db) = load_db() else {
        return;
    };
    let mut runner = scenario(db, true);
    // Put P1 in their own draw step with the per-step draw counter still at
    // zero, exactly as `finish_enter_phase` leaves it on step entry.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().phase = Phase::Draw;
    assert_eq!(
        runner.state().players[P1.0 as usize].cards_drawn_this_step,
        0,
        "the exemption is keyed on the per-step draw counter; it must start at 0"
    );

    // First draw of P1's draw step — exempt.
    draw_one(&mut runner, P1);
    assert_eq!(
        zone_names(runner.state(), P1, Zone::Hand),
        vec!["Grizzly Bears".to_string()],
        "P1's FIRST draw-step draw is exempt and must land in P1's hand"
    );
    assert!(
        zone_names(runner.state(), P0, Zone::Hand).is_empty(),
        "Notion Thief must not fire on the exempt draw; got {:?}",
        zone_names(runner.state(), P0, Zone::Hand)
    );

    // Second draw in the SAME step — no longer exempt, so it is redirected.
    draw_one(&mut runner, P1);
    assert_eq!(
        zone_names(runner.state(), P1, Zone::Hand),
        vec!["Grizzly Bears".to_string()],
        "P1's SECOND draw-step draw must be skipped — hand is unchanged"
    );
    assert_eq!(
        zone_names(runner.state(), P0, Zone::Hand),
        vec!["Sol Ring".to_string()],
        "the second draw redirects to P0, who draws their own top card"
    );
}

/// Discrimination probe for the `+1` half of
/// [`notion_thief_redirects_opponent_draw_to_its_controller`].
///
/// Severing `execute.sub_ability` reproduces exactly the regression that test is
/// there to catch — the parser dropping the "and you draw a card" link, or the
/// resolver never walking past the no-op `Effect::Unimplemented` head. In that
/// world the suppression half still holds (P1's draw is zeroed, because
/// `draw_is_substituted_away` keys only on the non-`Draw` head) but the
/// acquisition half vanishes, leaving a card strictly worse than not existing.
///
/// Asserting the mutant's outcome here proves the real test's `P0 == ["Sol
/// Ring"]` assertion discriminates that regression rather than restating a
/// value the pipeline would produce either way.
#[test]
fn severing_the_sub_ability_strands_the_draw_and_the_probe_sees_it() {
    let Some(db) = load_db() else {
        return;
    };
    let mut runner = scenario(db, true);

    let thief = notion_thief_id(runner.state());
    let object = runner
        .state_mut()
        .objects
        .get_mut(&thief)
        .expect("Notion Thief object");
    // `Definitions<T>` is copy-on-write and exposes no `iter_mut`; index through
    // its `IndexMut` impl instead.
    for i in 0..object.replacement_definitions.len() {
        let execute = object.replacement_definitions[i]
            .execute
            .as_mut()
            .expect("Notion Thief has an execute");
        assert!(
            execute.sub_ability.take().is_some(),
            "the mutation must actually remove a sub_ability — an already-None \
             chain would make this probe vacuous"
        );
    }
    for def in std::sync::Arc::make_mut(&mut object.base_replacement_definitions).iter_mut() {
        if let Some(execute) = def.execute.as_mut() {
            execute.sub_ability = None;
        }
    }

    draw_one(&mut runner, P1);

    assert!(
        zone_names(runner.state(), P1, Zone::Hand).is_empty(),
        "the mutant still suppresses P1's draw — suppression keys on the head, \
         not the tail; got {:?}",
        zone_names(runner.state(), P1, Zone::Hand)
    );
    assert!(
        zone_names(runner.state(), P0, Zone::Hand).is_empty(),
        "with the sub_ability severed P0 must draw NOTHING; this is the failure \
         the real test rejects. Got {:?}",
        zone_names(runner.state(), P0, Zone::Hand)
    );
}
