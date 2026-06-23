# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

phase.rs is a Magic: The Gathering game engine written in Rust (compiling to native + WASM) with a React/TypeScript frontend. It implements MTG game rules using functional architecture (discriminated unions, pure reducers, immutable state) with an Arena-quality browser UI. Card data is sourced from MTGJSON (MIT-licensed) with custom typed JSON ability definitions.

## Design Principles — READ THIS FIRST

**Above all else, this project prioritizes three co-equal pillars: idiomatic Rust, composable building-block architecture, and strict fidelity to the MTG Comprehensive Rules. These are non-negotiable and override convenience, speed-of-delivery, or "getting it working." Every code change must pass through all three lenses before anything else.**

- **Idiomatic Rust, always.** Use Rust's type system, ownership model, and idioms to their fullest. Prefer `enum` over stringly-typed data. Prefer exhaustive `match` over fallback defaults. Prefer trait-based polymorphism over dynamic dispatch when the type set is known. If the idiomatic path is harder, take it anyway — shortcuts compound into debt.
- **Rules-correct over convenient — the #1 hard rule.** This is an MTG rules engine — correctness to the Comprehensive Rules is a hard requirement, not a nice-to-have. Every implementation pattern MUST be verified against the relevant CR section before it is considered complete. When a rules-correct implementation is more complex than a shortcut, take the complex path. A simpler implementation that gets the rules wrong is not simpler — it is wrong. If you are unsure whether a behavior is rules-correct, look up the CR section, annotate the code, and implement what the rules say, not what seems reasonable. "It works for most cases" is not acceptable when the CR specifies exact behavior. No game logic ships without CR validation.
- **Build for the class, not the card.** Every new enum variant, parser pattern, effect handler, or filter must handle a *category* of cards, not a single card. Before writing any logic, ask: "How many cards does this cover?" If the answer is one, you're building a special case — find the general pattern and build that instead. A one-off that works for one card but breaks for the next card with the same pattern is not a building block; it is technical debt.
- **The engine owns all logic.** All game rules, validation, derived state, and computed values live in the `engine` crate. Transport layers (WASM bridge, Tauri IPC, WebSocket server) are thin serialization boundaries — zero game logic allowed. If multiple consumers need the same behavior, it belongs in the engine. Never duplicate logic across adapters. When in doubt, put it in the engine.
- **The frontend is a display layer, not a logic layer.** The React client renders engine-provided state and dispatches user actions — nothing more. It must never compute, derive, transform, or re-interpret game data. If the frontend needs a value, the engine must provide it. Formatting for display (e.g., string interpolation of engine-provided fields) is acceptable; calculating, filtering, or inferring game state is not. Any "smart" frontend code is a bug — move it to the engine.
- **Compose from building blocks.** Every new capability should be decomposed into reusable primitives that unlock future features. Before writing specific logic, ask: "What is the general pattern here?" and build that instead. This applies equally to data modeling: when a new field or parameter needs to distinguish cases, use an existing typed enum (e.g., `ControllerRef`, `Comparator`, `Option<T>`) — never a raw `bool`. A boolean isn't composable; an existing type is self-documenting, extensible, and expresses the full design space. Examples: `contains_possessive`/`contains_object_pronoun` for Oracle text matching, `ChangeZone` + `Shuffle` composition for compound shuffles, `Option<ControllerRef>` for "whose turn is required" instead of `requires_your_turn: bool`.
- **Parameterize, don't proliferate.** Before adding a sibling variant to an enum, ask: *is the new variant a leaf-level parameterization of an existing variant's structural axis (scope, target, aggregate function, condition shape)?* If yes, refactor the existing variants into a parameterized form (e.g., `LifeTotal { player: PlayerScope }` instead of `LifeTotal` + `OpponentLifeTotal` + `TargetLifeTotal`; `UnlessQuantity { comparator, filter, count }` instead of `UnlessControlsCountMatching` + `UnlessControlsMatching` + `UnlessControlsOtherLeq`). Adding a sibling to an enum that should be parameterized compounds debt exponentially: one sibling is cheap, ten siblings make the eventual refactor multi-week as call sites multiply across parser, converter, resolver, and tests. **Sibling-cluster smell:** when an enum has three or more variants that share a name root (X / OpponentX / TargetX / AllX), differ only in a context label, or only differ in a comparator/aggregator/scope axis, that's a parameterization that didn't happen — refactor before extending. The strict-failure tag is the right place to leave coverage waiting while the architecture wins. **Categorical boundary rule:** the parameterization axis must lie within a single CR rule section. Life is CR 119 (player-only). Power/toughness are CR 208/209 (creature/planeswalker). Don't unify these under one `Life { target: {Self,Opponent,Creature}, type: {Total,Remaining} }` — that conflates rule sections the engine treats as separately resolvable. Cross-section unification belongs at `TargetFilter` or at the effect handler (`Effect::DealDamage` per CR 120 unifies all damage subjects), never at the leaf-reference layer. **Discoverability:** before any engine variant proposal, grep `data/engine-inventory.json` (auto-generated by `cargo engine-inventory`; gitignored — run `cargo engine-inventory` to (re)generate it locally first) for existence verification and sibling-cluster smells. The inventory is the canonical source of engine surface — replaces hand-maintained CLAUDE.md lists that drift. Run the workspace `add-engine-variant` skill checklist as the runnable gate; CLAUDE.md is the principle, the skill is the gate.
- **Nom combinators on the first pass — no exceptions.** All new parser code MUST use nom combinators (`tag()`, `alt()`, `value()`, `terminated()`, `pair()`, etc.) from the very first line written. Never write `find()`, `split_once()`, `contains()`, or `starts_with()` for parsing dispatch and then "plan to convert to combinators later." There is no later — write it correctly the first time. Use `nom_on_lower` bridge for mixed-case text, `tag().parse()` for already-lowercase text. Use existing building blocks (`parse_single_cost`, `parse_target`, `parse_for_each_clause`, etc.) for composed operations. If you catch yourself writing string matching for parsing, stop and rewrite with combinators before proceeding. This has been a recurring issue and is non-negotiable.
- **Extend, don't hack.** New features should slot cleanly into existing patterns (effect handlers, game modules, ability definitions). If a feature requires working around the architecture, the architecture should be extended first.
- **Trace before you build.** Before implementing a new pattern, trace how an existing analogous feature works end-to-end (e.g., trace `enter_tapped` before building `enter_with_counters`; trace `Changeling` before building a new CDA). This prevents reinventing existing infrastructure and ensures consistency.
- **Production quality, always.** Write code as if a professional team will audit every line. No "good enough for now." No tech debt IOUs. Every function should be clear, every abstraction should earn its keep, and every pattern should be consistent across the codebase.
- **Single authority for ability costs.** When an ability has costs (tap, sacrifice, pay life, discard, etc.), all cost resolution must go through one authoritative resolver function. Callers dispatch activation — they never inspect or handle individual cost components. This prevents scattered responsibility where every call site must remember to sacrifice Treasures, pay life, or handle future cost types. If you find yourself checking an ability's cost structure at a call site, you're in the wrong layer — push it into the resolver.

### When in Doubt

- Is this logic in the right crate? → It probably belongs in `engine`.
- Am I fighting the type system? → Redesign the types, don't work around them.
- Should I add a special case? → Extend the existing pattern instead.
- Am I solving one card or a pattern? → Build the building block, not the special case.
- Is this the Rust way? → Check how `std` and well-known crates solve similar problems.
- Does this match the Comprehensive Rules? → Look up the CR section, annotate the code, implement what it says.
- Am I computing something in the frontend? → Move it to the engine and expose it in the state.

### CRITICAL: Multi-Agent Safety — Do Not Revert Other Agents' Work

**NEVER revert, overwrite, remove, or undo changes that you did not make.** Multiple AI agents may be working on this codebase concurrently. If you encounter unfamiliar code, new types, new files, or changes you don't recognize:

1. **Do not delete or rewrite them.** They are another agent's in-progress work.
2. **Work around them.** Your edits must be surgical — add only what you need without disturbing surrounding code.
3. **Never use `Write` to replace an entire file** when `Edit` with a targeted `old_string`→`new_string` would suffice. Whole-file rewrites destroy other agents' concurrent changes.
4. **If a file has been modified since you last read it**, re-read it before editing. The new content is intentional.
5. **Never `git checkout`, `git restore`, or `git stash`** files you didn't modify. These operations destroy other agents' uncommitted work.
6. **Never use `git stash` for any reason.** Do not stash to test something, compare branches, or check pre-existing state. Stashing risks merge conflicts on pop and can destroy in-progress work across the working tree. If you need to verify pre-existing behavior, use `git show` or `git diff` against a commit ref instead.

Violating this rule causes cascading failures across the team. Treat every line you didn't write as load-bearing.

**Defer to other active agents to fix their own errors.** If you run into compile, clippy, formatting, or test errors that are unrelated to your own work, wait a few minutes and check again before intervening. Repeat this patience loop while the error appears likely to belong to another active agent's in-progress changes. If the same unrelated error is still present after multiple waiting iterations, such as roughly 10 minutes, then you may proceed to fix the issue while preserving all unrelated work.

### Agent Team Orchestration Standards

When creating or participating in an agent team (whether triggered by `/batch-mechanics` or auto-initiated):

1. **Use existing skills.** Every implementation must follow the relevant skill checklist (`/add-engine-effect`, `/add-keyword`, `/add-trigger`, etc.). No ad-hoc approaches.
2. **Teammates cannot spawn subagents.** All review subagents must be spawned by the lead. The lead receives the plan/implementation from the teammate, spawns a review subagent (model: opus), and sends feedback back to the teammate. This review loop repeats until clean (max 3 rounds).
3. **Sequential execution by default.** Multiple teammates must not implement concurrently unless their file sets are completely disjoint. Shared files like `types/ability.rs`, `effects/mod.rs`, and `parser/oracle.rs` are frequent collision points.
4. **Use risk-scaled verification.** Run `cargo fmt --all`, then verify at the cheapest level that gives useful signal for the change. For small parser/AST-only fixes, run the parser combinator gate and targeted semantic checks, then let Tilt continue in the background; do not block every minor commit on full `clippy` + `test-engine` + `card-data` green. For non-trivial engine plumbing, shared target/stack/state-machine changes, frontend changes, or before marking an issue fixed-unreleased, collect stronger evidence from the relevant Tilt resources. Do NOT run cargo build/clippy/test directly — Tilt handles these continuously and running them manually causes target lock contention. TypeScript errors must not be committed.

### CRITICAL: Use Tilt Logs Instead of Running Builds

**Tilt is always running and continuously rebuilds on file changes.** Do NOT run `cargo build`, `cargo clippy`, `cargo test -p engine`, `pnpm run type-check`, or `pnpm lint` directly — these compete for cargo target locks and queue up redundant builds. Instead, check the Tilt logs for the relevant resource to see if your changes compiled/passed.

Read results with `tilt logs <resource> --tail N --since 2m`, or wait with `./scripts/tilt-wait.sh <resources>` (exit `0` ok, `1` terminal error, `124` timeout). Resources: `clippy`, `test-engine`, `test-ai`, `wasm`, `card-data`, `check-frontend`, `test-frontend`, `server`, manual `coverage`. Two non-negotiable traps: (1) do NOT treat `.status.buildHistory[0].error` as actionable while `.status.currentBuild.spanID` is present — only diagnose after `updateStatus == "error"` **and** `currentBuild.spanID == "none"` (`pending` with no span = queued behind a cargo lock; wait, don't shell out to cargo); (2) `cargo fmt --all` is the one command always run directly (Tilt doesn't auto-format). Detect whether Tilt is up with `tilt get uiresource clippy >/dev/null 2>&1` (exit 0 = up). Full resource table, log/wait recipes, and operational rules are in the **`project-reference`** skill.

**Risk-scaled verification cadence:** `cargo fmt --all` always runs directly (Tilt doesn't auto-format). For all other checks, prefer `tilt logs <resource>` / `./scripts/tilt-wait.sh` when Tilt is up, and fall back to direct cargo/pnpm only when it is down (`tilt get uiresource clippy >/dev/null 2>&1`; exit 0 = up). The full risk-scaled recipe (fast parser loop → full Rust verification → frontend, with `set -e`/CI caveats) lives in the **`project-reference`** skill. One-shot audit binaries (`cargo coverage`, `cargo semantic-audit`, `cargo parser-gaps`, `cargo rules-audit`) are not Tilt resources — invoke them directly.

### CRITICAL: Building Blocks and Architecture Purity

**Before writing any logic, search for existing building blocks.** Duplicating what these already do is a defect. Check these modules before writing new utility functions:

| Module | What lives here |
|--------|----------------|
| `parser/oracle_nom/` | **Nom 8.0 combinator foundation** — shared typed combinators used by all parser branches. `primitives.rs`: `parse_number`, `parse_number_or_x`, `parse_mana_symbol`, `parse_mana_cost`, `parse_color`, `parse_counter_type`, `parse_pt_modifier`, `parse_roman_numeral`. `target.rs`: target phrase combinators. `quantity.rs`: quantity expression combinators (including `parse_quantity_ref`, `parse_target_power_ref`). `duration.rs`: duration combinators. `condition.rs`: condition combinators. `filter.rs`: filter combinators. `error.rs`: `OracleResult` type, `oracle_err`. To express "parser couldn't handle this", the single authority is `Effect::unimplemented(name, fragment)` (`types/ability.rs`) — never hand-construct `Effect::Unimplemented { .. }` literals (gated). `context.rs`: `ParseContext` for stateful parsing. `bridge.rs`: `nom_on_lower` (run nom on lowercase, map remainder to original case), `nom_on_lower_required` (same with Result), `nom_parse_lower` (discard remainder). All parsers delegate atomic and structural operations to these combinators. `dispatch_line_nom` in `oracle.rs` uses these as the primary dispatch path for unclassified lines. |
| `parser/oracle_util.rs` | `TextPair` (paired original/lowercase slices with `strip_prefix`/`strip_suffix` for case-insensitive matching preserving original case), `parse_number` (delegates to `nom_primitives::parse_number` with word-boundary guard and X→0 fallback), mana symbol parsing, reminder text stripping, possessive/pronoun phrase matching, phrase variant helpers, subtype canonicalization, filter merging, `SELF_REF_TYPE_PHRASES` (normalization-safe self-reference constant), `SELF_REF_PARSE_ONLY_PHRASES` (parse-only self-references excluded from `~` normalization) |
| `parser/oracle_quantity.rs` | Semantic quantity interpretation: `parse_cda_quantity`, `parse_quantity_ref`, `parse_event_context_quantity`, `parse_for_each_clause` — maps Oracle text phrases to typed `QuantityExpr`/`QuantityRef` values |
| `parser/oracle_target.rs` | Target extraction from Oracle text (`"target creature"` → `TargetFilter`), type phrase parsing, event context refs |
| `parser/oracle_static.rs` | Static ability line parsing, continuous modification extraction (`"gets +N/+M and has flying"` → typed modifications), `strip_casting_prohibition_subject` (shared subject→`ProhibitionScope` extractor for all casting prohibition patterns) |
| `game/filter.rs` | Runtime `TargetFilter` evaluation against game objects and players |
| `game/zones.rs` | Zone manipulation primitives — creating, moving, adding, removing objects |
| `game/targeting.rs` | Target legality, zone queries (`zone_object_ids` for all objects in a zone), and target validation |
| `game/quantity.rs` | Dynamic quantity resolution (`QuantityExpr` → concrete `i32` from game state). `ObjectCount` uses `TargetFilter::extract_in_zone()` to count objects in the correct zone (not just battlefield). `CountersOnTarget` mirrors `TargetPower` pattern — resolves against the first object target. |
| `game/ability_utils.rs` | Ability construction, target slot wiring, chained ability building, target selection/validation |
| `game/keywords.rs` | Keyword presence queries, protection checks, keyword string parsing |

**Self-review every change as you go.** After writing code, ask:
1. Did I duplicate logic that an existing helper already handles?
2. Is this inline extraction something that should use a shared building block?
3. Would this logic work for 50 cards, or just the one I'm looking at?
4. Did I extend the general pattern, or write a special case?

If the answer to any of these is wrong, **stop and refactor before moving on.** Do not leave architectural debt for later — fix it now, in the same change.

**Test the building block, not the special case.** Tests should verify that composable primitives work correctly across their full input range — not just that one card's Oracle text parses. A parser test for "exile target creature" is more valuable than a test for a single card name. Effect handler tests should exercise the handler's parameters, not replay a single card's resolution. When a building block is extended, add tests for the new capability at the building-block level.

## Reference (build, architecture, env vars, releasing, CI)

Build/test/cargo commands, cargo aliases, WASM build, card-data pipeline + jq lookups, frontend commands, coverage, full crate/workspace architecture, engine internals, environment variables, releasing, and CI live in the **`project-reference`** skill (`.claude/skills/project-reference/SKILL.md`, shared with Codex via `.agents/skills` / `.codex/skills`). Invoke it or read the file when you need a command, a module location, or an env var — it is not resident every turn by design.

AI tactical policy scoring uses the card-equivalent `PolicyVerdict` contract in `crates/phase-ai/src/policies/registry.rs`; new policies must use the band helpers rather than raw sentinel scores. AI behavior changes must run `cargo ai-gate` and refresh baselines only with the paired-seed report attached.

## Documentation (`docs/`)

- **`.claude/skills/oracle-parser/SKILL.md`** — Oracle parser single source of truth: architecture, nom combinator mandate, parsing priority system, AST type system, all helper modules, CR annotation protocol, and contribution checklists.
- **`docs/MagicCompRules.txt`** — Full MTG Comprehensive Rules text from Wizards of the Coast. **Gitignored — not redistributed in this repo.** Run `./scripts/fetch-comp-rules.sh` once to download a local copy. Use this as the authoritative source when verifying CR numbers, looking up rule text, or annotating new game logic. `grep -n "^702.180" docs/MagicCompRules.txt` to look up any rule.
- **`.claude/skills/add-engine-effect/SKILL.md`** — Complete checklist for adding a new effect to the engine: types → parser → resolver → targeting → multiplayer filter → frontend → AI → tests. Covers every registration point that must be updated in lockstep. **Use this as the authoritative guide for any new effect work.**

## Conventions

### Rust Idioms — Write It Right the First Time

These patterns must be used on first write, not fixed after clippy complains:

- **`strip_prefix`/`strip_suffix`** over `starts_with` + manual slicing: `if let Some(rest) = s.strip_prefix("foo")` not `if s.starts_with("foo") { &s[3..] }`. **Compose from `std` primitives** — chain `strip_prefix` calls for multi-part patterns: `s.strip_prefix(word)?.strip_prefix(' ')?` not `format!("{word} ")` + `strip_prefix`. The standard library's string methods are building blocks; use them compositionally rather than constructing new strings to match against. Note: `strip_prefix` is still correct for `TextPair` dual-string operations and structural uses (punctuation stripping, dynamic string prefixes), but NOT for parsing dispatch (use nom `tag()` instead).
- **`TextPair` for dual-string parsing** — when matching on lowercase text but preserving original casing, use `TextPair::new(original, &lower)` and its `strip_prefix`/`strip_suffix` methods instead of manually computing `&text[text.len() - rest.len()..]` or `&text[prefix.len()..]`. In functions where `TextPair` cannot be constructed (e.g., `parse_target` where `lower` is a local `String` with a shorter lifetime than the returned `&str`), the `text.len() - rest.len()` offset idiom remains correct. See `oracle_util.rs`.
- **`oracle_nom` combinators** (see design principle above): use `nom_on_lower` bridge for mixed-case text, `tag().parse()` for already-lowercase text, and existing building blocks (`parse_single_cost`, `parse_target`, `parse_for_each_clause`, etc.) for composed operations. Use `parse_number_or_x` when X resolves to 0 (costs, P/T, counters); use `parse_number` when X should remain as `Variable("X")` (effect quantities). `parse_article_number` guards against word-boundary bugs (e.g., "another" matching as "a").
- **Iterator methods** over range-indexed loops: `for item in slice.iter().skip(1)` not `for i in 1..slice.len()`
- **`rsplit(' ').next()`** to get the last word, not `rsplit().collect::<Vec>().first()`
- **Exhaustive `match`** without wildcard fallbacks when the enum is known — let the compiler catch missing arms
- **Reuse existing building blocks** before writing one-off string logic. See the helper reference table in the "Building Blocks and Architecture Purity" section above
- **NEVER match on verbatim Oracle text strings** (e.g. `if lower == "the number of cards in your hand is greater than your life total"`). This is the single most prohibited pattern in the codebase. Every Oracle phrase must be decomposed into typed building blocks (grammar prefix/suffix stripping, composable helpers, typed enum variants). A verbatim string match handles exactly one card and poisons the parser architecture permanently. Instead: identify the grammatical structure, add typed `QuantityRef`/`Comparator`/`FilterProp` variants as needed, and parse with `strip_prefix`/`split_once` + helpers so the pattern covers every card in the class.
- **Compose nom combinators, don't enumerate permutations.** When a pattern has N independent dimensions (prefix × quantity × target × suffix), compose them with chained `alt()` + `tag()` calls — never expand into N! individual `tag("full string")` arms. Each axis of variation should be a single `alt()` call, chained sequentially. Example: `alt((tag("you put "), tag("you've put "))).parse(i)?; alt((tag("a counter"), tag("one or more counters"))).parse(i)?; tag(" on a ").parse(i)?; alt((tag("permanent"), tag("creature"))).parse(i)?;` — not 8 separate `tag("you put a counter on a permanent ...")` alternatives. The same principle applies to condition extraction: `parse_inner_condition` in `oracle_nom/condition.rs` is the **single authority** for all game-state conditions. Trigger and static parsers must delegate to it — never re-implement condition recognition as bespoke string matching. Only source-referential patterns ("if you cast it", "if it's attacking") that fundamentally require the trigger source as context may live outside the combinator.
- **Nest nom combinators by prefix dispatch.** When multiple `alt()` branches share a common prefix (e.g., `"during your upkeep"`, `"during your end step"`, `"during your turn"`), nest them: `preceded(tag("during "), parse_during_phrase)` where `parse_during_phrase` is a sub-combinator that dispatches on the remainder. This eliminates redundant prefix matching and mirrors BNF grammar production rules. Factor shared sub-patterns (e.g., opponent possessive `"an opponent's "` / `"an opponents "`) into reusable combinators. **When NOT to nest**: don't nest when alternatives are leaf-level variants of the same concept (e.g., apostrophe normalization) with no shared structural prefix.
- **Word-boundary scanning for multi-position phrase matching.** When timing/keyword phrases can appear at any position in a string (not just the start), use a scanning loop that tries a nom combinator at each word boundary: `while !remaining.is_empty() { if let Ok((rest, val)) = combinator(remaining) { results.push(val); remaining = rest.trim_start(); } else { remaining = remaining.find(' ').map_or("", |i| remaining[i+1..].trim_start()); } }`. This replaces scattered `contains()` chains with a single combinator tried at word boundaries — more precise (matches complete phrases, not arbitrary substrings) and defines all patterns in one composable combinator. See `scan_timing_restrictions` in `oracle_casting.rs` and `scan_for_phase` in `oracle_trigger.rs`.
- **Separate abstraction layers in enum design.** An enum variant must belong to exactly one semantic layer — do not conflate different concepts in the same type. Example: `QuantityRef` (a *reference* to a dynamic game value: `HandSize`, `LifeTotal`) must not contain `Fixed { value: i32 }` (a *constant* that requires no game-state lookup). Instead, introduce a wrapping expression type (`QuantityExpr`) that is either a `Ref(QuantityRef)` or a `Fixed(i32)`. Ask: "Does this variant belong to the same abstraction as all the others, or does it belong one level up?" Wrong layer placement creates API confusion, breaks exhaustive match semantics, and forces callers to handle heterogeneous cases that should be uniform.

### MTG Comprehensive Rules Annotations

**Any code that implements, enforces, or directly references an MTG game rule MUST be annotated with the corresponding Comprehensive Rules (CR) number.** This is not optional — it is a required part of every rules-related change, same as `cargo fmt`.

**Lookup:** The full Comprehensive Rules text is available at `docs/MagicCompRules.txt`.

**CRITICAL — Verification is mandatory, not optional:**
Every CR number you write MUST be verified by grepping `docs/MagicCompRules.txt` BEFORE adding it to code. This is non-negotiable. Do NOT rely on memory or training data for CR numbers — the 701.x keyword action numbers and 702.x keyword ability numbers are especially prone to hallucination because they are arbitrary sequential assignments with no mnemonic pattern. A wrong CR number is worse than no CR number: it creates false confidence that code was verified against the wrong rule.

```bash
# REQUIRED before writing any CR annotation:
grep -n "^701.21" docs/MagicCompRules.txt   # Verify: is 701.21 really Sacrifice?
grep -n "^702.122" docs/MagicCompRules.txt  # Verify: is 702.122 really Crew?
```

If you cannot find the rule number in `docs/MagicCompRules.txt`, do NOT write the annotation. Flag it as "needs manual verification" instead.

**Format:**
```rust
// CR 704.5a: A player with 0 or less life loses the game.
// CR 702.2c + CR 702.19b: Deathtouch with trample assigns lethal (1) to each blocker.   // interacting rules use `+`
// CR 704.3 / CR 800.4: SBAs may have ended the game during phase auto-advance.          // alternatives use `/`
/// Checks state-based actions (CR 704).                                                  // doc comment on rule-implementing function
```

**Rules:**
- **Prefix:** Always `CR`. Never `Rule`, `MTG Rule`, or bare numbers.
- **Number format:** `CR XXX`, `CR XXX.Y`, or `CR XXX.Ya`. Regex: `CR \d{3}(\.\d+[a-z]?)?`
- **Description is mandatory.** A bare `CR 704.5a` with no explanation is not acceptable — grep output must be self-documenting.
- **Placement:** Directly above or inline with the code that implements the rule.

**When writing or modifying engine code (`crates/engine/`):**
1. If you are adding new game logic, identify which CR rule(s) it implements and annotate.
2. If you are modifying existing game logic, verify existing CR annotations are present and still accurate. Add missing annotations.
3. If existing code near your change uses an old format (`Rule 514.1`, `MTG Rule 727`, `MTG 702.36`), migrate it to the `CR` format as part of your change.
4. Do not annotate boilerplate, serialization, or plumbing — only code that implements a game rule.

**Lookup:** `grep -r "CR 704" crates/engine/` finds all state-based action implementations. `grep -rn "CR \d" crates/engine/` lists all rule annotations. The `mtg-rules-auditor` agent can produce a full coverage report on demand.

### General Conventions

- Rust: `cargo fmt` + `clippy -D warnings` enforced in CI
- TypeScript: ESLint with `@typescript-eslint/recommended`, unused vars prefixed with `_`
- Frontend uses Tailwind CSS v4, Framer Motion for animations
- Tests colocated in `__tests__/` directories (frontend) or inline `#[cfg(test)]` modules (Rust)
- The `release` profile is optimized for WASM size: `opt-level = 'z'`, LTO, single codegen unit, stripped
- **jq lookup keys consumed by JS must use `js_downcase`, never `ascii_downcase`.** jq's `ascii_downcase` folds only `A–Z`, so an uppercase accented letter (e.g. `É` in Éomer/Éowyn) survives in the key, but the frontend resolves every lookup with JS `.toLowerCase()` which folds `É → é` — the keys silently never match and name-keyed image/data lookups return nothing. The `gen-scryfall-*.sh` scripts share a `js_downcase` jq helper (`scripts/lib/scryfall-fetch.sh`) that matches JS `toLowerCase()` for the Latin-1 accented range. Engine-keyed files (`card-data.json`) are unaffected — Rust `to_lowercase()` is already Unicode-aware. `ascii_downcase` stays correct only for ASCII-only keys (oracle ids, set codes, Scryfall UUIDs). Note: these Scryfall data files are gitignored and served from R2 in prod, so fixing the script requires regenerating and redeploying the data.

## Releasing

Use `cargo-release` via the workspace alias (`cargo release-local <version>`) — **never tag manually with `git tag`**. Full release + CI steps are in the **`project-reference`** skill; the **`ship-commits`** skill handles landing commits through the merge queue.

## Planning

Project planning docs live in `.planning/` with phase-based organization (phases 01-09+). Each phase has CONTEXT, RESEARCH, PLAN, SUMMARY, and VERIFICATION docs. `PROJECT.md` contains the project manifest with requirements and key decisions.
