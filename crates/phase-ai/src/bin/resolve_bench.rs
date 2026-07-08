//! Drive stack resolution on a saved GameState and time it.
//!
//! Loads `{ "gameState": ... }` (client checkpoint format), then repeatedly
//! applies `PassPriority` for whichever player has priority, draining the
//! stack. Times each `apply()` call and the whole drain. Isolates pure ENGINE
//! resolution cost ("evaluating the stack triggers") from any AI search.
//!
//! Build under the `tool` profile twice to isolate the debug-assertions tax at
//! fixed opt-level:
//!   CARGO_TARGET_DIR=/tmp/forge-prof-target cargo build --profile tool --bin resolve_bench
//!   RUSTFLAGS="-C debug-assertions=on" CARGO_TARGET_DIR=/tmp/forge-prof-target-dbg \
//!       cargo build --profile tool --bin resolve_bench

use std::collections::BTreeMap;
use std::fs;
use std::time::Instant;

use engine::game::engine::{apply, resolve_all_fast_forward, ResolveAllCallbackDecision};
use engine::game::perf_counters;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::player::PlayerId;
use serde::Deserialize;

#[derive(Deserialize)]
struct Saved {
    #[serde(rename = "gameState")]
    game_state: GameState,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/gs16.json".to_string());

    let raw = fs::read_to_string(&path).expect("read file");
    let saved: Saved = serde_json::from_str(&raw).expect("parse json");
    let original = saved.game_state;

    println!("debug_assertions = {}", cfg!(debug_assertions),);
    println!("objects = {}", original.objects.len());
    println!("battlefield = {}", original.battlefield.len());
    println!("stack (start) = {}", original.stack.len());
    println!("players = {}", original.players.len());
    println!(
        "waiting_for = {:?}",
        std::mem::discriminant(&original.waiting_for)
    );
    println!();
    print_stack_summary(&original);

    let resolution_cap = std::env::var("RESOLVE_BENCH_CAP")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(200);
    let iters = std::env::var("RESOLVE_BENCH_ITERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1);

    for it in 0..iters {
        if std::env::var("RESOLVE_BENCH_SKIP_APPLY").is_err() {
            bench_apply_loop(&original, it, resolution_cap);
        }
        bench_fast_forward(&original, it, resolution_cap);
    }
}

fn bench_apply_loop(original: &GameState, it: usize, resolution_cap: u32) {
    let mut state = original.clone();
    let mut step = 0usize;
    let mut total_events = 0usize;
    let mut items_resolved = 0u32;
    let start_stack = state.stack.len();
    perf_counters::reset();
    let t_all = Instant::now();
    let mut worst = std::time::Duration::ZERO;
    let mut worst_step = 0usize;

    loop {
        if state.stack.is_empty() {
            break;
        }
        let Some(actor) = state.waiting_for.acting_player() else {
            println!(
                "  [stop] non-single-actor waiting_for at step {step}: {:?}",
                std::mem::discriminant(&state.waiting_for)
            );
            break;
        };
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            println!(
                "  [stop] non-Priority waiting_for at step {step}: {:?}",
                std::mem::discriminant(&state.waiting_for)
            );
            break;
        }

        let t = Instant::now();
        let res = apply(&mut state, actor, GameAction::PassPriority);
        let dt = t.elapsed();
        if dt > worst {
            worst = dt;
            worst_step = step;
        }
        match res {
            Ok(r) => {
                let resolved = stack_resolved_count(&r.events);
                items_resolved = items_resolved.saturating_add(resolved);
                total_events += r.events.len();
                if it == 0 && (resolved > 0 || dt.as_millis() > 10) {
                    println!(
                            "  apply step {step:>5}: actor={:?} {dt:?}  events={} resolved={} stack_after={}",
                            actor,
                            r.events.len(),
                            resolved,
                            state.stack.len()
                        );
                }
            }
            Err(e) => {
                println!("  [err] step {step}: {e:?}");
                break;
            }
        }
        step += 1;
        if items_resolved >= resolution_cap {
            break;
        }
    }

    let all = t_all.elapsed();
    let counters = perf_counters::snapshot();
    println!(
            "apply iter {it}: resolved={items_resolved} stack {start_stack}->{} in {step} apply() calls, {all:?} total, worst {worst:?} @step{worst_step}, events={total_events}, counters={counters:?}",
            state.stack.len(),
        );
    println!();
}

fn bench_fast_forward(original: &GameState, it: usize, resolution_cap: u32) {
    let mut state = original.clone();
    let start_stack = state.stack.len();
    perf_counters::reset();
    let t_all = Instant::now();
    let result = resolve_all_fast_forward(&mut state, PlayerId(0), resolution_cap, |_, _| {
        ResolveAllCallbackDecision::Action(GameAction::PassPriority)
    });
    let all = t_all.elapsed();
    let counters = perf_counters::snapshot();
    println!(
        "fast  iter {it}: resolved={} stack {start_stack}->{} in {all:?}, events={}, logs={}, waiting={}, counters={counters:?}",
        result.items_resolved,
        state.stack.len(),
        result.events.len(),
        result.log_entries.len(),
        waiting_summary(&result.waiting_for),
    );
    println!();
}

fn waiting_summary(waiting_for: &WaitingFor) -> String {
    match waiting_for {
        WaitingFor::Priority { player } => format!("Priority({player:?})"),
        WaitingFor::OrderTriggers { player, triggers } => {
            format!("OrderTriggers({player:?}, {})", triggers.len())
        }
        other => format!("{:?}", std::mem::discriminant(other)),
    }
}

fn stack_resolved_count(events: &[GameEvent]) -> u32 {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
        .count() as u32
}

fn print_stack_summary(state: &GameState) {
    let mut sources = BTreeMap::<String, usize>::new();
    let mut kinds = BTreeMap::<&'static str, usize>::new();
    for entry in &state.stack {
        let source_name = state
            .objects
            .get(&entry.source_id)
            .map(|object| object.name.clone())
            .unwrap_or_else(|| format!("#{}", entry.source_id.0));
        *sources.entry(source_name).or_default() += 1;
        let kind = match &entry.kind {
            StackEntryKind::Spell { .. } => "Spell",
            StackEntryKind::ActivatedAbility { .. } => "ActivatedAbility",
            StackEntryKind::TriggeredAbility { .. } => "TriggeredAbility",
            StackEntryKind::KeywordAction { .. } => "KeywordAction",
        };
        *kinds.entry(kind).or_default() += 1;
    }
    println!("stack kinds: {kinds:?}");
    println!("top sources:");
    for (name, count) in sources.iter().rev().take(12) {
        println!("  {count:>6} {name}");
    }
    println!();
}
