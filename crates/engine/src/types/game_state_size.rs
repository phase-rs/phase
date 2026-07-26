//! Compile-time stack-budget ceilings for the types that dominate a
//! `GameState` stack frame.
//!
//! Not a rules invariant — a stack-budget one. `phase-server` moves `GameState`
//! by value through the action + AI path (`pre_action_state` in
//! `server-core/session.rs` and `boundary_snapshot` in `game/engine.rs` are
//! live simultaneously), so inline size is multiplied by every live slot. A
//! silent size regression there is an uncatchable guard-page abort in release,
//! not a catchable panic: `[profile.server-release]` is `panic = "unwind"`, and
//! unwinding cannot contain a stack overflow.
//!
//! `GameState`'s large maps are `im::HashMap` with structural sharing, so
//! `state.clone()` is already cheap on the *heap*. Nothing makes the inline
//! value cheap on a *stack frame* — that is what these ceilings bound.
//!
//! Ceilings are `measured.next_multiple_of(256) + 256` — one full 256 B bucket
//! of deliberate slack above the rounded measurement.
//!
//! The extra bucket is not laziness, it is calibration to the regression class.
//! What these asserts exist to catch is an inline `ResolvedAbility` (**5,264
//! B**) or a similar whole-struct field creeping back into a hot type; 256 B of
//! slack cannot hide anything in that class. What a zero-slack ceiling *does*
//! catch is an ordinary `Option<ObjectId>` on `PendingTrigger` — a struct that
//! is extended routinely — which would have tripped a build for an unrelated
//! author with no context for this file. Sized this way, the assert stays a
//! tripwire for layout regressions and stops being a tollbooth on normal work.
//!
//! Reproduce the measurement with an isolated target dir so a running Tilt keeps
//! the workspace cargo lock:
//!
//! ```text
//! CARGO_TARGET_DIR=/tmp/gs-size RUSTFLAGS="-Zprint-type-sizes" \
//!   cargo build -p engine --lib \
//!   | grep 'print-type-size type: `types::game_state::GameState`:'
//! ```
//!
//! Measured on `nightly-2026-04-19`, aarch64-apple-darwin:
//!
//! | Type | before boxing | after | ceiling |
//! |---|---:|---:|---:|
//! | `GameState` | 30,112 | 12,464 | 12,800 |
//! | `StackEntry` | 5,336 | 344 | 768 |
//! | `PendingCast` | 6,632 | 1,376 | 1,792 |
//! | `PendingTrigger` | 6,000 | 744 | 1,024 |
//!
//! When one of these fires, re-run the measurement above and change the number
//! deliberately — do not widen a ceiling to make a build pass.
//!
//! 64-bit only: `wasm32` has a different layout and its own (smaller) budget.

#[cfg(target_pointer_width = "64")]
const _: () = {
    use core::mem::size_of;
    assert!(
        size_of::<crate::types::game_state::GameState>() <= 12_800,
        "GameState grew past its stack budget. It is moved by value through the \
         phase-server action + AI path, so an overrun is an uncatchable \
         guard-page abort, not a panic. Re-run the -Zprint-type-sizes command in \
         this file's module docs, find the field that grew, and box it if it is a \
         large rarely-populated one. Change this number only deliberately, with \
         the new measurement recorded in the table above — do not widen it to \
         make a build pass."
    );
    assert!(
        size_of::<crate::types::game_state::StackEntry>() <= 768,
        "StackEntry grew past its stack budget. Every live GameState carries a \
         Vec of these and the resolver moves them by value. Re-run the \
         -Zprint-type-sizes command in this file's module docs and record the new \
         measurement in the table above; if a whole ResolvedAbility-sized struct \
         went inline, box it instead of widening this number."
    );
    assert!(
        size_of::<crate::types::game_state::PendingCast>() <= 1_792,
        "PendingCast grew past its stack budget. It is threaded by value through \
         the cast cost-payment chain, so it is multiplied by call depth. Re-run \
         the -Zprint-type-sizes command in this file's module docs and record the \
         new measurement in the table above; box a large field rather than \
         widening this number."
    );
    assert!(
        size_of::<crate::game::triggers::PendingTrigger>() <= 1_024,
        "PendingTrigger grew past its stack budget. Re-run the \
         -Zprint-type-sizes command in this file's module docs and record the new \
         measurement in the table above. If you added an ordinary small field and \
         this still fired, the type has genuinely outgrown the bucket — raise it \
         one 256 B step with the new measurement, do not delete the assert."
    );
};
