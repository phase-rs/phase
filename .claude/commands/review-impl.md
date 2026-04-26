Review the implementation in scope (uncommitted diff, the just-finished agent's work, or named files) for GAPS — things that are *missing* or *wrong*, not style nits.

**Step 1: Identify what was changed.** Read the diff and classify the surface area: engine logic, parser, frontend/UI, multiplayer/transport, AI heuristics, deck/format/feeds, build/CI/release, docs. Most changes touch one or two of these; a few touch many. Apply only the relevant lenses below.

**Step 2: Skip what CI already enforces.** Do not duplicate these gates:
- `scripts/check-parser-combinators.sh` — diff-based parser combinator gate (forbids new `.contains("...")` / `.starts_with("...")` / `.find("...")` / `.split_once("...")` for parser dispatch).
- `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`.
- `scripts/coverage-regression-check.sh --fail-on-engine` — fails the build if a previously-supported card loses engine support on unchanged Oracle text.
- TypeScript: `pnpm type-check` + `pnpm lint` (frontend changes).

**Step 3: Probe the universal dimensions.** Silence on a dimension means it passed.

---

## Universal (apply to every change)

### U1. Class vs Single Case
- Does the change cover a *class* of cases, or just one? Name 3+ examples in the class — cards, screens, error conditions, multiplayer scenarios — whatever's relevant for the surface being changed. If you can only name one, the change is a special case dressed as a building block.
- For UI: does the fix work across mobile / desktop / Tauri / multiplayer seats, or just the variant the user reported?
- For AI heuristics: does the policy cover the full category of game state (e.g., not just targeted threats but untargeted board wipes too)?

### U2. Sibling Coverage
- If a fix landed in one site of a class (Draw resolver, format picker, attack-arrow renderer, AI classifier), did the siblings need the same fix? Name them.
- If a parser arm or string was extended (singular form, one keyword, one CR section), are plural / possessive / negated / sibling variants covered?

### U3. Test Adequacy
- Does the test exercise the failure path the fix prevents? Constructor shortcuts (`create_object` setting `controller = owner`, factory builders that bypass the production wiring, `setUp` that pre-populates the post-fix state) can mask the very bug a regression test claims to catch.
- Tests assert the *building block*, not just one input?
- For UI changes you can't run a browser for: say so explicitly. Type-check passing is not feature-correctness.

### U4. Edge Cases (whichever apply)
- Empty inputs (0 mana, 0 targets, empty filter, empty list).
- Multi-target / modal / repeat-for / multi-seat interactions.
- Simultaneous events (dies + ETB in the same SBA pass, copy-of-copy, control change with summoning sickness, two players reconnecting at once).
- Eliminated players still being referenced.
- `im::Vector::truncate(n)` panics if `n > len` — guarded?
- Async race conditions (state updates after unmount, two requests in flight).

### U5. Idiomatic Code (the reviewer-only checks)
- New `bool` fields where an existing typed enum (`ControllerRef`, `Comparator`, `Option<T>`) would express the design space.
- Wildcard `_` match arms that should be exhaustive.
- Verbatim Oracle text strings in match arms (`if lower == "verbatim text"`) — CI's combinator gate doesn't catch this.
- TypeScript: `as any`, fresh `@ts-expect-error`, unchecked casts at trust boundaries.

---

## Engine logic (`crates/engine/src/game/`, `crates/engine/src/types/`)

### E1. CR Annotation Correctness (HARD GATE)
- For every `// CR <rule>` introduced or moved: was the rule number verified by `grep -n "^<rule>" docs/MagicCompRules.txt` BEFORE merge?
- Does the cited rule's body actually describe what the code is doing? CR 119 / 120 / 121 are adjacent and easily confused (119 = starting life, 120 = damage, 121 = drawing). 701.x and 702.x keyword numbers are arbitrary and prone to hallucination.
- See `.claude/skills/validate-cr-annotations/SKILL.md`.

### E2. Building Block Reuse
- Did the change duplicate logic in `parser/oracle_nom/`, `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`, `game/ability_utils.rs`, `game/keywords.rs`, `game/zones.rs`, `game/targeting.rs`?
- New helpers must justify their existence — is there truly no existing helper?

### E3. Engine ≠ Display Layer
- Game logic in the engine. Any leak into frontend, WASM bridge, or transport adapter is a gap.
- Multiplayer state filter (`filter_state_for_player`) updated when new player-visible state was added?

### E4. Owner vs Controller
- Player-scoped queries on non-battlefield zones (graveyard / library / hand / exile) should filter by `obj.owner == player`, not `obj.controller`. CR 404.2.
- Any test that uses `create_object` to set up the state will silently set `controller = owner` — it cannot exercise the divergent case.

### E5. Replacement Pipeline
- Zone changes should route through `ProposedEvent::ZoneChange` so RIP / Leyline of the Void / "exile instead" replacements can apply. Direct `zones::move_to_zone` calls bypass the pipeline.

---

## Parser (`crates/engine/src/parser/`)

### P1. Verbatim String Match (PROHIBITED)
- Any new `if lower == "verbatim oracle phrase"`? That handles exactly one card and poisons the parser permanently. Decompose into typed building blocks.
- The CI combinator gate catches `.contains/.find/.starts_with("...")` for dispatch, but not equality on full strings — manual review must.

### P2. Phrase Variant Coverage
- For each new tag/alt/preceded combinator: are plural / possessive / "an opponent's" / "your" / "their" / "non-X" / "another" variants covered? List the ones you checked.

### P3. Composable Combinator Layering
- N-dimensional patterns should compose `alt()` calls per axis, not enumerate the N! cartesian product as separate `tag("full string")` arms. See CLAUDE.md "Compose nom combinators, don't enumerate permutations."

---

## Frontend / UI (`client/src/`)

### F1. Display Layer Purity
- The frontend renders engine-provided state. Any computation, derivation, filtering, or inference of game data is a gap — push it into the engine.
- Engine fields exposed to the UI must be wired through the adapter (WASM / WebSocket / Tauri / P2P) symmetrically. Round-trip new fields in tests.

### F2. Reactivity & Lifecycle
- `useEffect` deps include the right identity? Stale state across back-to-back prompts is a common gap (e.g., reusing the same `ChoiceModal` for two prompts in a row).
- Cleanup on unmount: animations, timers, subscriptions, observers.

### F3. Mobile / Touch / a11y
- Touch targets ≥ 44pt. Hover-only affordances have a touch-equivalent. `:hover`-only state breaks on mobile.
- Modal / sheet scroll containment correct on iOS Safari?

### F4. Empty / Loading / Error States
- Each new screen / panel / modal handles empty-data, loading, and error explicitly?

---

## Multiplayer / transport (`crates/server-core`, `crates/phase-server`, `client/src/adapter/`)

### M1. State Leak
- New player-visible state filtered through `filter_state_for_player` so opponents can't see hidden information (hand, library, face-down)?

### M2. Wire Round-Trip
- New fields encoded and decoded symmetrically across all adapters (WASM, WebSocket, Tauri, P2P)? A regression test that round-trips the field belongs in `client/src/__tests__/`.

### M3. Reconnect / N-Player
- Disconnect grace period honored?
- Notification fires for every joiner / leaver, not just the first one in a 3+ player lobby?

---

## AI (`crates/phase-ai/`)

### AI1. Classifier Completeness
- Polarity / threat / category classifiers must cover the full enum, not just the easy variants. Untargeted board wipes (`Effect::DestroyAll`, `DamageAll`) are real threats; non-`target`-bearing effects are easy to miss.

### AI2. Deadline Correctness
- Deadline-bail branches must score candidates the same way as the no-bail branch — composing `tactical + penalty` once, not weighting `(tactical + penalty) * tactical_weight` on one side and adding penalty unweighted on the other.
- Cache keys must reflect the full set of inputs that change AI decisions. Hashing only `hand.len()` and not contents collides distinct positions.

### AI3. Combinatorial Bounds
- Any combination / permutation generator should short-circuit on infeasibility (e.g., total power below threshold) before enumerating.

---

## Deck / format / feeds (`crates/engine/src/game/deck_validation.rs`, `crates/feed-scraper`, `client/src/services/feedService.ts`)

### D1. Format Identity
- Singleton check uses `Basic` supertype, not name allowlists.
- Banlists / restrictions / partner rules accurately reflect the format (Commander vs Duel Commander vs Pauper Commander differ).

### D2. Feed Safety
- Refuse to overwrite cached state with empty / zero-deck responses on both server and client sides.

---

## Output Format

For each finding:
- **[HIGH/MED/LOW]** *short summary.* Evidence: `path/to/file.rs:line`. Why it matters: 1 sentence. Suggested fix: 1 line.

Silence on a commit = LGTM. Findings only — no diff recap, no praise.
