//! CR 118.3 + CR 613.1: clone budget for the ability-affordability read-out.
//!
//! Three claims, each with a control that demonstrably fires both ways:
//!
//! * **Row 11** — `activation_block_reasons` takes ZERO whole-state flush clones
//!   on a layers-clean board (CR 613.1: a clean board already IS the flushed
//!   board), and exactly one on a dirty board. The counter is
//!   `priority_cast_probe_state_clones`, the same one `legal_actions_full`'s own
//!   flush increments — a `state_clone_for_legality`-only instrument
//!   structurally cannot see this clone, which is why both counters are read.
//! * **Row 12** — the shared `activation_verdict` core runs exactly ONCE per
//!   examined ability. Paired with a second counter for abilities examined, so
//!   the number is attributable rather than a coincidence of a traversal that
//!   never ran; a two-pass mechanism would report ~2x.
//! * **Row 13** — `legal_actions_full`'s own cost is UNCHANGED: the display tail
//!   lives behind a separate entry point, so a non-display caller cannot pay for
//!   it. Paired positive: the `activation_block_reasons` call in the SAME test
//!   has a non-zero delta, so "unchanged" is never "nothing ran".
//!
//! Under `cargo nextest` each test runs in its own process, so the
//! `thread_local!` perf counters cannot bleed across tests.

use engine::ai_support::{activation_block_reasons, legal_actions_full};
use engine::game::layers::flush_layers;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

/// Board size. Every creature carries one unaffordable `Pay 5 life` ability, so
/// "abilities examined" is exactly `BOARD`.
const BOARD: usize = 12;

fn draw_for_life(amount: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::PayLife {
        amount: QuantityExpr::Fixed { value: amount },
    })
}

/// `BOARD` creatures, each with one unaffordable `Pay 5 life` ability, at 1 life.
fn unaffordable_board() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1);
    for i in 0..BOARD {
        scenario
            .add_creature(P0, &format!("Costly Engine {i}"), 1, 1)
            .with_ability_definition(draw_for_life(5));
    }
    scenario.build()
}

/// The same board, every ability AFFORDABLE — the positive control for the tail.
fn affordable_board() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    for i in 0..BOARD {
        scenario
            .add_creature(P0, &format!("Costly Engine {i}"), 1, 1)
            .with_ability_definition(draw_for_life(1));
    }
    scenario.build()
}

/// CR 613.1: genuinely flush the board, which leaves `layers_dirty` `Clean`.
/// Done through the real `flush_layers` rather than by assigning the enum, so
/// the "clean" precondition is a fact about the state, not a claim about it.
fn flushed_clean(runner: &mut GameRunner) {
    flush_layers(runner.state_mut());
    assert!(
        !runner.state().layers_dirty.is_dirty(),
        "precondition: the board is layers-clean after an explicit flush"
    );
}

/// Row 11: zero flush-clones on a layers-clean state; exactly one when dirty.
/// The counter demonstrably fires both ways, so the `0` is not vacuous.
#[test]
fn display_read_out_takes_no_flush_clone_on_a_clean_state() {
    let mut runner = unaffordable_board();

    // CR 613.1: clean board — no flush clone.
    flushed_clean(&mut runner);
    perf_counters::reset();
    let clean_map = activation_block_reasons(runner.state());
    let clean = perf_counters::snapshot();
    assert_eq!(
        clean.priority_cast_probe_state_clones, 0,
        "CR 613.1: a layers-clean board is already the flushed board — zero whole-state clones"
    );
    assert!(
        !clean_map.is_empty(),
        "reach-guard: the traversal genuinely ran and produced entries — a 0 on an \
         empty board would be vacuous"
    );

    // Dirty board — exactly one flush clone, so the counter fires both ways.
    let mut runner = unaffordable_board();
    runner.state_mut().layers_dirty.mark_full();
    perf_counters::reset();
    let dirty_map = activation_block_reasons(runner.state());
    let dirty = perf_counters::snapshot();
    assert_eq!(
        dirty.priority_cast_probe_state_clones, 1,
        "paired positive: a dirty board costs exactly ONE whole-state flush clone per call"
    );
    assert!(
        !dirty_map.is_empty(),
        "reach-guard: the dirty-board traversal produced entries too"
    );
}

/// Row 12: the shared `activation_verdict` core runs exactly once per examined
/// ability. The examined counter is the paired instrument that makes the ratio
/// attributable rather than coincidental.
#[test]
fn the_verdict_core_runs_once_per_examined_ability() {
    let mut runner = unaffordable_board();
    flushed_clean(&mut runner);
    perf_counters::reset();
    let map = activation_block_reasons(runner.state());
    let snap = perf_counters::snapshot();

    assert_eq!(
        snap.activation_block_display_abilities_examined, BOARD as u64,
        "paired positive: the traversal examined every ability on the board"
    );
    assert_eq!(
        snap.activation_verdict_passes, BOARD as u64,
        "the verdict core ran exactly once per examined ability — a two-pass \
         mechanism would report ~2x"
    );
    assert_eq!(
        map.values().map(Vec::len).sum::<usize>(),
        BOARD,
        "reach-guard: every examined ability produced a read-out entry"
    );
}

/// Row 13: `legal_actions_full` does not pay for the display tail — the tail
/// lives behind a separate entry point, so a non-display caller cannot enter it.
///
/// **The structural claim, with both halves non-zero somewhere so neither is
/// vacuous:** `legal_actions_full` reports ZERO display-traversal abilities
/// examined while running a non-zero number of verdict-core passes (so the
/// enforcement gate genuinely ran), and `activation_block_reasons` on the SAME
/// board reports `BOARD` examined.
///
/// ── A CORRECTION TO THE PLAN'S COST MODEL, measured here rather than assumed ──
///
/// The plan's §Cost table predicts "unaffordable, resource verdict → one dry-run
/// clone plus the target-legality tail", and specifies a positive control in
/// which an ALL-AFFORDABLE board reports a *strictly smaller* tail count. Both
/// are the wrong way round for `state_clone_for_legality`, and this test pins
/// the measured truth instead:
///
/// `costs::can_pay` consults the cheap `is_payable_for_activation` gate FIRST
/// and returns `false` from it without cloning; the counted
/// `record_state_clone_for_legality()` + `state.clone()` dry run sits BELOW that
/// early return. So a `PayLife 5` shortfall at 1 life costs **zero** clones,
/// while the same ability at 20 life — where the cheap gate passes and the dry
/// run must run — costs **one per ability**.
///
/// SCOPE OF THAT CLAIM — it is a property of the COST CLASS, not of the counter.
/// It holds only for costs whose `is_payable_for_activation` arm can answer
/// `false`. `AbilityCost::Mana` is the counter-case: its arm returns `true`
/// unconditionally (`cost_payability.rs`, "CR 601.2g: mana affordability is
/// checked by the mana payment step"), and a bare `Mana` cost matches neither
/// the `OneOf` nor the bare-`Waterbend` branch, so it reaches the counted clone
/// on BOTH the affordable and the unaffordable path. `Mana` is the shape in the
/// reported Sliver Overlord defect (`{3}:`), so it is pinned separately by
/// `a_mana_cost_reaches_the_dry_run_on_both_paths` below rather than left to
/// this `PayLife` fixture to imply.
///
/// The tail itself is free from this entry point on any board: CR 613.1 means a
/// clean board needs no clone, and on a dirty board
/// `activation_block_reasons` flushes ONCE up front (row 11), leaving the state
/// clean for every per-ability tail evaluation underneath it.
///
/// The tail's real discriminating test is row 7 in `ability_cost_block_readout.rs`
/// (an unaffordable ability with no legal target is not read out), which fails
/// if the tail is removed. This file pins the clone budget; that file pins the
/// tail's behaviour.
#[test]
fn legal_actions_full_does_not_pay_for_the_display_tail() {
    let mut runner = unaffordable_board();
    flushed_clean(&mut runner);
    perf_counters::reset();
    let (_actions, _costs, _grouped) = legal_actions_full(runner.state());
    let enforcement = perf_counters::snapshot();

    let mut runner = unaffordable_board();
    flushed_clean(&mut runner);
    perf_counters::reset();
    let map = activation_block_reasons(runner.state());
    let display_unaffordable = perf_counters::snapshot();

    let mut runner = affordable_board();
    flushed_clean(&mut runner);
    perf_counters::reset();
    let affordable_map = activation_block_reasons(runner.state());
    let display_affordable = perf_counters::snapshot();

    // ── the structural claim ────────────────────────────────────────────────
    assert_eq!(
        enforcement.activation_block_display_abilities_examined, 0,
        "row 13: `legal_actions_full` never enters the display traversal"
    );
    assert!(
        enforcement.activation_verdict_passes > 0,
        "paired positive (mandatory): the enforcement gate genuinely ran on this board, \
         so the zero above is not vacuous; got {}",
        enforcement.activation_verdict_passes
    );
    assert_eq!(
        display_unaffordable.activation_block_display_abilities_examined, BOARD as u64,
        "paired positive (mandatory): the display entry point DOES examine the board"
    );
    assert!(
        !map.is_empty(),
        "reach-guard: the display call produced entries"
    );
    assert!(
        affordable_map.is_empty(),
        "reach-guard: the all-affordable control produced NO entries — nothing to explain"
    );

    // ── the corrected cost model, pinned ────────────────────────────────────
    assert_eq!(
        display_unaffordable.state_clone_for_legality, 0,
        "a cheap-gate refusal (PayLife 5 at 1 life) costs ZERO dry-run clones — \
         `can_pay` returns before `record_state_clone_for_legality`"
    );
    assert_eq!(
        display_affordable.state_clone_for_legality, BOARD as u64,
        "and the AFFORDABLE board costs one dry-run clone per ability, because the \
         cheap gate passes and the dry run must run — for THIS cost class the counter \
         sees only the dry run's success path, never the tail"
    );
    assert!(
        display_affordable.state_clone_for_legality > display_unaffordable.state_clone_for_legality,
        "direction pinned explicitly FOR A CHEAP-GATE-REFUSABLE COST (PayLife): the \
         OPPOSITE of the plan's stated positive control ({} affordable vs {} \
         unaffordable). This inequality is a property of the fixture's cost class, \
         not of the counter — an all-`Mana` board makes the two sides EQUAL, which \
         `a_mana_cost_reaches_the_dry_run_on_both_paths` pins.",
        display_affordable.state_clone_for_legality,
        display_unaffordable.state_clone_for_legality,
    );
    assert_eq!(
        display_unaffordable.priority_cast_probe_state_clones, 0,
        "and no whole-state flush clone on a clean board (row 11's claim, restated \
         here so the two counters are read together as MAT-3 requires)"
    );
    assert_eq!(
        display_unaffordable.activation_verdict_flush_clones, 0,
        "CR 613.1: the gate's own target-legality tail takes no flush clone either — \
         the entry point already flushed, so every per-ability tail evaluation \
         underneath it sees a clean board"
    );
}

/// CR 613.1: the activation gate's tail clone is now CONDITIONAL and COUNTED.
///
/// Before this change it cloned the whole `GameState` unconditionally and
/// incremented no counter, so no budget test could see it. Two claims, with the
/// counter demonstrably firing both ways:
///
///   * on a layers-DIRTY state reached through the enforcement shim, the tail
///     clones once per examined ability — the pre-existing cost, now visible;
///   * on a layers-CLEAN state it clones zero times — the cost this change
///     actually removes.
///
/// It is deliberately its own counter rather than `state_clone_for_legality`:
/// that field is a per-candidate budget consumed by shipped memo tests in
/// `ai_support::filter`, and folding a newly-counted clone into it would double
/// their expected budgets for a pure instrumentation change.
#[test]
fn the_gate_tail_clone_is_conditional_on_layers_dirty_and_separately_counted() {
    use engine::game::casting::can_activate_ability_now;

    // AFFORDABLE abilities, deliberately: under `ActivationQuery::Legality` an
    // UNAFFORDABLE ability early-returns at the CR 118.3 exit and never reaches
    // the tail at all — which is exactly the property row 13 describes, and
    // which would make this measurement read 0 for the wrong reason.
    //
    // Dirty board: the tail clone fires, once per examined ability.
    let mut runner = affordable_board();
    runner.state_mut().layers_dirty.mark_full();
    perf_counters::reset();
    for i in 0..BOARD {
        let id = *runner
            .state()
            .battlefield
            .get(i)
            .expect("board is populated");
        let _ = can_activate_ability_now(runner.state(), P0, id, 0);
    }
    let dirty = perf_counters::snapshot();
    assert_eq!(
        dirty.activation_verdict_flush_clones, BOARD as u64,
        "paired positive: on a dirty board the tail clones once per examined ability"
    );
    // The affordable cost passes `can_pay`'s cheap gate, so its dry run DOES
    // spend one `state_clone_for_legality` per ability. The discriminating claim
    // is that the two clones are counted SEPARATELY: had the tail been folded
    // into this field it would read `2 * BOARD` here, which is exactly the
    // silent doubling that would have broken the shipped `ai_support::filter`
    // memo budgets.
    assert_eq!(
        dirty.state_clone_for_legality, BOARD as u64,
        "the dry-run budget counts the dry run ONLY — not the dry run plus the tail"
    );

    // Clean board: zero. Same call, same abilities — only `layers_dirty` differs.
    let mut runner = affordable_board();
    flushed_clean(&mut runner);
    perf_counters::reset();
    for i in 0..BOARD {
        let id = *runner
            .state()
            .battlefield
            .get(i)
            .expect("board is populated");
        let _ = can_activate_ability_now(runner.state(), P0, id, 0);
    }
    let clean = perf_counters::snapshot();
    assert_eq!(
        clean.activation_verdict_flush_clones, 0,
        "CR 613.1: a clean board is already the flushed board — the clone this \
         change removes"
    );
    assert_eq!(
        clean.activation_verdict_passes, BOARD as u64,
        "reach-guard: the gate genuinely ran once per ability, so the 0 above is \
         not vacuous"
    );
}

/// §Cost B3(a) — THE MEASUREMENT, committed as a re-runnable instrument.
///
/// `#[ignore]`d so it never costs CI time; run it with
/// `cargo nextest run -p phase-engine -E 'test(cost_measurement)' --run-ignored all --no-capture`.
///
/// Board: 4-player Commander, >=100 permanents across >=2 seats, every ability
/// unaffordable and TARGETED so the target-legality tail is exercised. Paired:
/// both entry points, same run, same binary, same board. Both clone counters are
/// reported (MAT-3: wall time captures both and attributes neither), plus the
/// board's `layers_dirty`, without which the flush-clone number is unreadable.
///
/// Positive control: the same instrument on an ALL-AFFORDABLE board, reported
/// beside it so the reader can see the counters move rather than taking a single
/// number on trust.
///
/// PROFILE CAVEAT, stated rather than implied: this runs under the `test`
/// profile (unoptimized + debuginfo). The CLONE COUNTS are profile-independent
/// and are the attributable half; the WALL TIMES are debug-profile numbers and
/// must NOT be quoted as release figures.
#[test]
#[ignore = "measurement instrument, not an assertion; see the doc comment"]
fn cost_measurement_display_entry_point_versus_legal_actions_full() {
    use std::time::Instant;

    /// 4-player Commander, `per_seat` permanents on each of two seats.
    fn commander_board(per_seat: usize, life: i32, cost: &AbilityCost) -> GameRunner {
        let mut scenario = GameScenario::new_n_player(4, 7);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, life);
        for seat in [P0, engine::game::scenario::P1] {
            for i in 0..per_seat {
                scenario
                    .add_creature(seat, &format!("Costly Engine {seat:?} {i}"), 1, 1)
                    // TARGETED, so the tail has real work to do.
                    .with_ability_definition(
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::Destroy {
                                target: TargetFilter::Any,
                                cant_regenerate: false,
                            },
                        )
                        .cost(cost.clone()),
                    );
            }
        }
        scenario.build()
    }

    let mut report = String::new();
    // The cost class is a measured axis, not a constant: `PayLife` is refused by
    // the cheap gate and clones nothing, while `Mana` — the shape in the reported
    // Sliver Overlord defect — reaches the dry run on both paths. Measuring only
    // `PayLife` reports the one class that does NOT behave like the bug.
    for (label, cost, life) in [
        (
            "UNAFFORDABLE PayLife (cheap-gate refusal)",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 5 },
            },
            1,
        ),
        (
            "ALL-AFFORDABLE PayLife (positive control)",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            40,
        ),
        (
            "UNAFFORDABLE Mana (the reported bug's cost shape)",
            AbilityCost::Mana {
                cost: ManaCost::generic(7),
            },
            40,
        ),
    ] {
        for dirty in [false, true] {
            let mut runner = commander_board(55, life, &cost);
            if dirty {
                runner.state_mut().layers_dirty.mark_full();
            } else {
                flushed_clean(&mut runner);
            }
            let permanents = runner.state().battlefield.len();

            perf_counters::reset();
            let t0 = Instant::now();
            let blocked = activation_block_reasons(runner.state());
            let display_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let d = perf_counters::snapshot();

            let mut runner2 = commander_board(55, life, &cost);
            if dirty {
                runner2.state_mut().layers_dirty.mark_full();
            } else {
                flushed_clean(&mut runner2);
            }
            perf_counters::reset();
            let t1 = Instant::now();
            let (actions, _costs, _grouped) = legal_actions_full(runner2.state());
            let enforce_ms = t1.elapsed().as_secs_f64() * 1000.0;
            let e = perf_counters::snapshot();

            report.push_str(&format!(
                "\n=== {label} | layers_dirty={} | permanents={permanents} ===\n\
                 activation_block_reasons : {display_ms:8.2} ms | entries={:5} \
                 state_clone_for_legality={:5} priority_cast_probe_state_clones={:3} \
                 activation_verdict_flush_clones={:5} verdict_passes={:5} examined={:5}\n\
                 legal_actions_full       : {enforce_ms:8.2} ms | actions={:5} \
                 state_clone_for_legality={:5} priority_cast_probe_state_clones={:3} \
                 activation_verdict_flush_clones={:5} verdict_passes={:5} examined={:5}\n",
                if dirty { "Full " } else { "Clean" },
                blocked.values().map(Vec::len).sum::<usize>(),
                d.state_clone_for_legality,
                d.priority_cast_probe_state_clones,
                d.activation_verdict_flush_clones,
                d.activation_verdict_passes,
                d.activation_block_display_abilities_examined,
                actions.len(),
                e.state_clone_for_legality,
                e.priority_cast_probe_state_clones,
                e.activation_verdict_flush_clones,
                e.activation_verdict_passes,
                e.activation_block_display_abilities_examined,
            ));
        }
    }
    println!("{report}");
}

/// `BOARD` creatures, each with one unaffordable `{7}` ability and no mana
/// sources anywhere. `AbilityCost::Mana` is the cost shape in the reported
/// Sliver Overlord defect (`{3}:`).
fn unaffordable_mana_board() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..BOARD {
        scenario
            .add_creature(P0, &format!("Mana Engine {i}"), 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Mana {
                    cost: ManaCost::generic(7),
                }),
            );
    }
    scenario.build()
}

/// The cost-class boundary of `state_clone_for_legality`, measured on both sides
/// in one test so neither number can be read without the other.
///
/// `AbilityCost::Mana`'s `is_payable_for_activation` arm returns `true`
/// unconditionally, so a mana shortfall is detected only by the dry run — which
/// clones. A `PayLife` shortfall is caught by the cheap gate and clones nothing.
/// The two therefore disagree on the SAME entry point and the SAME board size,
/// which is why `legal_actions_full_does_not_pay_for_the_display_tail`'s
/// inequality is scoped to its fixture's cost class rather than stated of the
/// counter.
///
/// Consequence worth stating plainly: on a mana-cost board the display traversal
/// pays one whole-`GameState` clone per examined ability regardless of
/// affordability. It stays off the AI-search hot path (`legal_actions_full` is
/// byte-unchanged in what it computes) and is bounded, but it is not free, and
/// `Mana` is the dominant activated-cost shape.
#[test]
fn a_mana_cost_reaches_the_dry_run_on_both_paths() {
    let mut mana_runner = unaffordable_mana_board();
    flushed_clean(&mut mana_runner);
    perf_counters::reset();
    let mana_map = activation_block_reasons(mana_runner.state());
    let mana = perf_counters::snapshot();

    let mut life_runner = unaffordable_board();
    flushed_clean(&mut life_runner);
    perf_counters::reset();
    let life_map = activation_block_reasons(life_runner.state());
    let life = perf_counters::snapshot();

    // Reach-guards first: both boards must actually produce a full read-out, or
    // the clone counts below are measuring a traversal that never happened.
    assert_eq!(
        mana.activation_block_display_abilities_examined, BOARD as u64,
        "reach-guard: the mana board's abilities were all examined"
    );
    assert_eq!(
        life.activation_block_display_abilities_examined, BOARD as u64,
        "reach-guard: the life board's abilities were all examined"
    );
    assert_eq!(
        mana_map.values().map(Vec::len).sum::<usize>(),
        BOARD,
        "reach-guard: every unaffordable mana ability is read out"
    );
    assert_eq!(
        life_map.values().map(Vec::len).sum::<usize>(),
        BOARD,
        "reach-guard: every unaffordable life ability is read out"
    );

    assert_eq!(
        mana.state_clone_for_legality, BOARD as u64,
        "an UNAFFORDABLE mana cost still pays one dry-run clone per ability — \
         `is_payable_for_activation` returns true unconditionally for `Mana`, so the \
         shortfall is only detectable below the counted clone"
    );
    assert_eq!(
        life.state_clone_for_legality, 0,
        "paired contrast (mandatory): the same entry point on the same board size \
         with a cheap-gate-refusable cost pays ZERO — so the number above is a fact \
         about the COST CLASS, not about the entry point"
    );
}
