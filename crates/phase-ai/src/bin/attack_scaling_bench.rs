//! Profile combat declaration as the attacker count scales.
//!
//! The user couldn't reproduce "slow attacking" interactively, so this builds a
//! synthetic go-wide board of N vanilla creatures, advances to the
//! declare-attackers step, and times two things per N:
//!   1. `get_valid_attacker_ids` — the legality scan the engine runs to build
//!      the `WaitingFor::DeclareAttackers` snapshot (the read path).
//!   2. `apply(DeclareAttackers{all})` — declaring every creature at once (the
//!      write path a player hits when they alpha-strike).
//!
//! Printing time vs N exposes any O(n^2) curve directly (doubling N should ~2x
//! a linear cost; ~4x flags quadratic).
//!
//! Build/run with a debug build in an isolated target dir (keeps Tilt's own
//! target lock uncontended):
//!   CARGO_TARGET_DIR=/tmp/forge-dbg cargo run \
//!       -p phase-ai --bin attack_scaling_bench
//!
//! The perf counters this prints are profile-independent — only the absolute
//! per-call times inflate in a debug build. The `profiling` profile compiles
//! the engine very slowly and contends with Tilt, so prefer the debug build
//! above unless you specifically need realistic wall-clock numbers.

use std::time::{Duration, Instant};

use engine::game::combat::{
    get_valid_attack_targets, get_valid_attacker_ids, AttackTarget, CombatState,
};
use engine::game::perf_counters;
use engine::game::scenario::GameScenario;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn bench_n(n: usize) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    for _ in 0..n {
        scenario.add_vanilla(P0, 1, 1);
    }
    let mut runner = scenario.build();

    // Build the DeclareAttackers waiting state directly (mirrors the engine's own
    // combat tests, combat.rs:4077). `advance_to_combat()` would walk the draw
    // step, but the synthetic scenario has empty libraries, so the active player
    // decks out (CR 104.3c) before combat. Setting the state at the
    // declare-attackers step skips that and isolates the cost we want to measure.
    {
        let s = runner.state_mut();
        s.phase = Phase::DeclareAttackers;
        s.active_player = P0;
        s.priority_player = P0;
        s.combat = Some(CombatState::default());
    }
    let valid_attacker_ids = get_valid_attacker_ids(runner.state());
    let valid_attack_targets = get_valid_attack_targets(runner.state());
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids,
        valid_attack_targets,
    };

    let at_declare = matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    );

    // Isolated read-path timing: the legality scan that builds the snapshot.
    let start = Instant::now();
    let valid = get_valid_attacker_ids(runner.state());
    let scan_dt = start.elapsed();

    let attacks: Vec<(_, AttackTarget)> = valid
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    let declared = attacks.len();

    perf_counters::reset();
    let start = Instant::now();
    let result = runner.declare_attackers(&attacks);
    let declare_dt = start.elapsed();
    let c = perf_counters::snapshot();

    let per_attacker = if declared == 0 {
        Duration::ZERO
    } else {
        declare_dt / declared as u32
    };

    print!(
        "N={n:5} at_declare={at_declare:5} valid={declared:5}  \
         scan={scan_dt:>10.3?}  declare={declare_dt:>10.3?}  per_attacker={per_attacker:>9.3?}  \
         clones={} layers(full={} inc={}) mana_sweeps={} swept={}",
        c.state_clone_for_legality,
        c.layers_full_eval,
        c.layers_incremental,
        c.mana_display_sweeps,
        c.mana_display_swept_objects,
    );
    match result {
        Ok(_) => println!("  -> {}", runner.state().waiting_for.variant_name()),
        Err(e) => println!("  -> ERR {e:?}"),
    }
}

fn main() {
    println!("debug_assertions = {}", cfg!(debug_assertions));
    println!();
    for n in [50usize, 100, 200, 400, 800, 1200] {
        bench_n(n);
    }
}
