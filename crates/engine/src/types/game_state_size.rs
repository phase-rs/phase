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
//! Ceilings are the measured size rounded up to the next 256 B. Reproduce the
//! measurement with an isolated target dir so a running Tilt keeps the
//! workspace cargo lock:
//!
//! ```text
//! CARGO_TARGET_DIR=/tmp/gs-size RUSTFLAGS="-Zprint-type-sizes" \
//!   cargo build -p engine --lib \
//!   | grep 'print-type-size type: `types::game_state::GameState`:'
//! ```
//!
//! Measured on `nightly-2026-04-19`, aarch64-apple-darwin:
//!
//! | Type | before boxing | after |
//! |---|---:|---:|
//! | `GameState` | 30,112 | 12,464 |
//! | `StackEntry` | 5,336 | 344 |
//! | `PendingCast` | 6,632 | 1,376 |
//! | `PendingTrigger` | 6,000 | 744 |
//!
//! When one of these fires, re-run the measurement above and change the number
//! deliberately — do not widen a ceiling to make a build pass.
//!
//! 64-bit only: `wasm32` has a different layout and its own (smaller) budget.

#[cfg(target_pointer_width = "64")]
const _: () = {
    use core::mem::size_of;
    assert!(size_of::<crate::types::game_state::GameState>() <= 12_544);
    assert!(size_of::<crate::types::game_state::StackEntry>() <= 512);
    assert!(size_of::<crate::types::game_state::PendingCast>() <= 1_536);
    assert!(size_of::<crate::game::triggers::PendingTrigger>() <= 768);
};
