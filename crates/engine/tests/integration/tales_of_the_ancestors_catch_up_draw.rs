//! Runtime pipeline regression for Tales of the Ancestors (KHM) line 1:
//! "Each player with fewer cards in hand than the player with the most cards in
//! hand draws cards equal to the difference."
//!
//! Before the parser fix the subject restriction + verb clause dispatched to a
//! bogus `Effect::Unimplemented { name: "with" }`. The fix recognizes the line
//! via a typed cross-player hand-size extremum combinator
//! (`parse_player_with_extremum_cards_in_hand`) plus a Verify guard, lowering it
//! to a `player_scope: All` `Effect::Draw { count: Difference { left:
//! HandSize{AllPlayers{Max}}, right: HandSize{ScopedPlayer} }, target:
//! Controller }`. Every runtime primitive (the per-player APNAP fan-out, the
//! CR 608.2e clause-minimum snapshot that freezes the Max operand, the negative
//! draw clamp per CR 107.1b) already exists and is exercised by Balance.
//!
//! The load-bearing discriminators drive the real cast→resolve pipeline through
//! `add_real_card` + `rehydrate_game_from_card_db` so the test reads the
//! deployed CardDatabase parse path (NOT `from_oracle_text`, which would re-parse
//! and mask data staleness). If the parser fix were reverted, Tales would parse
//! to `Unimplemented` and every per-player hand delta below would be 0 — the
//! `draws_catch_up_to_max` assertions flip.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AggregateFunction, Effect, PlayerFilter, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const P2: PlayerId = PlayerId(2);
const TALES: &str = "Tales of the Ancestors";

/// {3}{U} worth of mana so P0 (the caster) can pay Tales' cost from its pool.
fn tales_mana() -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..3 {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    pool.push(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    pool
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn hand_names(runner: &GameRunner, player: PlayerId) -> Vec<String> {
    runner.state().players[player.0 as usize]
        .hand
        .iter()
        .filter_map(|id| runner.state().objects.get(id).map(|o| o.name.clone()))
        .collect()
}

/// Build an N-player game with `hand_counts[i]` generic filler cards in player
/// `i`'s hand, Tales in P0's hand (cast from the real CardDatabase parse), and a
/// deterministic library top per player so every drawn card is an identifiable
/// sentinel. `sentinel[i]` is the name each of player `i`'s draws will pull.
fn build_tales_game(
    db: &CardDatabase,
    hand_counts: &[usize],
    sentinels: &[&str],
) -> (GameRunner, ObjectId) {
    let count = hand_counts.len() as u8;
    let mut scenario = GameScenario::new_n_player(count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, tales_mana());

    // Tales in P0's hand via the deployed parse path.
    let tales = scenario.add_real_card(P0, TALES, Zone::Hand, db);

    for (i, &n) in hand_counts.iter().enumerate() {
        let pid = PlayerId(i as u8);
        // Generic filler hand cards (names are per-player so deltas are legible).
        let filler: Vec<String> = (0..n).map(|j| format!("Filler P{i} #{j}")).collect();
        let filler_refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        scenario.with_cards_in_hand(pid, &filler_refs);

        // Enough copies of this player's sentinel on top of their library that
        // the maximum possible catch-up draw is always covered.
        let top: Vec<&str> = vec![sentinels[i]; 8];
        scenario.with_library_top(pid, &top);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner, tales)
}

/// 3 players, hands 2 / 5 / 3 → max = 5 → draws 3 / 0 / 2. The load-bearing
/// discriminating case: every per-player delta is non-degenerate and the
/// sentinel cards must land in the correct hand.
#[test]
fn draws_catch_up_to_max_three_players() {
    let Some(db) = load_db() else { return };

    let (mut runner, tales) = build_tales_game(
        db,
        &[2, 5, 3],
        &["Sentinel P0", "Sentinel P1", "Sentinel P2"],
    );

    // Tales itself is in P0's hand (counted in P0's hand size before cast). P0
    // begins with 2 filler + 1 Tales = 3; Tales leaves hand on cast, so the
    // measured hand sizes at resolution are 2 / 5 / 3.
    runner.cast(tales).resolve();
    runner.advance_until_stack_empty();

    // CR 107.1b clamp: P1 is the leader (5), draws 0.
    assert_eq!(hand_size(&runner, P1), 5, "P1 (leader) draws 0");
    // P0: 2 → 5 (drew 3 = 5 − 2).
    assert_eq!(hand_size(&runner, P0), 5, "P0 draws up to the max (3)");
    // P2: 3 → 5 (drew 2 = 5 − 3).
    assert_eq!(hand_size(&runner, P2), 5, "P2 draws up to the max (2)");

    // Each player's specific sentinels landed in the right hand — a wrong-player
    // draw is caught immediately.
    assert_eq!(
        hand_names(&runner, P0)
            .iter()
            .filter(|n| n.as_str() == "Sentinel P0")
            .count(),
        3,
        "P0 drew exactly 3 of its own sentinel"
    );
    assert_eq!(
        hand_names(&runner, P2)
            .iter()
            .filter(|n| n.as_str() == "Sentinel P2")
            .count(),
        2,
        "P2 drew exactly 2 of its own sentinel"
    );
    assert!(
        !hand_names(&runner, P1)
            .iter()
            .any(|n| n.as_str() == "Sentinel P1"),
        "P1 (leader) drew none of its sentinel"
    );
}

/// CR 608.2e snapshot / ordering guard. P0 is the active player and draws first
/// in APNAP. With the frozen-max snapshot, P0's draw up to 5 must NOT raise the
/// maximum a later player measures against — so P2 still draws exactly 2 and
/// nobody over-draws. Without the snapshot freeze a later player would mis-measure
/// the now-larger maximum.
#[test]
fn frozen_max_snapshot_apnap_ordering() {
    let Some(db) = load_db() else { return };

    // P0 is active by construction (at_phase sets active = priority). Hands at
    // resolution: 2 / 5 / 3, identical to the discriminating case, but here the
    // assertion is specifically that the EARLIER-APNAP P0 drawing to 5 does not
    // inflate the maximum P2 (later) measures.
    let (mut runner, tales) = build_tales_game(
        db,
        &[2, 5, 3],
        &["Sentinel P0", "Sentinel P1", "Sentinel P2"],
    );
    assert_eq!(
        runner.state().active_player,
        P0,
        "test premise: P0 is the active player (draws first in APNAP)"
    );

    runner.cast(tales).resolve();
    runner.advance_until_stack_empty();

    // P2 (later in APNAP) draws exactly 2 — measured against the FROZEN max of 5,
    // not a max inflated by P0's earlier draw. If the snapshot did not freeze the
    // Max operand, P2 would draw more than 2.
    assert_eq!(
        hand_size(&runner, P2),
        5,
        "P2 draws exactly 2 against the frozen max — no over-draw from P0's earlier draw"
    );
    // And P0 still drew exactly 3 (its own earlier draw didn't move its own target).
    assert_eq!(hand_size(&runner, P0), 5, "P0 drew exactly 3");
}

/// Clamp boundary: 2-player 0 / 4 → draws 4 / 0.
#[test]
fn clamp_two_player_zero_and_four() {
    let Some(db) = load_db() else { return };

    // P0 holds only Tales (0 filler); P1 holds 4 filler. On cast Tales leaves
    // P0's hand → measured 0 / 4. Max = 4 → P0 draws 4, P1 draws 0.
    let (mut runner, tales) = build_tales_game(db, &[0, 4], &["Sentinel P0", "Sentinel P1"]);

    runner.cast(tales).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_size(&runner, P0),
        4,
        "P0 (0) catches up to the max (4)"
    );
    assert_eq!(hand_size(&runner, P1), 4, "P1 (leader, 4) draws 0");
}

/// All-equal: 3 / 3 / 3 → draws 0 / 0 / 0 (every player is a leader and clamps).
#[test]
fn all_equal_hands_draw_nothing() {
    let Some(db) = load_db() else { return };

    // P0 holds 3 filler + Tales; Tales leaves on cast, so the measured hands are
    // 3 / 3 / 3 — every player is a leader and clamps to 0.
    let (mut runner, tales) = build_tales_game(
        db,
        &[3, 3, 3],
        &["Sentinel P0", "Sentinel P1", "Sentinel P2"],
    );

    runner.cast(tales).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(hand_size(&runner, P0), 3, "P0 (3) draws 0 — all equal");
    assert_eq!(hand_size(&runner, P1), 3, "P1 (3) draws 0 — all equal");
    assert_eq!(hand_size(&runner, P2), 3, "P2 (3) draws 0 — all equal");
}

/// SHAPE test (labelled): the deployed parse of Tales is the expected typed AST.
/// This asserts the parsed `Effect` shape only and does NOT resolve through the
/// engine — the runtime discriminators above (`draws_catch_up_to_max_*`) are the
/// load-bearing regression guards.
#[test]
fn tales_parses_to_expected_draw_shape() {
    let Some(db) = load_db() else { return };

    let face = db
        .get_face_by_name(TALES)
        .expect("Tales of the Ancestors in fixture");
    let effect = face
        .abilities
        .iter()
        .map(|a| a.effect.as_ref())
        .find(|e| matches!(e, Effect::Draw { .. }))
        .expect("Tales line 1 should parse to Effect::Draw");

    match effect {
        Effect::Draw { count, target } => {
            assert_eq!(*target, TargetFilter::Controller, "draws to the controller");
            match count {
                QuantityExpr::Difference { left, right } => {
                    assert_eq!(
                        **left,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::AllPlayers {
                                    aggregate: AggregateFunction::Max,
                                    exclude: None,
                                },
                            },
                        },
                        "left operand is the frozen cross-player Max"
                    );
                    assert_eq!(
                        **right,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::ScopedPlayer,
                            },
                        },
                        "right operand is the live per-player hand size"
                    );
                }
                other => panic!("expected Difference count, got {other:?}"),
            }
        }
        other => panic!("expected Effect::Draw, got {other:?}"),
    }

    // player_scope drives the APNAP fan-out over every player.
    let ability = face
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::Draw { .. }))
        .expect("draw ability");
    assert_eq!(
        ability.player_scope,
        Some(PlayerFilter::All),
        "player_scope: All drives the per-player APNAP iteration"
    );
}
