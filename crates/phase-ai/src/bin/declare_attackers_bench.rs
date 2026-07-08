//! Profile the full declare-attackers cost on a *real* saved board.
//!
//! `attack_scaling_bench` uses a synthetic go-wide board of vanilla creatures.
//! This loads an actual client checkpoint (`{ "gameState": ... }`), forces the
//! active player into their declare-attackers step, and times every sub-path a
//! turn-40 squirrel board actually hits so we can see which one dominates:
//!
//!   1. `get_valid_attacker_ids` / `get_valid_attack_targets` — the read-path
//!      legality scan that builds the `WaitingFor::DeclareAttackers` snapshot.
//!   2. `candidate_actions` + `validated_candidate_actions` — the GENERIC AI /
//!      frontend legal-action path. `validated_candidate_actions` runs the
//!      `SimulationFilter` (a full `GameState::clone()` + `apply` per candidate),
//!      so a board with N attackers × M targets clones the whole state N×M times.
//!   3. `choose_action` — the real AI decision. For DeclareAttackers this
//!      delegates to the specialized combat AI (NOT the generic candidate
//!      explosion), so it isolates the combat-AI heuristic cost.
//!   4. `apply(DeclareAttackers{all})` — the human bulk-submit write path.
//!
//! Build/run with an isolated target dir (keeps Tilt's lock uncontended):
//!   CARGO_TARGET_DIR=/tmp/forge-prof-target cargo run \
//!       -p phase-ai --bin declare-attackers-bench -- path/to/state.json
//!
//! Counters are profile-independent; only the absolute per-call times inflate
//! in a debug build. Prefer debug for fast iteration on the scaling shape.

use std::io::Write;
use std::time::Instant;

use engine::ai_support;
use engine::game::combat::{
    get_valid_attack_targets, get_valid_attacker_ids, AttackTarget, CombatState,
};
use engine::game::engine::apply;
use engine::game::perf_counters;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use phase_ai::search::choose_action;
use rand::rngs::StdRng;
use rand::SeedableRng;

fn counters_line(label: &str) {
    let c = perf_counters::snapshot();
    println!(
        "    [{label}] clones={} static_scans={} layers(full={} inc={}) mana_sweeps={} swept={} attackable_sweeps={} shadow_scans={}",
        c.state_clone_for_legality,
        c.static_full_scans,
        c.layers_full_eval,
        c.layers_incremental,
        c.mana_display_sweeps,
        c.mana_display_swept_objects,
        c.attackable_player_sweeps,
        c.combat_shadow_block_scans,
    );
    std::io::stdout().flush().ok();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/gs.json".to_string());

    let raw = std::fs::read_to_string(&path).expect("read state file");
    let mut state = load_saved_game_state(&raw).expect("parse saved state");

    println!("debug_assertions = {}", cfg!(debug_assertions));
    println!("path = {path}");
    println!("objects = {}", state.objects.len());
    println!("battlefield = {}", state.battlefield.len());
    println!("players = {}", state.players.len());
    println!("active_player (orig) = {:?}", state.active_player);
    println!("phase (orig) = {:?}", state.phase);
    println!();
    std::io::stdout().flush().ok();

    // Force the active player into their declare-attackers step. Mirrors
    // attack_scaling_bench: set the phase/combat scaffolding and rebuild the
    // waiting-state snapshot from the live queries.
    let active = state.active_player;
    state.phase = Phase::DeclareAttackers;
    state.priority_player = active;
    state.combat = Some(CombatState::default());

    // ── 1. Read path: the legality scan that builds the snapshot ──────────────
    perf_counters::reset();
    let t = Instant::now();
    let valid_attacker_ids = get_valid_attacker_ids(&state);
    let scan_attackers_dt = t.elapsed();

    let t = Instant::now();
    let valid_attack_targets = get_valid_attack_targets(&state);
    let scan_targets_dt = t.elapsed();

    println!("=== read path (snapshot build) ===");
    println!("valid attackers:     {}", valid_attacker_ids.len());
    println!("valid attack targets:{}", valid_attack_targets.len());
    println!("get_valid_attacker_ids:  {scan_attackers_dt:?}");
    println!("get_valid_attack_targets:{scan_targets_dt:?}");
    counters_line("read");
    println!();

    state.waiting_for = WaitingFor::DeclareAttackers {
        player: active,
        valid_attacker_ids: valid_attacker_ids.clone(),
        valid_attack_targets: valid_attack_targets.clone(),
    };

    // ── 2. Generic candidate path (frontend legal-actions / SimulationFilter) ─
    perf_counters::reset();
    let t = Instant::now();
    let raw_candidates = ai_support::candidate_actions(&state);
    let raw_dt = t.elapsed();
    let raw_count = raw_candidates.len();
    println!("=== generic candidate path (frontend / SimulationFilter) ===");
    println!("raw candidates:      {raw_count}");
    println!("candidate_actions:   {raw_dt:?}");
    counters_line("candidate_actions");
    // SimulationFilter clones the whole state + applies PER candidate. On a
    // 700-attacker board that is ~2000 full-state clones; in a debug build this
    // can OOM/take minutes, so gate it behind DA_BENCH_VALIDATE=1.
    if std::env::var("DA_BENCH_VALIDATE").is_ok() {
        perf_counters::reset();
        let t = Instant::now();
        let valid_candidates = ai_support::validated_candidate_actions(&state);
        let validated_dt = t.elapsed();
        println!("validated candidates:{}", valid_candidates.len());
        println!("validated_candidate_actions (clone+apply per cand): {validated_dt:?}");
        counters_line("validated");
    } else {
        println!("validated_candidate_actions: SKIPPED (set DA_BENCH_VALIDATE=1 to run)");
    }
    println!();

    // ── 3. Real AI decision (delegates to specialized combat AI) ──────────────
    let config = create_config_for_players(
        AiDifficulty::Medium,
        Platform::Native,
        state.players.len() as u8,
    )
    .into_measurement(42);
    let mut rng = StdRng::seed_from_u64(42);
    perf_counters::reset();
    let t = Instant::now();
    let action = choose_action(&state, active, &config, &mut rng);
    let choose_dt = t.elapsed();
    let declared = match &action {
        Some(GameAction::DeclareAttackers { attacks, .. }) => attacks.len(),
        _ => 0,
    };
    println!("=== choose_action (combat AI) ===");
    println!("duration:            {choose_dt:?}");
    println!("attackers chosen:    {declared}");
    counters_line("choose_action");
    println!();

    // ── 4. Human bulk-submit write path: declare every valid attacker ─────────
    let target = valid_attack_targets
        .first()
        .copied()
        .unwrap_or(AttackTarget::Player(engine::types::player::PlayerId(0)));
    let attacks: Vec<(_, AttackTarget)> =
        valid_attacker_ids.iter().map(|&id| (id, target)).collect();
    let n_all = attacks.len();
    let mut sim = state.clone();
    perf_counters::reset();
    let t = Instant::now();
    let result = apply(
        &mut sim,
        active,
        GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        },
    );
    let apply_dt = t.elapsed();
    println!("=== apply(DeclareAttackers all) (human bulk submit) ===");
    println!("attackers declared:  {n_all}");
    println!("apply duration:      {apply_dt:?}");
    println!(
        "per attacker:        {:?}",
        if n_all == 0 {
            std::time::Duration::ZERO
        } else {
            apply_dt / n_all as u32
        }
    );
    println!(
        "result:              {}",
        if result.is_ok() { "Ok" } else { "Err" }
    );
    counters_line("apply");
}
