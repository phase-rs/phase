//! PR-6.5 inc2b — the growing-cascade DETECTOR, live ≥3-player win (N3).
//!
//! CR 732.2a + CR 704.5a + CR 104.2a: a super-critical (μ = 2) all-opponent drain
//! cascade in a 3-player game grows the stack without bound, so the shipped
//! constant-depth loop fingerprint (`loop_states_equal_modulo_resources`) never
//! matches. The new Karp–Miller-style coverability path
//! (`loop_states_cover_modulo_growth`) + the MP-general winner predicate
//! (`live_mandatory_loop_winner`) confirm the covering pair LIVE and shortcut to the
//! controller's win — BEFORE the opponents reach 0 life naturally (the discriminator).
//!
//! ON arm: `loop_detection == On` ⇒ the C2 gate auto-resolves the fan-out, the ring
//! accumulates, and the reconcile seam declares `GameOver { winner: Some(P0) }` with
//! both opponents still at POSITIVE life and P0's unbounded axes marked.
//!
//! OFF arm (default gameplay byte-preserved): `loop_detection == Off` ⇒ no shortcut;
//! the cascade drains both opponents to natural CR 704.5a SBA deaths (life ≤ 0,
//! `is_eliminated`), `unbounded_resources` empty.
//!
//! Revert-fails (each isolates one layer): (i) revert the winner predicate ⇒ natural
//! death signature; (ii) revert `cover_modulo_growth` ⇒ ring never confirms ⇒ natural
//! death; (iii) revert C2 ⇒ the fan-out `OrderTriggers`-prompts and the run stalls.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::game_state::{LoopDetectionMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const DRAIN_CLERIC: &str = "Whenever you gain life, each opponent loses 1 life.";
const BLOOD_SIPPER: &str = "Whenever an opponent loses life, you gain 1 life.";
const KICKOFF: &str = "You gain 1 life.";

fn life(runner: &GameRunner, p: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.life)
        .unwrap()
}

fn is_eliminated(runner: &GameRunner, p: PlayerId) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.is_eliminated)
        .unwrap()
}

/// Build the 3-player drain engine controlled by P0, with `loop_detection` set to
/// `mode`. Returns a runner positioned in P0's precombat main with the kick-off
/// sorcery in hand (id returned).
fn setup(mode: LoopDetectionMode) -> (GameRunner, engine::types::ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// Drive the cascade until `GameOver` or the cap, returning the number of beats
/// consumed and the terminal `waiting_for`. Passes priority on `Priority` windows
/// and submits the identity order on any `OrderTriggers` prompt (all the fan-out's
/// triggers are the same mandatory ability, so the order is immaterial). Any OTHER
/// prompt is returned as a stall for the assertions to diagnose.
///
/// `stall_on_order`: when true (the ON-arm check), an `OrderTriggers` prompt is
/// returned immediately instead of answered — it means C2 failed to auto-resolve.
fn drive(runner: &mut GameRunner, cap: usize, stall_on_order: bool) -> (usize, WaitingFor) {
    for beat in 0..cap {
        match runner.state().waiting_for.clone() {
            WaitingFor::GameOver { .. } => return (beat, runner.state().waiting_for.clone()),
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return (beat, runner.state().waiting_for.clone());
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } if !stall_on_order => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                if runner
                    .act(GameAction::OrderTriggers { order })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .is_err()
                {
                    return (beat, runner.state().waiting_for.clone());
                }
            }
            other => return (beat, other),
        }
    }
    (cap, runner.state().waiting_for.clone())
}

#[test]
fn n3_growing_cascade_three_player_live_win_on() {
    let (mut runner, kickoff) = setup(LoopDetectionMode::On);
    // Resolve the kick-off lifegain, seeding the μ=2 mutual amplifying cascade.
    let _ = runner.cast(kickoff).resolve();
    let (beats, wf) = drive(&mut runner, 500, true);
    eprintln!(
        "ON arm: beats={beats} wf={wf:?} P1={} P2={}",
        life(&runner, P1),
        life(&runner, P2)
    );

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "ON: the growing-cascade shortcut must declare P0 the winner"
    );
    // THE DISCRIMINATOR: the shortcut fired BEFORE natural death — both opponents
    // are still at POSITIVE life (a reverted predicate/cover would only reach
    // GameOver via the natural CR 704.5a death, with life <= 0).
    assert!(
        life(&runner, P1) > 0 && life(&runner, P2) > 0,
        "ON: both opponents must still be at positive life (shortcut fired early): P1={}, P2={}",
        life(&runner, P1),
        life(&runner, P2)
    );
    assert!(
        runner
            .state()
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| !axes.is_empty()),
        "ON: P0's unbounded loop axes must be marked (mark_unbounded_loop fired)"
    );
    // The shortcut arm does NOT write is_eliminated (that is the natural-death path).
    assert!(
        !is_eliminated(&runner, P1) && !is_eliminated(&runner, P2),
        "ON: the shortcut must not mark opponents eliminated"
    );
}

#[test]
fn n3_growing_cascade_three_player_natural_death_off() {
    let (mut runner, kickoff) = setup(LoopDetectionMode::Off);
    let _ = runner.cast(kickoff).resolve();
    let (beats, wf) = drive(&mut runner, 2000, false);
    eprintln!(
        "OFF arm: beats={beats} wf={wf:?} P1={} P2={}",
        life(&runner, P1),
        life(&runner, P2)
    );

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "OFF: the cascade still ends the game for P0 — via the NATURAL CR 704.5a death"
    );
    // Natural-death signature: opponents actually crossed 0 and were eliminated.
    assert!(
        life(&runner, P1) <= 0 && life(&runner, P2) <= 0,
        "OFF: both opponents must have drained to <= 0 life (no early shortcut)"
    );
    assert!(
        is_eliminated(&runner, P1) && is_eliminated(&runner, P2),
        "OFF: the natural elimination path marks opponents eliminated"
    );
    assert!(
        runner.state().unbounded_resources.is_empty(),
        "OFF: no unbounded axes are marked (the detector never ran)"
    );
}
