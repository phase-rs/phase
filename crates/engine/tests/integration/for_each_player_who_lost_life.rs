//! Backlog root-cause #5 (dropped 'for each' / dynamic count collapsed to Fixed):
//! "for each **player** who lost life this turn" must lift to a dynamic
//! `PlayerCount { LifeChangedThisTurn { scope: All, direction: Lost } }`, not
//! collapse to `Fixed(1)`.
//!
//! - **Reaper's Scythe**: "At the beginning of your end step, put a soul counter
//!   on this Equipment for each player who lost life this turn." → `PutCounter`
//!   count lifts to the all-players lost-life `PlayerCount`.
//! - **Strefan, Maurer Progenitor**: "At the beginning of your end step, create a
//!   Blood token for each player who lost life this turn." → `Token` count lifts.
//!
//! The `player` scope (all players, controller included) is the cell this fix
//! adds; the pre-existing `opponent` scope already worked (Teysa/Gev/Kaito). The
//! SHAPE tests drive the real parse pipeline (`parse_oracle_text`) on verbatim
//! Oracle text; the RUNTIME tests drive the real end-step trigger through
//! `apply()` and count the soul counters / Blood tokens actually created.
//!
//! Discrimination: with the fix reverted the count is `Fixed(1)` → 1; with the
//! scope wrong (`Opponent`) the controller's own loss is excluded → 1; only the
//! correct all-players scope counts controller + opponent → 2.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Effect, LifeChangeDirection, PlayerFilter, PlayerRelation, QuantityExpr, QuantityRef,
};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const REAPER_ORACLE: &str = "Job select\n\
    At the beginning of your end step, put a soul counter on this Equipment for each player who \
    lost life this turn.\n\
    Equipped creature gets +1/+1 for each soul counter on this Equipment and is an Assassin in \
    addition to its other types.\n\
    Death Sickle — Equip {2}";

const STREFAN_ORACLE: &str = "Flying\n\
    At the beginning of your end step, create a Blood token for each player who lost life this \
    turn.\n\
    Whenever Strefan attacks, you may sacrifice two Blood tokens. If you do, you may put a \
    Vampire card from your hand onto the battlefield tapped and attacking. It gains indestructible \
    until end of turn.";

// The end-step trigger sentences in isolation — the runtime permanent carries ONLY
// the for-each trigger, so the counter/token delta measures it alone.
const REAPER_END_STEP: &str =
    "At the beginning of your end step, put a soul counter on this Equipment for each player who \
     lost life this turn.";
const STREFAN_END_STEP: &str =
    "At the beginning of your end step, create a Blood token for each player who lost life this \
     turn.";

fn all_players_lost() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::PlayerCount {
            filter: PlayerFilter::LifeChangedThisTurn {
                scope: PlayerRelation::All,
                direction: LifeChangeDirection::Lost,
            },
        },
    }
}

fn soul() -> CounterType {
    CounterType::Generic("soul".to_string())
}

/// Number of "soul" counters on `obj` (CR 122.1), `0` if absent.
fn soul_counters(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&soul()).copied())
        .unwrap_or(0)
}

/// Count battlefield Blood tokens (CR 111.10 predefined token; subtype "Blood")
/// controlled by `player`.
fn count_blood(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == player && obj.is_token)
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Blood"))
        })
        .count()
}

fn assert_no_swallowed(parsed: &engine::parser::oracle::ParsedAbilities, card: &str) {
    assert!(
        !parsed
            .parse_warnings
            .iter()
            .any(|w| format!("{w:?}").contains("SwallowedClause")),
        "{card}: no clause may remain swallowed: {:?}",
        parsed.parse_warnings
    );
}

/// SHAPE #1 — Reaper's Scythe's end-step `PutCounter` lifts the for-each to the
/// all-players lost-life `PlayerCount`. Reach-guard: the effect really is a
/// `PutCounter` of a "soul" counter (not `Unimplemented`), so the swallow
/// assertion is not vacuous. Fails iff the fix is reverted (count → `Fixed(1)`).
#[test]
fn reaper_scythe_shape_lifts_all_players_lost_life() {
    let parsed = parse_oracle_text(REAPER_ORACLE, "Reaper's Scythe", &[], &[], &[]);
    let end_step = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::End))
        .expect("Reaper's Scythe has an end-step trigger");
    let execute = end_step.execute.as_ref().expect("end-step execute");

    let count = match execute.effect.as_ref() {
        Effect::PutCounter {
            counter_type,
            count,
            ..
        } => {
            assert_eq!(
                *counter_type,
                soul(),
                "reach-guard: end-step effect must place a soul counter"
            );
            count.clone()
        }
        other => panic!("reach-guard: end-step effect must be PutCounter, got {other:?}"),
    };
    assert_eq!(
        count,
        all_players_lost(),
        "Reaper's Scythe must place one soul counter per player who lost life this turn"
    );
    assert_no_swallowed(&parsed, "Reaper's Scythe");
}

/// SHAPE #2 — Strefan's end-step `Token` lifts the for-each. Reach-guard: the
/// effect really is a Blood `Token` (not `Unimplemented`).
#[test]
fn strefan_shape_lifts_all_players_lost_life() {
    let parsed = parse_oracle_text(
        STREFAN_ORACLE,
        "Strefan, Maurer Progenitor",
        &["Flying".to_string()],
        &[],
        &[],
    );
    let end_step = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::End))
        .expect("Strefan has an end-step trigger");
    let execute = end_step.execute.as_ref().expect("end-step execute");

    let count = match execute.effect.as_ref() {
        Effect::Token { name, count, .. } => {
            assert_eq!(
                name, "Blood",
                "reach-guard: end-step effect must create Blood"
            );
            count.clone()
        }
        other => panic!("reach-guard: end-step effect must be Token, got {other:?}"),
    };
    assert_eq!(
        count,
        all_players_lost(),
        "Strefan must create one Blood token per player who lost life this turn"
    );
    assert_no_swallowed(&parsed, "Strefan, Maurer Progenitor");
}

/// Build a 4-player game (P0 controls Reaper's Scythe) after combat, seed each
/// player's `life_lost_this_turn` ledger, and return the runner + the Equipment's
/// id ready to advance into the end step. Starting in post-combat main means
/// `advance_to_end_step` neither halts at DeclareAttackers nor wraps a turn
/// boundary, so the ledger (CR 119.3 — reset only at turn start) survives to the
/// end-step trigger's resolution.
fn reaper_runner(losses: [(PlayerId, u32); 4]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 20);
    scenario.at_phase(Phase::PostCombatMain);
    let reaper = scenario
        .add_artifact_from_oracle(P0, "Reaper's Scythe", REAPER_END_STEP)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    for (pid, n) in losses {
        if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == pid) {
            p.life_lost_this_turn = n;
        }
    }
    (runner, reaper)
}

fn strefan_runner(losses: [(PlayerId, u32); 4]) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(4, 20);
    scenario.at_phase(Phase::PostCombatMain);
    scenario
        .add_creature(P0, "Strefan, Maurer Progenitor", 5, 5)
        .from_oracle_text(STREFAN_END_STEP);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    for (pid, n) in losses {
        if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == pid) {
            p.life_lost_this_turn = n;
        }
    }
    runner
}

/// RUNTIME (Reaper's Scythe, discriminating) — controller P0 lost 4 and opponent
/// P1 lost 3 (both count under the all-players scope); P2/P3 lost 0 → exactly 2
/// soul counters. Revert-probe: `Fixed(1)` → 1; wrong `Opponent` scope → 1 (P0's
/// own loss excluded). Only the correct all-players scope makes 2.
#[test]
fn reaper_scythe_counts_all_players_who_lost_life() {
    // Reach-guard: the lift is active before we drive.
    let parsed = parse_oracle_text(REAPER_END_STEP, "Reaper's Scythe", &[], &[], &[]);
    let count = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::End))
        .and_then(|t| t.execute.as_ref())
        .and_then(|e| match e.effect.as_ref() {
            Effect::PutCounter { count, .. } => Some(count.clone()),
            _ => None,
        });
    assert_eq!(
        count,
        Some(all_players_lost()),
        "reach-guard: end-step PutCounter must carry the all-players lost-life count, got {count:?}"
    );

    let (mut runner, reaper) = reaper_runner([(P0, 4), (P1, 3), (P2, 0), (P3, 0)]);
    let before = soul_counters(&runner, reaper);
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        soul_counters(&runner, reaper) - before,
        2,
        "controller (lost 4) + opponent P1 (lost 3) both count → 2 soul counters"
    );
}

/// RUNTIME (Reaper's Scythe, controller-inclusion discriminator) — ONLY the
/// controller lost life. All-players scope counts the controller → 1; the wrong
/// `Opponent` scope would exclude it → 0.
#[test]
fn reaper_scythe_counts_controller_own_life_loss() {
    let (mut runner, reaper) = reaper_runner([(P0, 3), (P1, 0), (P2, 0), (P3, 0)]);
    let before = soul_counters(&runner, reaper);
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        soul_counters(&runner, reaper) - before,
        1,
        "only the controller lost life; the all-players scope still counts them → 1"
    );
}

/// RUNTIME (Reaper's Scythe, zero flip) — no player lost life. The for-each ranges
/// over an empty set → 0 counters. A `Fixed(1)` revert would wrongly make 1, so
/// the 0 delta is a crisp discriminator (paired with the positive reach-guard in
/// `reaper_scythe_counts_all_players_who_lost_life`).
#[test]
fn reaper_scythe_no_counter_when_no_player_lost_life() {
    let (mut runner, reaper) = reaper_runner([(P0, 0), (P1, 0), (P2, 0), (P3, 0)]);
    let before = soul_counters(&runner, reaper);
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        soul_counters(&runner, reaper) - before,
        0,
        "no player lost life → empty for-each → 0 soul counters (a bare Fixed(1) would make 1)"
    );
}

/// RUNTIME (Strefan, discriminating) — controller P0 lost 2 and opponent P2 lost 1
/// → 2 Blood tokens. Revert-probe: `Fixed(1)` → 1; `Opponent` scope → 1.
#[test]
fn strefan_creates_one_blood_per_player_who_lost_life() {
    let mut runner = strefan_runner([(P0, 2), (P1, 0), (P2, 1), (P3, 0)]);
    let before = count_blood(runner.state(), P0);
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        count_blood(runner.state(), P0) - before,
        2,
        "controller (lost 2) + opponent P2 (lost 1) both count → 2 Blood tokens"
    );
}

/// RUNTIME (Strefan, zero flip) — no player lost life → 0 Blood tokens.
#[test]
fn strefan_no_blood_when_no_player_lost_life() {
    let mut runner = strefan_runner([(P0, 0), (P1, 0), (P2, 0), (P3, 0)]);
    let before = count_blood(runner.state(), P0);
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        count_blood(runner.state(), P0) - before,
        0,
        "no player lost life → 0 Blood tokens (a bare Fixed(1) would make 1)"
    );
}
