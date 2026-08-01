# CR733 resolved-command journal — Run 6 P2 tranche 2 report

## Starting state

Confirmed before edits: `/Users/matt/dev/forge.rs-cr733`, branch
`cr733/resolved-commands`, `HEAD`
`039983da1ec679a49fb6362c8ba3fb0a4f7d47b3`, and no tracked modifications.
Only untracked `.agents/` material was permitted by the charter; none affected the starting
worktree check.

## Landed implementation

- `770b993c2e` — `cr733(p2): apply resolved scalar and status commands`

## P2 recipe coverage

### Scalar resources

1. `GameState::apply_resolved_player_edit` is the final semantic mutation primitive for
   `Life { delta }`, `Energy { delta }`, `Counter { kind, delta }`, and
   `Speed { old, new }`. It changes only the addressed resource axis; it never restores a
   captured `Player` snapshot.
2. `ResolvedPlayerEditCommand` carries the exact `PlayerId`, typed semantic edit, and
   `RulesExecutionNodeRef` cause. Resource edits reject zero deltas, underflow, and overflow;
   speed validates its old value before applying its exact transition.
3. Ordinary life gain/loss/damage routes through the existing replacement pipeline, then the
   final post-replacement delta reaches the applier. Energy gain/payment, player-counter gain
   and removal (including rad/ticket), speed, and the debug resource actions also construct and
   apply typed commands. Life bookkeeping remains part of the semantic delta rather than a
   caller-side write.
4. Each ordinary edit records `ResolvedRulesCommand::PlayerEdit` under the active execution node
   (or a proposal node when no active node exists), using the shared P1/P2 ordinal stream.
5. The factored paths preserve their existing replacement, event, and preflight structure;
   only their final direct field writes were replaced with the applier call. This was reviewed
   by source comparison, not by an executed test suite.
6. The integration coverage drives a real Lightning Bolt cast through `GameRunner`, captures the
   final `-3` life command, replays it from the pre-state, and compares life plus the per-turn
   life-loss bookkeeping. It also exercises every scalar command shape for compositional writes.
7. The hostile scalar test applies an exact energy removal twice and requires
   `ResolvedPlayerEditReplayInvariantError::ResourceUnderflow` on the second application.

### Tap/untap/exert/status

1. `game::object_state::apply_resolved_object_edit` is the final transition primitive for the
   typed `Tapped` and `Exerted` status axes.
2. `ResolvedObjectStatusCommand` carries `ObjectIncarnationRef`, status axis, required old
   boolean, new boolean, and cause. Its applier verifies both the exact incarnation and the
   prior status before mutating.
3. Tap/untap effects, replacement-choice completion, tap/untap costs, auto-tapping, the untap
   step/Seedborn, manual land helpers, attacking, exerting, and debug tap actions now construct
   and apply the status command through the shared authority.
4. The authority records `ResolvedRulesCommand::ObjectStatus` under the active node after a
   successful non-no-op transition. It keeps no-op requests out of the semantic stream.
5. Existing event emission and replacement choice flow remain at their old callers; the applier
   owns only the final exact status write.
6. The real Dimir Signet activation replay applies the full journal from the pre-state and now
   compares both exact mana state and the Signet source's tapped status.
7. The hostile status test selects that source's recorded command, proves the second apply fails
   with `StatusPreconditionMismatch`, then bumps the same object ID's incarnation and proves a
   `StaleObject` failure.

## Applier contracts and wire checks

```rust
GameState::apply_resolved_player_edit(
    &mut self,
    command: &ResolvedPlayerEditCommand,
) -> Result<(), ResolvedPlayerEditReplayInvariantError>

game::object_state::apply_resolved_object_edit(
    state: &mut GameState,
    command: &ResolvedObjectStatusCommand,
) -> Result<(), ResolvedObjectStatusReplayInvariantError>
```

`ResolvedRulesJournal` validates both new payload families fail-closed on deserialize: a player
edit must have a non-empty semantic operation and matching cause/node; an object-status command
must have a true transition and matching cause/node. Unit coverage round-trips valid payloads and
rejects zero scalar edits, no-op status commands, and an unrelated cause. The new status command
uses `ObjectIncarnationRef` directly; it introduces no new sentinel/default identity field, so no
new legacy-shape sentinel migration was needed.

## Rules evidence

Verified against `/Users/matt/dev/forge.rs/docs/MagicCompRules.txt` before annotations:

- CR 107.14 and CR 122.1 — energy/player counters.
- CR 119.2, CR 119.3, CR 119.4, and CR 119.5 — damage, gain/loss, paying life, and setting life.
- CR 701.26a-b — tap and untap.
- CR 701.43d — exert as an attack cost.
- CR 702.179b-f — speed.

## Verification and parity risk

- `cargo fmt --all` and `git diff --check` passed before commit.
- The commit's parser-combinator and router/grant architecture pre-commit gates passed.
- Per the binding charter, no Cargo build, clippy, or test suite was run. Tilt status from another
  checkout was not treated as evidence.
- Confidence is moderate. The inspected ordinary paths retain their existing replacement and
  event boundaries, and commands are semantic deltas rather than snapshots. The unrun integration
  suite remains the material risk, particularly in broad cost/replacement/turn status funnels.

## Remaining P2 families

The next matrix boundary is **Counters**. Later P2 families remain zone changes; draw/mill/
discard/exile/sacrifice/return; library order/reveal/shuffle; token/object creation; deletion/
cleanup; modifier registries; stack; turn/combat/outcome; triggers/LKI; continuations; ledgers;
information; and player leave. Do not begin P3 reconstruction.

## Most important next-run fact

P1 node placeholders and every P2 semantic entry share one contiguous
`ResolvedCommandOrdinal` stream. New family work must append its typed command under the owning
node and call the final authority applier; it must never replay by redispatching an effect,
replacement pipeline, solver, RNG, or allocator.
