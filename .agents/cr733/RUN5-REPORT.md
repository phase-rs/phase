# CR733 resolved-command journal — Run 5 P2 tranche 1 report

## Starting state

Confirmed before edits: `/Users/matt/dev/forge.rs-cr733`, branch
`cr733/resolved-commands`, `HEAD` `d571b0c25cacd890a5ef22f223fb4c99551c5cfc`, and no
tracked modifications. The prior run's exact-pip/producer/payment provenance was read and
retained.

## Landed commit

- `5e24ac6900` — `cr733(p2): apply resolved mana commands`

## P2 recipe coverage

### Mana insert

1. `GameState::add_mana_to_pool` remains the real-pool insertion authority. It stamps the
   ordinary pip, constructs `ResolvedManaInsertCommand`, calls the exact applier, and records
   the command/provenance.
2. `ResolvedManaInsertCommand` contains the exact `ManaUnit`, recipient `PlayerId`, and
   producer node. The enclosing `ResolvedCommandJournalEntry.ordinal` carries command order.
3. `GameState::apply_resolved_mana_insert(&ResolvedManaInsertCommand) -> Result<(),
   ResolvedManaReplayInvariantError>` accepts no allocator, replacement pipeline, RNG, or
   dynamic query context. It rejects unknown players, unstamped/duplicate pips, re-inserts the
   recorded unit, and advances only the recorded pip high-water.
4. The journal records `ResolvedRulesCommand::ManaInsert` under its producer node using a new
   contiguous command ordinal. P1's initial node slot remains an intentionally empty payload.
5. Existing state-facing producers still call `add_mana_to_pool`; no producer was rerouted
   around the final authority.
6. The integration test snapshots a real Dimir Signet activation's pre-state, replays every
   recorded semantic mana command in journal order, and compares pools, surviving exact pip IDs,
   pip high-water, and the canonical journal.
7. That same real activation composes the auto-tapped basic-land insert, its exact payment, and
   the Signet's inserts in ordinal order.

### Mana spend/remove

1. `game::mana_payment::remove_exact_mana_units` is the final exact-unit primitive. It first
   validates a scratch pool, then removes only the recorded units from the live pool with the
   solver's existing `swap_remove` ordering; a malformed command cannot partially debit a pool.
2. `ResolvedManaSpendCommand` contains payer, recipient, payment node, and consumption-ordered
   `ResolvedManaSpentUnit` values, each preserving exact unit and producer identity.
3. `select_mana_payment` now owns selection on an immutable pool. Every live P1 payment funnel
   (cast, non-cast, nested auto-tap, mana-ability hybrid sub-cost, and Assist finalization) calls
   `GameState::resolve_and_apply_mana_spend`, which journals and delegates to
   `GameState::apply_resolved_mana_spend(&ResolvedManaSpendCommand) -> Result<(),
   ResolvedManaReplayInvariantError>`. The applier has no solver or dynamic query input.
4. A payment node gets its preserved P1 placeholder entry plus a semantic
   `ResolvedRulesCommand::ManaSpend` entry under its own fresh ordinal.
5. Selection continues to use the existing scratch solver and its original fallback/order rules;
   final removal repeats its exact selected `swap_remove` sequence rather than assigning a
   scratch pool into player state.
6. The real-activation replay test covers the exact selected payment from its pre-state.
7. The hostile replay test applies one recorded spend twice and asserts the second removal fails
   with `ResolvedManaReplayInvariantError::MissingExactManaUnit`, not a substitute selection.

## Journal and wire checks

`ResolvedCommandJournalEntry` now carries an optional semantic command for P2 while accepting
old P1 slots via serde default. Custom validation retains ordinal/node/provenance checks and now
also rejects malformed command payloads: wrong producer/payment node, wrong payer/recipient,
empty spends, unstamped/duplicate command pips, mismatched exact units, and duplicate semantic
insert/spend payloads. Inline tests cover valid round-trip plus malformed and duplicated payload
rejection. The viewer projection continues to replace the entire journal with its default, and
the existing turn-boundary truncation remains unchanged.

## Rules evidence

Verified against `/Users/matt/dev/forge.rs/docs/MagicCompRules.txt` before editing:

- CR 106.4 — mana enters a player's pool.
- CR 118.3a — paying mana removes the indicated mana from that pool.
- CR 605.3b and CR 605.4a — activated and triggered mana abilities resolve immediately where
  the P1 node boundaries are touched.

## Verification and risk

- `cargo fmt --all` and `git diff --check` passed before the implementation commit.
- The commit's parser-combinator and architecture pre-commit gates passed.
- Per charter, no cargo build, clippy, or test suite was run. Tilt reported green only for the
  main checkout, so it is not evidence for this worktree.
- Parity confidence is moderate: the real mutation paths preserve the existing solver choice and
  `swap_remove` ordering; state-layer dirtiness remains at its former call sites after a
  successful applier call. The unrun full suite is the remaining verification risk.

## Next P2 boundary

The next tranche should take the next matrix family only; do not start retained-subset
reconstruction. The most important fact to preserve is that P1 node placeholders and new P2
semantic entries share one contiguous ordinal stream: append a typed command entry under its
owning node, and use P2's exact payment command operands rather than ever re-solving mana.
