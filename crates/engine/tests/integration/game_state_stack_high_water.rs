//! Stack **high-water measurement** for the four-player Commander fixture that
//! `game_state_stack_budget.rs` guards with a survive/abort bit.
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
//! executed in CI**. It compiles out on every job. This file's assertion is on
//! a measured value rather than on a target-calibrated bound, so it runs
//! wherever its stack-bounds query is known good — CI's `ubuntu-latest` x86_64
//! included, which is what the sibling guard never managed.
//!
//! # Which targets, and why only one
//!
//! 64-bit Linux — see the `cfg` below. Two things this gate is *not*:
//!
//! It is not a calibration gate like the sibling file's. Nothing here is tuned
//! to a target; the assertion is on a measured value and the safety argument is
//! on the ABI, so neither needs a bisection to move.
//!
//! It is not a claim that other platforms cannot work. `pthread_getattr_np` is
//! in Bionic, and Darwin has `pthread_get_stackaddr_np` /
//! `pthread_get_stacksize_np` with the opposite convention — it returns the
//! high end. An earlier revision of this file enabled Android and macOS on that
//! basis. They came back out because nobody has *run* it there, and a
//! stack-bounds query that is merely plausible is the same species of claim
//! this file exists to replace.
//!
//! **To add a target:** add its arm to [`stack_bounds`], widen the `cfg`, and
//! run the test on that target — not a cross-build, an execution. The runtime
//! checks in [`stack_bounds`] and [`paint_below_this_frame`] are designed to
//! fail loudly on a target whose assumptions do not hold, so the cost of trying
//! is a red test rather than a corrupted one. CI's `ubuntu-latest` x86_64 is
//! covered, which is the gap this file was written to close.
//!
//! # Method
//!
//! 1. Spawn a thread with an explicit `MEASURE_STACK_BYTES` stack.
//! 2. Ask the platform for that thread's real stack bounds, and check the
//!    answer against a local of the asking frame before trusting it.
//! 3. Descend one frame into [`measure`], leaving the fixture's `GameRunner` in
//!    the caller's frame, and paint `[low, hi)` with `SENTINEL`.
//! 4. Run the fixture.
//! 5. Scan up from `low` for the deepest byte that is no longer `SENTINEL`.
//!
//! # Why the paint cannot corrupt live state
//!
//! Painting a span of the running thread's own stack earns its safety
//! argument, so here it is in full. Three claims, each checked at runtime
//! rather than assumed:
//!
//! - **The span is inside the mapping.** `low` comes from
//!   `pthread_getattr_np` / `pthread_get_stackaddr_np` — the thread's real
//!   bounds — not from `anchor` arithmetic against the requested
//!   `Builder::stack_size`, which is a *request* and tells you nothing about
//!   where the allocation landed or how the guard page was carved out of it.
//!   [`stack_bounds`] then refuses to return a range that does not contain a
//!   local of its caller's frame, so a platform whose query describes some
//!   other stack fails loudly instead of handing back an address to write to.
//!
//! - **Only values live at paint time can be damaged.** Everything the fixture
//!   allocates *after* the paint — every frame it pushes, every temporary in
//!   the `cast(..).resolve()` chain — is written into the painted region on
//!   purpose. That is the measurement. So the question is only ever "where do
//!   the values alive at this instant sit".
//!
//! - **The ceiling is a frame boundary, so that question has a structural
//!   answer.** `hi` is computed inside [`paint_below_this_frame`], one call
//!   *below* [`measure`]. The stack grows down, so every frame above that one —
//!   [`measure`]'s locals and spill slots, the closure's, the thread entry's —
//!   is at a higher address than `hi` by the calling convention. Nothing has to
//!   be enumerated, which is the point: an enumeration can only list values
//!   that have names in the source, and the ones easiest to miss are compiler
//!   temporaries and spill slots, which do not. Stack-growth direction is an
//!   ABI property rather than a Rust guarantee, so it is asserted at runtime
//!   too, against a local of the calling frame.
//!
//! Two things do not follow from the frame boundary and are therefore checked
//! separately: [`PAINT_CLEARANCE`] covers the painting function's own frame and
//! the `memset` frame `write_bytes` pushes *below* it, and the `GameRunner` is
//! reached through a `&mut`, so its pointee is asserted outside the region —
//! the reference proves nothing about where the referent lives. Holding it by
//! reference from a shallower frame is deliberate: whether a captured value
//! lands in the closure's frame or its caller's is the compiler's choice, and
//! this removes the choice instead of betting on it.
//!
//! # What the number is, and is not
//!
//! It is the deepest byte the fixture **wrote**, relative to the painting
//! frame. Two honest caveats, neither of which affects its use as a regression
//! signal:
//!
//! - **It is a lower bound.** A frame that reserves space without writing every
//!   byte of it leaves sentinel behind, so the true reserved depth can exceed
//!   the measured one.
//! - **It cannot see shallower than [`PAINT_CLEARANCE`].** If the fixture never got
//!   below that, the scan finds nothing disturbed and the result saturates.
//!   The reach-guards below, plus an explicit saturation assertion, exist so
//!   that case reports "the paint or the scan is broken", not "the path is
//!   cheap".
//!
//! Absolute values are target-dependent (ABI, register pressure, spill
//! decisions) and `[profile.test]` inherits `dev`, so nothing is optimized
//! away. Compare numbers **within** a target, never across targets.
//!
//! # Reading the number
//!
//! It is printed, and both harnesses hide stdout for a test that passes — which
//! this one normally does. To actually see it:
//!
//! ```text
//! cargo test -p phase-engine --test integration -- --nocapture \
//!     --exact game_state_stack_high_water::four_player_commander_action_stack_high_water
//! cargo nextest run -p phase-engine --success-output immediate -E 'test(stack_high_water)'
//! ```
//!
//! So a default CI run asserts the ceiling but does not surface the value.
//! Surfacing it per-run — and ratcheting on it — is the follow-up this file is
//! the prerequisite for, not something it claims to have done.
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
//! supported target (a single un-nested `apply()` must not approach the
//! production stack budget), and **print** the thing that is only comparable
//! within a target.
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
//! The 32 MiB figure is also not headroom to spend: the server docs record
//! that AI search keeps one live `Option<GameState>` per ply and that search
//! depth is data-driven, so production depth is this fixture's number times a
//! multiplier no static bound covers. The risk that matters is the multiplier,
//! not the linear drift — and the drift is not even monotonic. See
//! <https://github.com/phase-rs/phase/issues/8376>.

#![cfg(all(target_pointer_width = "64", target_os = "linux"))]

use std::ptr;
use std::sync::atomic::{compiler_fence, Ordering};

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
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

/// Ceiling for the measured high-water. Fails the test on every supported
/// target, without being tuned to any of them.
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

/// Clearance kept below [`paint_below_this_frame`]'s own locals.
///
/// This does **not** have to cover [`measure`]'s frame, let alone the test's:
/// `hi` is derived inside a function one call deeper, so every frame above it
/// is out of range by the calling convention rather than by this constant. What
/// it has to cover is that one leaf-ish function's own locals, the `memset`
/// frame `write_bytes` pushes *below* it while painting, and the ABI red zone
/// (128 B on x86-64 SysV; zero on AArch64). Those total a few hundred bytes at
/// `opt-level = 0`, so 16 KiB is roughly two orders of magnitude of slack.
///
/// The cost is that depths shallower than this are indistinguishable, which the
/// saturation assertion turns into a loud failure rather than a small number.
const PAINT_CLEARANCE: usize = 16 << 10;

const SENTINEL: u8 = 0xAB;
const SENTINEL_WORD: u64 = u64::from_ne_bytes([SENTINEL; 8]);

// Fixture text copied verbatim from `game_state_stack_budget.rs` so the two
// measure the same path. A death trigger on every seat makes the measured
// resolution a real trigger cascade rather than a bare removal.
const MURDER_ORACLE: &str = "Destroy target creature.";
const DEATH_TRIGGER_ORACLE: &str =
    "Whenever this creature or another creature dies, you gain 1 life.";

/// What [`measure`] hands back.
struct Measurement {
    /// Deepest written byte, as a depth below the painting frame.
    high_water: usize,
    /// The validated `[low, high)` from [`stack_bounds`]. Reported so that a
    /// run on a newly enabled target shows what its query actually returned,
    /// rather than only that the run agreed with it.
    bounds: (usize, usize),
    /// The depth the scan saturates at, i.e. what `high_water` equals when
    /// nothing in the painted region was disturbed. Derived from the *aligned*
    /// `hi`, not from [`PAINT_CLEARANCE`], because rounding moves it by up to 7 bytes
    /// and a saturation check against the unrounded constant would miss.
    saturation_floor: usize,
}

/// The running thread's usable stack as `[low, high)`, from the platform.
///
/// `probe` must be the address of a local in the caller's frame. It is not used
/// to *derive* the bounds — that is the whole point — but to **validate** them:
/// a query that does not describe the stack this code is running on panics here
/// rather than returning a range the caller will write into.
///
/// Only ever called on a thread this file spawned. That matters on Linux, where
/// the main thread's stack is reported from `RLIMIT_STACK` and is not fully
/// mapped; a spawned thread's is a single mapping, so every byte in `[low,
/// high)` is writable.
fn stack_bounds(probe: *const u8) -> (usize, usize) {
    let (low, high) = unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        let rc = libc::pthread_getattr_np(libc::pthread_self(), &mut attr);
        assert_eq!(rc, 0, "pthread_getattr_np failed with {rc}");

        let mut base: *mut libc::c_void = ptr::null_mut();
        let mut size: libc::size_t = 0;
        let rc = libc::pthread_attr_getstack(&attr, &mut base, &mut size);
        assert_eq!(rc, 0, "pthread_attr_getstack failed with {rc}");

        let mut guard: libc::size_t = 0;
        let rc = libc::pthread_attr_getguardsize(&attr, &mut guard);
        assert_eq!(rc, 0, "pthread_attr_getguardsize failed with {rc}");

        libc::pthread_attr_destroy(&mut attr);

        // Skipping `guard` bytes above the reported base is correct when the
        // guard is carved out of the reported block, and merely costs one guard
        // page of range when the platform already excluded it. Both directions
        // are safe; only the other rounding would not be.
        (base as usize + guard, base as usize + size)
    };

    assert!(
        low < high,
        "platform reported an empty stack range [{low:#x}, {high:#x})"
    );
    let probe = probe as usize;
    assert!(
        probe >= low && probe < high,
        "the platform stack query does not contain this frame's own local \
         ({probe:#x} is outside [{low:#x}, {high:#x})), so the bounds are wrong \
         for this target — refusing to paint"
    );
    (low, high)
}

/// Paints from `low` up to a ceiling derived from **this** function's frame,
/// and returns that ceiling.
///
/// Deriving `hi` here rather than in [`measure`] is what makes the exclusion a
/// proof instead of a checklist. The stack grows down, so every frame above
/// this one — [`measure`]'s locals, its spill slots, its caller's, the whole
/// chain up to the thread entry — lies at a higher address than this frame's
/// locals, and therefore above `hi`. Nothing needs enumerating, which matters
/// because the things easiest to forget are the ones with no name in the source:
/// compiler temporaries and spill slots.
///
/// `caller_probe` is the address of a local in [`measure`]'s frame, used only to
/// check the direction of stack growth at runtime rather than trust it.
///
/// # Safety
///
/// `low` must be the lowest writable byte of the current thread's stack, as
/// returned and validated by [`stack_bounds`].
#[inline(never)]
unsafe fn paint_below_this_frame(low: usize, caller_probe: usize) -> usize {
    let floor = 0u8;
    let floor_addr = &floor as *const u8 as usize;

    assert!(
        floor_addr < caller_probe,
        "this callee's frame ({floor_addr:#x}) is not below its caller's          ({caller_probe:#x}), so the stack does not grow down here and the          whole exclusion argument fails — refusing to paint"
    );

    let hi = (floor_addr - PAINT_CLEARANCE) & !7;
    assert!(
        low < hi,
        "no room to paint between the validated stack floor ({low:#x}) and          this frame ({hi:#x})"
    );

    ptr::write_bytes(low as *mut u8, SENTINEL, hi - low);

    // Tripwire, not a proof — and the difference was measured, not assumed.
    // `floor` is a zero-initialised local of this frame, so reading `SENTINEL`
    // back means the write ran past `hi` into live storage.
    //
    // What it does **not** do is catch a large overrun. Planting `hi =
    // floor_addr + 64` as a deliberate negative control does not trip this
    // assertion: it produces `SIGSEGV`, because `floor_addr` is itself a local
    // in the painted frame, so by the time the check runs it is reading through
    // an address made of sentinel bytes. A checker whose own storage is inside
    // the region it checks cannot be relied on. The guarantee here is the frame
    // boundary plus [`PAINT_CLEARANCE`]; this only narrows the window where a
    // marginal overrun would otherwise pass quietly.
    assert_eq!(
        ptr::read_volatile(floor_addr as *const u8),
        0,
        "the paint reached this frame's own local at {floor_addr:#x}: \
         PAINT_CLEARANCE ({PAINT_CLEARANCE} B) is too small for this target's \
         frame layout"
    );

    hi
}

/// Lowest address in `[low, hi)` whose byte is no longer [`SENTINEL`], or `hi`
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

/// Paint, run the measured action, scan.
///
/// `#[inline(never)]` and taking `runner` by reference are both load-bearing,
/// not style: they keep the `GameRunner` in a shallower frame than the painted
/// region, so the largest live value in the test provably cannot be in range.
/// See the safety argument in the module docs.
#[inline(never)]
fn measure(runner: &mut GameRunner, murder: ObjectId, victim: ObjectId) -> Measurement {
    let probe = 0u8;
    let probe_addr = &probe as *const u8 as usize;
    let bounds = stack_bounds(&probe);
    let (stack_low, _stack_high) = bounds;

    // Word-align the floor: `deepest_disturbed` scans `u64`s.
    let low = stack_low.next_multiple_of(8);

    // The one live value whose placement is not settled by the calling
    // convention. Everything in this frame and above it is excluded by
    // construction — see `paint_below_this_frame` — but `runner` is a
    // reference, and its *pointee* lives wherever the caller put it. In
    // practice that is a shallower frame or the heap, both outside the region;
    // this asserts it rather than reasoning about it.
    let runner_lo = runner as *mut GameRunner as usize;
    let runner_hi = runner_lo + size_of::<GameRunner>();

    // SAFETY: `low` is the validated floor from `stack_bounds`, and the ceiling
    // is derived inside the callee from its own frame, so every frame above it
    // — this one included — is out of range by the calling convention.
    let hi = unsafe { paint_below_this_frame(low, probe_addr) };

    // Checked here rather than inside the painter: this frame is above the
    // painted region and above the painter's frame, so it is still intact for
    // any overrun that stopped short of it — which is exactly the case the
    // painter's own tripwire cannot report.
    assert_eq!(
        probe, 0,
        "the paint reached this frame's local at {probe_addr:#x}; the frame \
         boundary in paint_below_this_frame did not hold on this target"
    );

    assert!(
        runner_hi <= low || runner_lo >= hi,
        "the GameRunner ({runner_lo:#x}..{runner_hi:#x}) overlaps the paint \
         region ({low:#x}..{hi:#x}); it must live in a shallower frame or the \
         heap"
    );
    compiler_fence(Ordering::SeqCst);

    runner.cast(murder).target_objects(&[victim]).resolve();
    runner.advance_until_stack_empty();

    compiler_fence(Ordering::SeqCst);
    // SAFETY: same region just painted, still owned by this thread.
    let deepest = unsafe { deepest_disturbed(low, hi) };

    Measurement {
        high_water: probe_addr - deepest,
        bounds,
        saturation_floor: probe_addr - hi,
    }
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

    let runner = scenario.build();

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
            let mut runner = runner;
            let measurement = measure(&mut runner, murder, victim);
            (runner, measurement)
        })
        .expect("spawn measuring thread");

    let (runner, measurement) = handle.join().expect(
        "the measured action panicked. Unlike game_state_stack_budget.rs this \
         thread has 64 MiB — more than production's 32 MiB — so an overflow \
         here would mean genuinely unbounded recursion rather than a budget \
         overrun.",
    );
    let Measurement {
        high_water,
        bounds: (stack_low, stack_high),
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

    // The product, and the only line here worth comparing across commits —
    // within a target. Both harnesses capture stdout for a passing test, so
    // reading it needs `cargo test -- --nocapture` or
    // `cargo nextest run --success-output immediate`; see the module docs.
    println!(
        "STACK_HIGH_WATER_BYTES={high_water} ({} KiB)",
        high_water / 1024
    );
    println!(
        "STACK_BOUNDS_VALIDATED=[{stack_low:#x}, {stack_high:#x}) ({} MiB requested)",
        MEASURE_STACK_BYTES >> 20
    );
}
