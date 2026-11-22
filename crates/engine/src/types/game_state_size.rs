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
//!   cargo build -p phase-engine --lib \
//!   | grep 'print-type-size type: `types::game_state::GameState`:'
//! ```
//!
//! Measured on `nightly-2026-04-19` — `GameState` on x86_64-unknown-linux-gnu,
//! the other three rows on aarch64-apple-darwin (not re-measured):
//!
//! | Type | before boxing | after | ceiling |
//! |---|---:|---:|---:|
//! | `GameState` | 30,112 | 14,208 | 14,336 |
//! | `StackEntry` | 5,336 | 344 | 768 |
//! | `PendingCast` | 6,632 | 1,376 | 1,792 |
//! | `PendingTrigger` | 6,000 | 744 | 1,024 |
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc when #9248 (the activation
//! cost carrier) merged with the activation cost-reduction election: 13,840 B,
//! over the ceiling. Boxing `pending_mass_library_order_choice` (328 B inline,
//! populated only between the batches of one resolution) brought it to 13,520,
//! whose ceiling by the rule above is still 13,824.
//!
//! `GameState` re-measured on aarch64-apple-darwin (`nightly-2026-04-19`) at
//! 13,840 B once the turn-scoped block-history ledger
//! (`creature_blocked_attackers_this_turn`) and the added-phase bookkeeping
//! (`steps_started_this_turn`, `next_extra_phase_id`, `last_added_phase_ids`)
//! landed together; none of those fields is a large rarely-populated one, so
//! the ceiling moved to 13,840.next_multiple_of(256) + 256 = 14,336.
//! `GameState` now measures 14,208 B on x86_64-unknown-linux-gnu, under the
//! 14,336 ceiling.
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc at 14,352 B when the
//! CR 614.12a pre-entry as-enters choice landed: two small boxed fields ? the
//! entry event's `enter_with_choices` (`Option<Box<EntryChoices>>`, which widens
//! every inline `ProposedEvent` by one pointer) and
//! `PendingReplacement::delivery_boundary` (`Option<Box<ProposedEvent>>`). Both
//! are already boxed, so nothing large went inline; the ceiling moved to
//! 14,352.next_multiple_of(256) + 256 = 14,848.
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc (`nightly-2026-04-19`) at
//! 15,072 B when the Horde stack merged: ~110 B of small Horde bookkeeping
//! (three `u32`s, three `Vec`s, a `usize`, a `u8`, an `im::Vector`, and an
//! already-boxed `pending_hero_event`) plus upstream's accumulated small fields
//! since the Horde base. No single large rarely-populated field went inline, so
//! the ceiling moves by the rule to
//! 15,072.next_multiple_of(256) + 256 = 15,360.
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc at 14,864 B when the
//! scheme-support branch merged main's tracked-set member origins
//! (`tracked_set_member_origins`, an `im::HashMap`, 24 B inline) alongside
//! the branch's own named-label scratch (`named_label_this_resolution`,
//! `Option<String>`, 24 B inline): each side was green alone under 14,848,
//! and the union crossed it by 16 B. Neither field is a large
//! rarely-populated one — both are ordinary 24 B fields, and the origins
//! map is already heap-shared — so the ceiling moves to
//! 14,864.next_multiple_of(256) + 256 = 15,360.
//!
//! Base `GameState` on x86_64-pc-windows-msvc measured 14,336 B at
//! `nightly-2026-04-19`, exactly at the ceiling of that time. The attacked-player referent
//! work adds `attack_target: Option<AttackTarget>` to `ZoneChangeCombatStatus`
//! (7 -> 24 B, align 1 -> 8), so `TriggerSourceContext` grows by 16 B and so does
//! `WaitingFor::OpponentGuess`, its largest holder, which set the inline size of
//! `WaitingFor` at 1,760 B. `WaitingFor::OpponentGuess.owner` (an
//! `OpponentGuessOwner`, 1,656 B, populated only while an opponent guess is
//! pending) is boxed. On x86_64-pc-windows-msvc `GameState` then measures
//! 14,208 B, `WaitingFor` 1,632 B and `TriggerSourceContext` 1,496 B, with
//! `WaitingFor::NamedChoice` (1,631 B) the largest `WaitingFor` variant. The
//! boxing keeps `GameState` under the ceiling above. Boxing OpponentGuess.owner leaves NamedChoice and
//! ChooseOneOfBranch more than 200 B apart, so `WaitingFor` carries an intentional
//! `#[allow(clippy::large_enum_variant)]`; the GameState ceiling above is the bound.
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc at 14,864 B when the
//! CR 603.10d control-change evidence tables (`control_change_captures`,
//! `control_change_observations`, and their two `u64` id counters) landed.
//! Both tables are empty outside a resolution, so they are lazily boxed
//! (`LazyBoxedMap`: unallocated while empty, so an empty clone allocates
//! nothing); `GameState` then measures 14,832 B, under the unchanged 14,848
//! ceiling.
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc (`nightly-2026-04-19`) at
//! 14,928 B at the PR-9867/upstream merge (80 B over the old ceiling). The
//! union adds only small fields — two 48 B `HashMap`s (`combat_prevention_tally`,
//! `attacked_players_directly_this_turn`), a 24 B batch `Vec`, id/owner stamps,
//! and upstream's onset/ingress/policy markers — while the large paused-combat
//! record rides boxed (`Option<Box<PendingCombatLifelink>>`, 8 B inline).
//! `WaitingFor` grew 1,632 -> 1,672 B and `TriggerSourceContext` 1,496 ->
//! 1,536 B. Nothing large went inline, so the ceiling moves to
//! 14,928.next_multiple_of(256) + 256 = 15,360.
//!
//! `GameState` re-measured on x86_64-unknown-linux-gnu (`nightly-2026-04-19`) at
//! 14,896 B with the tracked-set member origins (`tracked_set_member_origins`,
//! 24 B), the loop-detection held window (8 B) and decision tally
//! (`loop_decision_tally`, 40 B) and the loop shortcut's rising ∞ axes
//! (`unbounded_rising`, 24 B); none of those fields is a large rarely-populated
//! one, so the ceiling is 14,896.next_multiple_of(256) + 256 = 15,360.
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc (`nightly-2026-04-09`) at
//! 15,376 B when the Horde stack merged main through `bf584d6b`: the branch's
//! Horde bookkeeping (~110 B of small fields, already counted at 15,072 here)
//! unioned with main's new small fields since the merge base (`un_actions`,
//! the loop-detection held window, `players_who_flipped_coins_this_turn`, minus the
//! removed `unbounded_loop_enablers`). Each side holds under 15,360 alone —
//! the branch at 15,072 on this builder, main green in CI where this assert
//! runs on every 64-bit build — and the union crosses by 16 B. No single
//! large rarely-populated field went inline, so the ceiling moves by the rule
//! to 15,376.next_multiple_of(256) + 256 = 15,872.
//!
//! `GameState` re-measured on x86_64-unknown-linux-gnu (`nightly-2026-04-18`) at
//! 15,920 B when the two-phase damage batch merged main through `51b7fe41`: the
//! batch's execution-local ledger (`damage_batch_ledger`, 112 B inline, populated
//! only while a damage batch settles) unioned with main's small fields. Boxing the
//! ledger brought `GameState` to 15,808 B, under the unchanged 15,872 ceiling.
//!
//! `GameState` re-measured on x86_64-pc-windows-msvc (`nightly-2026-04-19`) at
//! 15,904 B when the layer pass's control-change working memory
//! (`layer_control_changes`, `announced_control_changes`, two 24 B `Vec`s)
//! met main's small fields since its base. Both lists are empty at every
//! action boundary, so they are lazily boxed (`LazyBoxedVec`, 8 B each);
//! `GameState` then measures 15,872 B, at the unchanged ceiling.
//!
//! `GameState` re-measured on aarch64-apple-darwin (`nightly-2026-04-19`) at
//! 15,904 B when the canonical library-placement branch merged main: its
//! `ProposedEvent::ZoneChange::library_placement` held a 96 B
//! `Option<LibraryPosition>` inline, making `ProposedEvent` 288 B (200 B with
//! the field boxed) and widening both `GameState` fields that hold one inline
//! (`pending_replacement`, `pending_liminal_entry_resume`). Only library moves
//! carry a placement, so the field is boxed (`Option<Box<LibraryPosition>>`);
//! `GameState` then measures 15,728 B, under the unchanged 15,872 ceiling.
//!
//! `GameState` re-measured on aarch64-apple-darwin (`nightly-2026-04-19`) at
//! 15,888 B when the counter-removal instruction-scope work (#10699) merged
//! main through `ed6fd9ae`. Main alone measures 15,824 B; the branch adds two
//! execution-local fields, `instruction_scopes` (`Vec<OpenInstruction>`, 24 B)
//! and `last_closed_instruction` (`Option<ClosedInstruction>`, 32 B), and the
//! union crosses 15,872 by 16 B. Neither field is a large rarely-populated
//! one: `zones::within_instruction` writes both on every instruction scope,
//! so boxing would put an allocation on that path. The ceiling moves by the
//! rule to 15,888.next_multiple_of(256) + 256 = 16,384.
//!
//! `GameState` re-measured on aarch64-apple-darwin (`nightly-2026-04-19`) at
//! 15,776 B when #10699 merged main through `e453af3d`, after the canonical
//! library-placement entry above boxed `library_placement`. That is under the
//! 16,384 ceiling, so the ceiling is unchanged.
//!
//! `GameState` measured on aarch64-apple-darwin (`nightly-2026-04-19`) at
//! 15,952 B when #10637's staged destroy and ChangeZone populations merged
//! main through `51b7fe41d9`: the branch's `staged_population_parent`
//! (`Option<StagedPopulationParent>`, 24 B) unioned with main's new fields
//! since the merge base, among them `pending_player_scope_choice`
//! (`Option<PendingPlayerScopeChoice>`, 56 B), `unsettled_entry_batch_events`,
//! `instruction_boundary_marks` and the two departed-activation ledgers (small
//! `Vec`s and heap-shared `im::HashMap`s). No single large
//! rarely-populated field went inline, so the ceiling moves by the rule to
//! 15,952.next_multiple_of(256) + 256 = 16,384.
//!
//! `GameState` re-measured on x86_64-unknown-linux-gnu (`nightly-2026-04-18`) at
//! 15,856 B when the two-phase damage batch merged main through `91639fdf`: the
//! branch's boxed `damage_batch_ledger` (8 B) and `damage_event_losses` unioned
//! with main's fields since the branch's last merge, and main's combat one-shot
//! application slot (`combat_one_shot_applications`, now the ledger's
//! `applications`) dropped. That is under the unchanged 16,384 ceiling.
//!
//! `GameState` re-measured on x86_64-unknown-linux-gnu while semantically porting
//! the as-enters dice/counters branch (72ae22642) onto main through da2af2bca5,
//! including canonical placement and component delivery (#10636), at 15,840 B.
//! Measured with the isolated `-Zprint-type-sizes` command above. The merged
//! parked-entry and pre-entry bookkeeping remains under the calibrated ceiling:
//! 15,840.next_multiple_of(256) + 256 = 16,384, so the ceiling is unchanged.
//!
//! `GameState` re-measured on x86_64-unknown-linux-gnu (`nightly-2026-04-19`) at
//! 15,936 B when the two-phase damage batch merged main through `206116c5`:
//! the branch's boxed `damage_batch_ledger` and `damage_event_losses` unioned
//! with the as-enters dice/counters bookkeeping (#10433) above. That is under
//! the unchanged 16,384 ceiling.
//!
//! `GameState` re-measured on x86_64-unknown-linux-gnu (`nightly-2026-04-19`) at
//! 15,952 B when the two-phase damage batch merged main through `f99eb633`:
//! main's boxed `pending_damage_post_counter` (#10016) unioned with the branch's
//! fields. That is under the unchanged 16,384 ceiling.
//!
//! #10637 contributes a 24 B staged parent witness to `GameState`; the batch
//! parking witness is synchronous and does not enlarge the saved state.
//! The staged parent's inline size fits within the existing bucket's slack;
//! keep the 16,384 ceiling.
//!
//! When one of these fires, re-run the measurement above and change the number
//! deliberately — do not widen a ceiling to make a build pass.
//!
//! 64-bit only: `wasm32` has a different layout and its own (smaller) budget.

#[cfg(target_pointer_width = "64")]
const _: () = {
    use core::mem::size_of;
    assert!(
        size_of::<crate::types::game_state::GameState>() <= 16_384,
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
