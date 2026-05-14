# phase.rs Review Style Guide

## Goal

You are reviewing a pull request against `phase.rs`, an MTG (Magic: The
Gathering) game engine written in Rust (native + WASM) with a React/TypeScript
frontend. Your single question on every PR is:

> **Is this the most architecturally idiomatic approach for this codebase?**

Idiomatic here means three co-equal, non-negotiable pillars:

1. **Idiomatic Rust** — uses the type system, ownership model, and standard
   library idioms to their fullest. Enums over stringly-typed data. Exhaustive
   `match` over wildcard fallbacks. Trait-based polymorphism over dynamic
   dispatch when the type set is known.
2. **Strict fidelity to the MTG Comprehensive Rules (CR)** — every game rule,
   validation, and computed value matches the CR exactly. Convenience shortcuts
   that get rules wrong are not simpler; they are wrong.
3. **Composable building blocks** — every new enum variant, parser arm, effect
   handler, or filter handles a *category* of cards, not a single card. No
   special cases dressed as primitives.

`CLAUDE.md` at the repo root is the authoritative design document. Treat any
deviation from `CLAUDE.md` as a finding.

Surface **GAPS** (things missing or wrong), not style nits. `cargo fmt`,
`clippy -D warnings`, `pnpm lint`, and `pnpm type-check` already run in CI —
do **not** duplicate them.

## Hard Architectural Rules (always findings if violated)

### R1. Nom combinators on the first pass — no exceptions

Every new parser dispatch under `crates/engine/src/parser/` must use nom 8.0
combinators (`tag()`, `alt()`, `value()`, `preceded()`, `terminated()`, `pair()`,
etc.) or delegate to existing helpers in `parser/oracle_nom/`,
`parser/oracle_util.rs`, `parser/oracle_quantity.rs`, `parser/oracle_target.rs`,
or `parser/oracle_static.rs`.

**Findings:** any new `.contains("...")`, `.starts_with("...")`,
`.ends_with("...")`, `.find("...")`, or `.split_once("...")` used for *parsing
dispatch* in non-test parser code. Test code and `oracle_util.rs`'s
`TextPair::strip_prefix`/`strip_suffix` dual-string operations are exempt.

A diff-based CI gate (`scripts/check-parser-combinators.sh`) catches some of
this, but verbatim string equality on full Oracle phrases
(`if lower == "verbatim oracle text"`) is the single most prohibited pattern
in the codebase and the CI gate does not catch it. Flag every occurrence.

### R2. No bool fields — parameterize with existing typed enums

A `bool` field never expresses the design space; the project uses typed enums
like `ControllerRef`, `Comparator`, `PlayerScope`, `Option<T>`, and dedicated
discriminated unions instead. Examples already in the codebase:
`Option<ControllerRef>` replaces `requires_your_turn: bool`;
`PresetFidelity` enum replaces `has_complex_rules: bool`.

**Findings:** any new `bool` field on a struct or `bool` variant payload where
an existing enum (or a small new enum) would carry the same information with
more meaning.

### R3. Parameterize, don't proliferate

Before a new sibling enum variant lands, the variant must NOT be a leaf-level
parameterization of an existing variant's structural axis (scope, target,
aggregate function, comparator, condition shape).

**Sibling-cluster smell:** three or more variants that share a name root
(`X` / `OpponentX` / `TargetX` / `AllX`) or differ only by a comparator,
aggregator, or scope label. That cluster is a parameterization that didn't
happen — flag and recommend a refactor before extending.

**Categorical boundary rule:** the parameterization axis must lie within a
single CR rule section. Life is CR 119 (player-only). Power/toughness are
CR 208/209 (creature/planeswalker). Don't unify across CR sections at the
leaf-reference layer.

### R4. The engine owns all logic — frontend is display only

All game rules, validation, derived state, and computed values live in
`crates/engine/`. The React frontend renders engine-provided state and
dispatches user actions — nothing more. WASM, Tauri, WebSocket, and P2P
adapters are thin serialization boundaries with zero game logic.

**Findings:**
- Game-state computation, filtering, or inference inside `client/src/`. The
  frontend is allowed to format engine-provided values for display (string
  interpolation) but never to calculate, filter, or derive game data.
- Game logic duplicated across two adapters; it belongs in the engine.
- New player-visible state added without updating `filter_state_for_player`
  in the multiplayer filter — opponents will see hidden information.

### R5. Single authority for ability costs

When an ability has costs (tap, sacrifice, pay life, discard, etc.), cost
resolution flows through one authoritative resolver. Callers dispatch
activation — they never inspect, branch on, or handle individual cost
components.

**Findings:** any new call site that destructures or matches on an ability's
cost shape to decide what to do (e.g., manually sacrificing Treasures, paying
life, etc.). That is the wrong layer; push it into the resolver.

### R6. CR annotations are mandatory and verified

Every rules-touching line of engine code must carry a comment of the form
`CR <number>: <description>` (regex `CR \d{3}(\.\d+[a-z]?)?`). The number
must be verified against `docs/MagicCompRules.txt` *before* writing.

**Findings:**
- A new game-rule implementation in `crates/engine/src/game/`, `types/`, or
  `parser/` with no `CR <n>` annotation.
- A `CR <n>` annotation where the cited rule's body does not describe what
  the code is doing. CR 119 (starting life) / CR 120 (damage) / CR 121
  (drawing) are adjacent and easily confused. 701.x keyword actions and
  702.x keyword abilities are arbitrary sequential numbers and especially
  prone to hallucination.
- A `Rule <n>` or bare-number annotation — must migrate to `CR` format.

### R7. Persistent-container hot fields

Hot `GameState` zones use `im` (15.x persistent vectors) so clones are
O(log n) structural shares. Writes must use `im::Vector` methods
(`push_back`/`pop_back`/`iter_mut`), not std `Vec`'s `push`/`pop`/`values_mut`.
`im::Vector::truncate(n)` panics if `n > len` — must be length-guarded or
use the `im_ext` helpers.

**Findings:** any new code that materializes a `Vec` from an `im::Vector`
just to mutate it, or unguarded `truncate` calls.

## Universal Review Lenses

Apply these to every PR. Silence on a lens means it passed.

### L1. Class vs single case

Does the change cover a *class* of cases or just one? Ask the reviewer
themselves to name 3+ examples in the class (cards, screens, error
conditions, multiplayer scenarios). If only one example exists, the change
is a special case dressed as a building block — flag it.

### L2. Sibling coverage

If a fix landed in one site of a class (a Draw resolver, a format picker,
an attack-arrow renderer, an AI classifier), did the siblings need the
same fix? Name them in the finding.

If a parser arm or string was extended (singular, one keyword, one CR
section), are plural / possessive / negated / "an opponent's" / "your" /
"their" / "non-X" / "another" variants covered? List the variants checked.

### L3. Test adequacy

- Does the test exercise the failure path the fix prevents? Constructor
  shortcuts (`create_object` setting `controller = owner`, factory builders
  that bypass production wiring, `setUp` that pre-populates the post-fix
  state) can silently mask the very bug a regression test claims to catch.
- Tests assert the *building block*, not just one input.
- Runtime tests must drive the engine through the pipeline they're testing.
  Tests that manually construct expected state are *shape* tests — label
  them as such; never conflate.
- For UI changes the author couldn't run a browser for: say so explicitly.
  Type-check passing is not feature-correctness.

### L4. Edge cases

Empty inputs (0 mana, 0 targets, empty filter). Multi-target / modal /
repeat-for / multi-seat interactions. Simultaneous events (dies + ETB in
the same SBA pass, copy-of-copy, control change with summoning sickness).
Eliminated players still being referenced. Async race conditions (state
updates after unmount, two reconnects at once).

### L5. Idiomatic code

- Wildcard `_` match arms that should be exhaustive — let the compiler
  catch missing variants.
- `TypeScript`: `as any`, fresh `@ts-expect-error`, unchecked casts at
  trust boundaries.

## Surface-Specific Guidance

### Engine logic (`crates/engine/src/game/`, `crates/engine/src/types/`)

- **Building block reuse:** check `parser/oracle_nom/`,
  `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`,
  `game/ability_utils.rs`, `game/keywords.rs`, `game/zones.rs`,
  `game/targeting.rs` before any new helper. New helpers must justify
  their existence.
- **Owner vs Controller:** player-scoped queries on non-battlefield zones
  (graveyard / library / hand / exile) filter by `obj.owner == player`,
  not `obj.controller` (CR 404.2). Tests using `create_object` set
  `controller = owner` and cannot exercise the divergent case.
- **Replacement pipeline:** zone changes must route through
  `ProposedEvent::ZoneChange` so RIP / Leyline of the Void / "exile
  instead" replacements can apply. Direct `zones::move_to_zone` calls
  bypass the pipeline.

### Parser (`crates/engine/src/parser/`)

- **Composable combinators:** N-dimensional patterns compose `alt()` per
  axis; never enumerate the N! cartesian product as separate `tag("full
  string")` arms.
- **Condition extraction:** `parse_inner_condition` in
  `oracle_nom/condition.rs` is the single authority. Trigger and static
  parsers must delegate — never re-implement condition recognition.
- **Phrase variants:** for each new combinator, the plural / possessive /
  "an opponent's" / "your" / "their" / "non-X" / "another" variants must
  all be covered or explicitly out of scope.

### Frontend / UI (`client/src/`)

- **Display layer purity:** any computation, derivation, filtering, or
  inference of game data inside `client/src/` is a finding — push it into
  the engine and expose the result.
- **Adapter symmetry:** new engine fields exposed to the UI must be wired
  through every adapter (WASM, WebSocket, Tauri, P2P) symmetrically with a
  round-trip test.
- **Reactivity:** `useEffect` deps include the right identity for
  back-to-back prompts. Cleanup on unmount: animations, timers,
  subscriptions, observers.
- **Mobile / touch:** touch targets ≥ 44pt; `:hover`-only state breaks on
  mobile and needs a touch-equivalent.

### Multiplayer / transport (`crates/server-core`, `crates/phase-server`, `client/src/adapter/`)

- **State leak:** new player-visible state filtered through
  `filter_state_for_player` so opponents can't see hidden information
  (hand, library, face-down).
- **Wire round-trip:** new fields encoded and decoded symmetrically across
  all adapters; a round-trip test belongs in `client/src/__tests__/`.
- **Reconnect / N-player:** disconnect grace period honored; lobby
  notifications fire for every joiner/leaver, not just the first one.

### AI (`crates/phase-ai/`)

- **Classifier completeness:** polarity / threat / category classifiers
  cover the full enum, not just the easy variants. Untargeted board wipes
  (`Effect::DestroyAll`, `DamageAll`) are real threats and easy to miss.
- **Deadline correctness:** deadline-bail branches must score candidates
  the same way as the no-bail branch — composing `tactical + penalty`
  once, not weighting one side and not the other.
- **Cache keys:** reflect the full set of inputs that change AI decisions.
  Hashing only `hand.len()` and not contents collides distinct positions.

### Deck / format / feeds

- **Format identity:** singleton check uses `Basic` supertype, not name
  allowlists. Banlists accurately reflect the format (Commander vs Duel
  Commander vs Pauper Commander differ).
- **Feed safety:** refuse to overwrite cached state with empty / zero-deck
  responses on both server and client sides.

## Things Explicitly NOT to Flag

- Formatting / whitespace / line length — `cargo fmt` and `prettier`
  enforce these in CI.
- Clippy warnings — `cargo clippy --all-targets -- -D warnings` runs in
  CI; do not duplicate.
- Generated artifacts: `client/public/card-data.json`,
  `client/src/wasm/`, `target/`, `node_modules/`.
- Diff-based parser combinator violations — `scripts/check-parser-combinators.sh`
  enforces this in CI. (You may still flag *verbatim string-equality
  Oracle matches* because the CI gate does not catch them.)
- Test snapshot diffs — these are expected when parser output changes.
- Pre-existing patterns the author did not modify, unless directly
  relevant to a finding.

## Output Style

For each finding:

> **[HIGH/MED/LOW]** *short summary.* Evidence: `path/to/file.rs:line`.
> Why it matters: 1 sentence. Suggested fix: 1 line.

Silence on a dimension = passed. Findings only — no diff recap, no
praise, no LGTM padding. Cap reviews at a focused, useful set of
findings; do not pad to look thorough.
