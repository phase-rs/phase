//! Portable stack **high-water measurement** for the four-player Commander
//! fixture that `game_state_stack_budget.rs` guards with a survive/abort bit.
//!
//! This is the instrument that file's module docs record as the better
//! available design and deliberately do not build:
//!
//! > Spawn a thread with a known large stack, fill it with a sentinel pattern,
//! > run the fixture, then scan for the deepest disturbed byte. That yields a
//! > *number* rather than the survive/abort bit this test produces.
//!
//! Why it matters here: `game_state_stack_budget.rs` is
//! `#![cfg(all(target_arch = "aarch64", target_os = "macos"))]`, and this
//! repository's CI has no macOS runner — every workflow in `.github/workflows/`
//! is `ubuntu-latest` or an Android cross-build — so that guard has **never
//! executed in CI**. It compiles out on every job. A measurement asserts on a
//! value rather than on a target-calibrated bound, so it runs everywhere the
//! engine builds, this repository's CI included.
//!
//! # Method
//!
//! 1. Spawn a thread with an explicit `MEASURE_STACK_BYTES` stack.
//! 2. Take `anchor`, the address of a local in the measuring frame. The stack
//!    grows down, so every frame the fixture pushes lies below it.
//! 3. Paint `[low, anchor - FRAME_GAP)` with `SENTINEL`. `FRAME_GAP` keeps the
//!    measuring frame's own live locals out of the painted region; `low` keeps
//!    `GUARD_MARGIN` of clearance above the guard page.
//! 4. Run the fixture.
//! 5. Scan up from `low` for the deepest byte that is no longer `SENTINEL`.
//!
//! # What the number is, and is not
//!
//! It is the deepest byte the fixture **wrote**, relative to `anchor`. Two
//! honest caveats, neither of which affects its use as a regression signal:
//!
//! - **It is a lower bound.** A frame that reserves space without writing every
//!   byte of it leaves sentinel behind, so the true reserved depth can exceed
//!   the measured one.
//! - **It cannot see shallower than `FRAME_GAP`.** If the fixture never got
//!   below that, the scan finds nothing disturbed and the result saturates.
//!   The reach-guards below, plus an explicit saturation assertion, exist so
//!   that case reports "the paint or the scan is broken", not "the path is
//!   cheap".
//!
//! Absolute values are target-dependent (ABI, register pressure, spill
//! decisions) and `[profile.test]` inherits `dev`, so nothing is optimized
//! away. Compare numbers **within** a target, never across targets.
//!
//! # Why the assertion is loose, and what it is for
//!
//! The number is the product; [`HIGH_WATER_CEILING_BYTES`] only stops the run
//! from being decoration. A *tight* ratchet — assert within a few percent of a
//! recorded value — is what would actually catch drift, and it is deliberately
//! not what this file does, because a recorded value is a per-target constant
//! and one portable assertion cannot carry a table of them. That is the same
//! trap `game_state_stack_budget.rs` fell into: it took a number measured on
//! one target, and to stay honest about it had to `cfg` itself down to that
//! target, where CI never runs it.
//!
//! So the split is deliberate: **assert** the thing that is true on every
//! target (a single un-nested `apply()` must not approach the production stack
//! budget), and **print** the thing that is only comparable within a target.
//! Tightening the printed number into a per-target ratchet is the follow-up
//! once it has a run history in CI to set rows from — see the drift figures in
//! <https://github.com/phase-rs/phase/issues/8376>.
//!
//! # A note on severity, so nobody reads this test as a fire alarm
//!
//! `game_state_stack_budget.rs`'s 3 MiB bound is **not** the production stack
//! budget. Its module docs are explicit that 3 MiB was chosen to sit inside a
//! `[2,560 KiB, 3,328 KiB]` window so that reverting the `ResolvedAbility`
//! boxing would flip the test red — a discrimination dial, not a safety
//! margin. The production budget is `RUNTIME_THREAD_STACK_BYTES` in
//! `phase-server/src/main.rs`: **32 MiB**, on the runtime owner thread and on
//! every Tokio worker and blocking thread.
//!
//! That distinction is why this file's ceiling is nowhere near 3 MiB. Crossing
//! 3 MiB means the *discriminating gate* has lost its calibration, which is
//! worth knowing and is what issue #8376 reports. Crossing this file's ceiling
//! would mean something much worse.
//!
//! The 32 MiB figure is also not headroom to spend: the server docs record
//! that AI search keeps one live `Option<GameState>` per ply and that search
//! depth is data-driven, so production depth is this fixture's number times a
//! multiplier no static bound covers. The risk that matters is the multiplier,
//! not the linear drift.

#![cfg(target_pointer_width = "64")]

use std::ptr;
use std::sync::atomic::{compiler_fence, Ordering};

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{PlayerId, WaitingFor};

/// Large enough that the fixture cannot reach the guard page, so the run
/// produces a number instead of the SIGABRT this instrument exists to replace.
///
/// Deliberately **larger** than production's 32 MiB: a measurement that aborts
/// has measured nothing, and the join below distinguishes "overran a budget"
/// from "recurses without bound" only if this stack is not itself the bound.
const MEASURE_STACK_BYTES: usize = 64 << 20;

/// Ceiling for the measured high-water. Fails the test, portably, on every
/// target the engine builds for.
///
/// **Derivation, so this is a stated policy and not a magic number.**
/// Production runs the engine on 32 MiB threads (`RUNTIME_THREAD_STACK_BYTES`,
/// `phase-server/src/main.rs`). This fixture measures **one** `apply()` with no
/// AI search above it, while production nests several live `GameState` slots
/// and one more per AI ply on the same frame chain. A quarter of the production
/// stack for the un-nested case leaves the nesting somewhere to go.
///
/// Headroom at the time of writing: 2,493 KiB measured on
/// `x86_64-unknown-linux-gnu` debug, so this fires at ~3.3x the current value.
/// It will not flake on a target that runs a couple of hundred KiB deeper —
/// and the slack is deliberate, because this number moves. The same fixture
/// measured 2,915 KiB fifteen commits earlier.
///
/// **If this fires**, do not raise it as the first move. It means either a new
/// recursion on the resolution path or a per-frame cost that has grown by
/// nearly 3x — both are findings, not calibration errors.
const HIGH_WATER_CEILING_BYTES: usize = 8 << 20;

/// Clearance kept above the low end of the thread stack. Covers the guard page
/// and whatever std's thread-entry frames sit below `anchor`, so the paint
/// never writes outside the mapping.
const GUARD_MARGIN: usize = 256 << 10;

/// Clearance kept below `anchor`. The measuring frame's own locals live in
/// here and must survive the paint; the cost is that depths shallower than
/// this are indistinguishable.
const FRAME_GAP: usize = 32 << 10;

const SENTINEL: u8 = 0xAB;
const SENTINEL_WORD: u64 = u64::from_ne_bytes([SENTINEL; 8]);

// Fixture text copied verbatim from `game_state_stack_budget.rs` so the two
// measure the same path. A death trigger on every seat makes the measured
// resolution a real trigger cascade rather than a bare removal.
const MURDER_ORACLE: &str = "Destroy target creature.";
const DEATH_TRIGGER_ORACLE: &str =
    "Whenever this creature or another creature dies, you gain 1 life.";

/// What the measuring thread hands back.
struct Measurement<R> {
    runner: R,
    /// Deepest written byte, as a depth below `anchor`.
    high_water: usize,
    /// The depth the scan saturates at, i.e. what `high_water` equals when
    /// nothing in the painted region was disturbed. Derived from the *aligned*
    /// `hi`, not from `FRAME_GAP`, because rounding moves it by up to 7 bytes
    /// and a saturation check against the unrounded constant would miss.
    saturation_floor: usize,
}

/// Paints `[low, hi)` with `SENTINEL`.
///
/// # Safety
///
/// `[low, hi)` must lie inside the current thread's stack mapping, strictly
/// below every live local of the caller's frame and strictly above the guard
/// page. The caller derives both bounds from `anchor` for exactly that reason.
unsafe fn paint(low: usize, hi: usize) {
    ptr::write_bytes(low as *mut u8, SENTINEL, hi - low);
}

/// Lowest address in `[low, hi)` whose byte is no longer `SENTINEL`, or `hi`
/// if the whole region survived.
///
/// Word-scans first so 64 MiB is cheap, then narrows to the byte. Volatile
/// reads because the writes it is looking for happened through frames the
/// compiler has every right to believe are dead.
///
/// # Safety
///
/// Same contract as [`paint`]: `[low, hi)` must be the region just painted.
unsafe fn deepest_disturbed(low: usize, hi: usize) -> usize {
    let mut addr = low;
    while addr + 8 <= hi {
        if ptr::read_volatile(addr as *const u64) != SENTINEL_WORD {
            for byte in addr..addr + 8 {
                if ptr::read_volatile(byte as *const u8) != SENTINEL {
                    return byte;
                }
            }
        }
        addr += 8;
    }
    while addr < hi {
        if ptr::read_volatile(addr as *const u8) != SENTINEL {
            return addr;
        }
        addr += 1;
    }
    hi
}

#[test]
fn four_player_commander_action_stack_high_water() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for seat in [P0, P1, PlayerId(2), PlayerId(3)] {
        scenario.add_creature_from_oracle(seat, "Zulaport Cutthroat", 0, 1, DEATH_TRIGGER_ORACLE);
        scenario.add_vanilla(seat, 2, 2);
        scenario.add_vanilla(seat, 3, 3);
    }
    let victim = scenario.add_creature(P1, "Doomed Bystander", 4, 4).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();

    // Reach-guards: without these, a fixture that never reached the cast would
    // measure a no-op and report a flatteringly small high-water.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "reach-guard: P0 holds priority before the measured action, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&victim)
            .map(|object| object.zone),
        Some(Zone::Battlefield),
        "reach-guard: the removal target is on the battlefield before the action"
    );
    let life_before = runner.life(P0);

    let handle = std::thread::Builder::new()
        .stack_size(MEASURE_STACK_BYTES)
        .spawn(move || {
            let anchor_local = 0u8;
            let anchor = &anchor_local as *const u8 as usize;
            // Word-align both ends: `deepest_disturbed` scans `u64`s, and
            // `anchor` is the address of a `u8` so it carries no alignment.
            let low = (anchor + GUARD_MARGIN - MEASURE_STACK_BYTES).next_multiple_of(8);
            let hi = (anchor - FRAME_GAP) & !7;

            // The paint's safety contract says `[low, hi)` holds nothing live.
            // `runner` is the one capture large enough to reach past FRAME_GAP
            // if it were laid out below `anchor`, and painting over it would
            // corrupt the fixture silently rather than fail. Check rather than
            // reason about a layout no rule pins: whether a captured value
            // lands in this frame or in the caller's is the compiler's choice.
            let runner_lo = ptr::addr_of!(runner) as usize;
            let runner_hi = runner_lo + std::mem::size_of_val(&runner);
            assert!(
                runner_hi <= low || runner_lo >= hi,
                "the fixture ({runner_lo:#x}..{runner_hi:#x}) overlaps the paint \
                 region ({low:#x}..{hi:#x}); raise FRAME_GAP rather than \
                 painting over live state"
            );

            // SAFETY: `anchor` is a local of this frame, so the stack mapping
            // runs from below it down to `anchor - MEASURE_STACK_BYTES` plus
            // whatever std's entry frames occupy above `anchor`. `low` sits
            // `GUARD_MARGIN` above that floor and `hi` sits `FRAME_GAP` below
            // this frame's locals, so `[low, hi)` is stack this thread owns and
            // nothing live occupies — the assertion above checks the one
            // capture big enough to make that last clause false.
            unsafe { paint(low, hi) };
            compiler_fence(Ordering::SeqCst);

            runner.cast(murder).target_objects(&[victim]).resolve();
            runner.advance_until_stack_empty();

            compiler_fence(Ordering::SeqCst);
            // SAFETY: same region just painted, still owned by this thread.
            let deepest = unsafe { deepest_disturbed(low, hi) };

            Measurement {
                runner,
                high_water: anchor - deepest,
                saturation_floor: anchor - hi,
            }
        })
        .expect("spawn measuring thread");

    let measurement = handle.join().expect(
        "the measured action panicked. Unlike game_state_stack_budget.rs this \
         thread has 64 MiB — more than production's 32 MiB — so an overflow \
         here would mean genuinely unbounded recursion rather than a budget \
         overrun.",
    );
    let Measurement {
        runner,
        high_water,
        saturation_floor,
    } = measurement;

    // Positive outcome assertions: the measured action really did resolve.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&victim)
            .map(|object| object.zone),
        Some(Zone::Graveyard),
        "Murder resolved and put the target in the graveyard"
    );
    assert!(
        runner.life(P0) > life_before,
        "the death triggers resolved and gained P0 life (before {life_before}, after {})",
        runner.life(P0)
    );

    // Saturation guard: a result pinned at the floor means the scan found
    // nothing disturbed, which the reach-guards above say cannot be "the
    // fixture was cheap". It would mean the paint or the scan is wrong.
    assert!(
        high_water > saturation_floor,
        "measurement did not discriminate: high-water saturated at its floor \
         ({saturation_floor} B), so the paint/scan is broken rather than the \
         path cheap"
    );

    // The portable assertion. See HIGH_WATER_CEILING_BYTES for the derivation
    // and for why raising it is the wrong first response.
    assert!(
        high_water <= HIGH_WATER_CEILING_BYTES,
        "four-player Commander cast + resolve used {high_water} B ({} KiB) of \
         stack, over the {HIGH_WATER_CEILING_BYTES} B ({} KiB) ceiling — a \
         quarter of phase-server's 32 MiB RUNTIME_THREAD_STACK_BYTES. \
         Production nests several live GameState slots and one per AI ply on \
         this same chain, so this is a finding about the resolution path, not \
         a stale bound.",
        high_water / 1024,
        HIGH_WATER_CEILING_BYTES / 1024
    );

    // The product. Stable, greppable, and the only line here that is worth
    // comparing across commits — within a target.
    println!(
        "STACK_HIGH_WATER_BYTES={high_water} ({} KiB)",
        high_water / 1024
    );
}
