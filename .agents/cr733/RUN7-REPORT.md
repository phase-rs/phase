# CR733 resolved-command journal — Run 7 P2 tranche 3 report

## Starting state

Confirmed before edits: `/Users/matt/dev/forge.rs-cr733`, branch
`cr733/resolved-commands`, `HEAD`
`a1984d3c5b2e830819812d2dc90f4810c79fd741`, and no tracked modifications.
The required prerequisite plan, Run 5/6 reports, P2 mana/scalar-status commits,
and CR733 authority matrix were read before implementation.

## Landed implementation

- `b905f7e85e` — `cr733(p2): apply resolved counter and ledger commands`

## P2 recipe coverage

### Counters

1. Player counters remain owned by the pre-existing
   `ResolvedPlayerEdit::Counter` authority. This tranche adds only object-counter
   delivery; it does not duplicate energy, experience, ticket, or other player
   counter handling.
2. `ResolvedObjectCounterCommand` carries the exact `ObjectIncarnationRef`,
   `CounterType`, captured predecessor count, final add/remove count, counter-add
   actor, and causal node. Its precondition makes an exact delivery
   non-idempotent.
3. `game::effects::counters::apply_counter_addition` and
   `apply_counter_removal` construct commands only at their final delivery seam.
   Addition is reached after `add_counter_with_replacement` has completed the
   CR 614 replacement path, so the command records the final Vorinclex/Hardened
   Scales-class count and never re-enters replacement processing on replay.
4. The shared applier is:

   ```rust
   game::effects::counters::apply_resolved_counter_edit(
       state: &mut GameState,
       command: &ResolvedObjectCounterCommand,
   ) -> Result<(), ResolvedObjectCounterReplayInvariantError>
   ```

   It validates object existence, exact incarnation, expected predecessor count,
   nonzero count, and overflow before changing the map. A stale occurrence or
   repeated command fails typed; it never partially mutates state. Counter-added
   history and layer dirtiness remain inside this final authority. Existing event
   emission stays at the ordinary caller after a successful command application.
5. Ordinary zero additions/removals remain no-ops: no command, map edit, or
   event is created. An inline regression test covers that path.
6. The integration test casts source-verified **Stony Strength** through
   `GameRunner`, replays its recorded object-counter command from the pre-state,
   and compares counters plus counter history. A hostile test proves both a
   double application and a same-ID new incarnation are typed failures.

### Ledgers and once-per-* facts

1. New `game::ledger` is the sole final authority for this tranche's per-event
   ledger edits. It records semantic key/append edits rather than replacing any
   whole map, set, or history container.
2. `ResolvedLedgerEdit` covers the requested event facts: finalized spell casts,
   activated abilities, constrained triggers (once per turn/game/per opponent
   and max-times-per-turn), and consumed once-per-turn permission keys.
3. Ordinary spell-cast, activated-ability, and trigger recording now route
   through the ledger authority. Cast/play finalization routes all currently
   modeled graveyard, hand, alternative-cost, exile, and top-library
   once-per-turn permission consumptions through it as well.
4. The appliers are:

   ```rust
   game::ledger::resolve_and_apply_ledger_edit(
       state: &mut GameState,
       edit: ResolvedLedgerEdit,
   ) -> Result<(), ResolvedLedgerEditReplayInvariantError>

   game::ledger::apply_resolved_ledger_edit(
       state: &mut GameState,
       command: &ResolvedLedgerEditCommand,
   ) -> Result<(), ResolvedLedgerEditReplayInvariantError>
   ```

   Spell records validate the captured aggregate/map/history prefix before
   appending both histories; activation and max-trigger counts validate their
   exact predecessor values; set inserts reject duplicates as typed failures.
5. The integration test's real Stony Strength cast also supplies an actual
   spell-cast ledger entry. Replaying it from the cast pre-state reproduces its
   aggregate and per-player histories; a second application returns
   `SpellCastPreconditionMismatch`.

## Adjudications and wire safety

- Turn-boundary bulk clears are deliberately **not** journaled here. They remain
  the future Turn-transition family's semantic aggregate; this tranche records
  only per-event inserts/consumptions.
- Other matrix ledger rows remain out of this narrow tranche, including
  commander-cast facts, `spells_cast_last_turn`, ability-resolution/land/damage
  and other per-turn ledgers, and unbounded/loop tracking. They require their
  own exact semantic command scopes rather than being folded into this one.
- Journal deserialization fail-closes both new families on an unrelated node,
  zero/no-op counter edit, impossible counter predecessor, overflowing ledger
  prefix, or legacy object identity. `ObjectIncarnationRef`'s inherited
  bare-ID compatibility shape produces `LEGACY_INCARNATION`; executable counter
  and trigger-ledger payloads now reject it before either applier is reachable.
  The unit test mutates each serialized legacy shape and confirms rejection.
- No new serde-default identity field or applier sentinel was introduced.

## Rules evidence

Verified before annotations against
`/Users/matt/dev/forge.rs/docs/MagicCompRules.txt`:

- CR 122.1, 122.1a-g, 122.2, and 122.6/a — counters and counters put on an
  object.
- CR 614.1, 614.1a-d, and 614.12 — replacement effects and entry replacement
  handling.
- CR 601.2i — a spell becomes cast after casting is complete.
- CR 602.5b — restricted activated abilities remain restricted on that object.
- CR 603.2c — trigger occurrences.

The Stony Strength test Oracle text was independently read from the Scryfall
card API: “Put a +1/+1 counter on target creature you control. Untap that
creature.”

## Verification and parity risk

- `cargo fmt --all` and `git diff --check` passed.
- The implementation commit's parser-combinator and router/grant pre-commit
  gates passed.
- Per the binding charter, no Cargo build, clippy run, or test suite was run;
  Tilt from another checkout was not used as evidence.
- Confidence is **moderate**. Source inspection confirms that object additions
  journal after replacement delivery and that production per-event ledger
  writers now enter the shared applier. The unrun integration suite is the
  remaining material risk, especially broad replacement and cast-finalization
  funnels.

## Remaining P2 families

Zone changes; draw/mill/discard/exile/sacrifice/return; library
order/reveal/shuffle; token/object creation; deletion/cleanup; modifier
registries; stack; turn/combat/outcome; trigger/LKI collection; continuation
state; remaining ledger rows; information; and player leave remain outside this
tranche. Do not begin P3 reconstruction.

## Most important next-run fact

Every semantic entry—including these counter and ledger commands—must append
under its owning P1 node in the one shared `ResolvedCommandOrdinal` stream. A
future family must replay its final resolved operands through its own authority,
never by re-entering replacement processing, effect dispatch, or a bulk
container write.
