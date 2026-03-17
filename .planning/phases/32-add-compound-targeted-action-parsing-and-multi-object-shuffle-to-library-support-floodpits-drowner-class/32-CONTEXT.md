# Phase 32: Compound Targeted-Action Parsing & Multi-Object Shuffle-to-Library — Context

**Gathered:** 2026-03-17
**Status:** Ready for planning

<domain>
## Phase Boundary

Extend the Oracle parser and effect system to handle two categories of currently-unsupported card patterns:
1. **Compound targeted actions** — "tap X and put a counter on it" (~20+ cards)
2. **Multi-object shuffle-to-library** — "shuffle this and target X into their owners' libraries"

Plus a supporting building block:
3. **Counter-based target filtering** — "creature with a stun counter on it" (dozens of cards)

Floodpits Drowner is the motivating card, but all three building blocks cover broad card classes. Full pipeline regeneration and coverage audit included.

</domain>

<decisions>
## Implementation Decisions

### Generic Compound Effect Splitter (Building Block #1)
- **`try_split_compound(primary: ParsedEffectClause, remainder: &str) -> ParsedEffectClause`** — generic function at the ParsedEffectClause level, NOT specific to targeted actions
- **Handles both "and" and "then" connectors** — "and" implies simultaneous/same-target effects, "then" implies sequential. Both produce sub_ability chains (engine resolves sequentially regardless)
- **`TargetFilter::ParentTarget`** — new variant for anaphoric "it"/"that creature"/"that player" references. Resolution copies the resolved ObjectId/PlayerId from the parent ability's target list
- **Both object and player targets** — ParentTarget resolves to either ObjectId or PlayerId depending on what the parent targeted. Parser detects pronouns ("it" for objects, "that player" for players)
- **Unparseable remainder** — emit `Unimplemented` sub_ability (not discard). Primary effect still resolves, gap is visible in card data
- **Call-site gating** — try_split_compound only runs when the caller KNOWS it has a single-target effect with leftover text. Multi-subject patterns (X and Y as compound subjects) are handled by a separate compound-subject parser
- **Single split, no recursion** — split once at first "and"/"then". Natural recursion occurs through the sub-parse of the remainder (remainder itself may hit try_split_compound)

### Generic Compound Subject Splitter (Building Block #2)
- **`try_split_compound_subject(text: &str) -> Option<(Subject, Subject, &str)>`** — verb-generic helper that detects "X and Y" in subject position
- **Verb-agnostic** — splits subjects only, doesn't know about verbs. Each verb-specific parser (shuffle, exile, return) calls this helper when it detects a compound subject
- **Returns two subjects + remainder** — e.g., "this creature and target creature with a stun counter on it" → (SelfRef, Targeted(Creature, HasCounter(Stun, 1)), remainder)
- **Callers chain effects** — the verb-specific parser creates chained effects of the same type for each subject

### Counter-Based Target Filtering (Building Block #3)
- **`FilterProp::HasCounter { counter_type: CounterType, minimum: u32 }`** — includes minimum count field from the start (default 1 for "with a counter", supports "with three or more")
- **General `parse_counter_filter()`** — parses "with a/an [counter_type] counter(s) on it" for any counter type: stun, shield, +1/+1, -1/-1, loyalty, lore, etc. Uses existing CounterType enum
- **Composed into parse_target() pipeline** — "target creature with a stun counter on it" = Typed(Creature) + HasProp(HasCounter(Stun, 1)). Counter filter slots in as a property suffix in the existing target parsing flow
- **Runtime matching in filter_matches_object()** — new HasCounter match arm reads obj.counters HashMap. Consistent with how all other FilterProp variants are matched

### Multi-Object Shuffle-to-Library (Building Block #4)
- **ChangeZone per object** — decomposed into chained ChangeZone effects via sub_ability. Self-reference via SelfRef, target via normal target resolution. Parser uses try_split_compound_subject to detect compound subject
- **`owner_library: true`** — ChangeZone field indicating destination is owner's library (not controller's). Each object goes to its owner's library per CR 400.7
- **Auto-shuffle per CR 401.3** — ChangeZone-to-Library automatically shuffles the owner's library after each move (unless specific_position is set for "put on top"/"put on bottom"). No explicit Shuffle sub_ability needed for "shuffle into library" patterns
- **No shuffle dedup** — per CR 401.3, shuffling happens per-object-moved. Same-owner double-shuffle is CR-correct and functionally harmless
- **No shuffle if replacement redirects** — if a replacement effect changes the destination (e.g., Rest in Peace → exile), no shuffle occurs. CR 614.6: replacement effects change the event itself
- **Pre-loop SelfRef guard** — ChangeZone handler checks for SelfRef before the normal target loop, resolves source object, and routes through standard zone-change + replacement pipeline (handles commander zone redirection, death triggers, etc.)

### Testing & Validation
- **Full coverage audit** — `cargo coverage` before/after to measure Unimplemented delta across all ~32K cards
- **Full Floodpits Drowner integration test** — GameScenario: cast with Flash, ETB triggers (tap + stun counter via compound split), activate shuffle ability (self + stunned creature into owners' libraries), assert both gone and libraries shuffled
- **Parser snapshot tests** — representative compound patterns for snapshot regression
- **Pipeline regeneration** — `gen-card-data.sh` + `cargo coverage` as final step, commit updated card-data.json + coverage-data.json

### Claude's Discretion
- Exact function signatures and module placement for try_split_compound and try_split_compound_subject
- Internal connector detection heuristics (distinguishing "and" between effects vs "and" in keyword lists)
- ChangeZone specific_position flag design for "put on top/bottom" opt-out
- Parser integration point for counter filter within parse_target() flow
- Test fixture design for compound-action sample cards

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Gap Analysis
- `docs/plan-floodpits-drowner.md` — Full gap analysis: 3 gaps with existing pattern references and complexity ratings

### Parser Architecture
- `docs/parser-instructions.md` — Oracle parser contribution guide: how to add new patterns, intercept points, subject stripping
- `crates/engine/src/parser/oracle_effect.rs` — Main effect parser: `parse_targeted_action_ast()` (compound target splitting), `try_split_pump_compound()` (existing compound pattern to follow)
- `crates/engine/src/parser/oracle_target.rs` — Target parsing: `parse_target()` pipeline where counter filter composes
- `crates/engine/src/parser/oracle.rs` — Main parser orchestration

### Effect System
- `crates/engine/src/game/effects/change_zone.rs` — Zone change handlers: where auto-shuffle and SelfRef guard are added
- `crates/engine/src/game/effects/shuffle.rs` — Existing shuffle effect
- `crates/engine/src/game/effects/mod.rs` — Effect handler registry

### Filter System
- `crates/engine/src/game/filter.rs` — FilterProp enum, filter_matches_object() runtime matching
- `crates/engine/src/types/ability.rs` — TargetFilter enum (line ~625), where ParentTarget variant is added

### Counter System
- `crates/engine/src/game/game_object.rs` — GameObject struct, counters HashMap
- `crates/engine/src/types/counters.rs` — CounterType enum

### Replacement Pipeline
- `crates/engine/src/game/replacement.rs` — Replacement pipeline for zone-change replacements (Rest in Peace, etc.)

### Skills
- `.claude/skills/extend-oracle-parser/SKILL.md` — Oracle parser extension guide
- `.claude/skills/add-engine-effect/SKILL.md` — Effect addition checklist

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `try_split_pump_compound()` in oracle_effect.rs — existing compound splitting pattern for "+N/+N and gains keyword" to follow
- `parse_targeted_action_ast()` in oracle_effect.rs — where compound effect remainder is currently discarded, integration point
- `parse_target()` in oracle_target.rs — existing target parsing pipeline where counter filter composes
- `ChangeZone` effect — existing zone-change handler, extend for owner_library and auto-shuffle
- `Shuffle` effect — existing shuffle handler, may be simplified if auto-shuffle handles common cases
- `filter_matches_object()` in filter.rs — existing FilterProp dispatch, extend with HasCounter arm
- `CounterType` enum — stun, shield, +1/+1, etc. already defined
- `GameScenario` test harness — infrastructure for integration test

### Established Patterns
- Sub_ability chaining — standard sequential effect composition used throughout engine
- `#[serde(tag = "type")]` discriminated unions — all new enum variants must follow
- `strip_prefix`/`split_once` Oracle text decomposition — idiomatic parsing approach
- `TargetFilter::SelfRef` — existing self-reference pattern in target system
- `specific_position` on ChangeZone — existing field for "put on top/bottom" (verify existence)

### Integration Points
- `oracle_effect.rs` — where try_split_compound is called after parse_targeted_action_ast
- `oracle_target.rs` — where parse_counter_filter integrates into parse_target pipeline
- `change_zone.rs` — where SelfRef guard and auto-shuffle are added
- `filter.rs` — where HasCounter runtime matching is added
- `types/ability.rs` — where ParentTarget variant is added to TargetFilter, HasCounter to FilterProp
- `crates/engine-wasm/` — where new types get tsify derives for TypeScript generation

</code_context>

<specifics>
## Specific Ideas

- try_split_compound follows the same call-site pattern as try_split_pump_compound — runs after the primary effect is parsed, operates on the leftover text
- try_split_compound_subject is a new pattern — splits "X and Y" subjects before verb-specific parsing. Verb-agnostic helper, callers chain effects
- Auto-shuffle on ChangeZone-to-Library is a universal CR 401.3 building block, not Floodpits Drowner-specific. Covers Terminus, Hallowed Burial, Condemn, etc.
- ParentTarget fills a gap in the target system — currently no way for a sub_ability to reference "the same thing my parent targeted"
- Coverage audit validates that these building blocks improve real parser coverage across the 32K card corpus

</specifics>

<deferred>
## Deferred Ideas

- HasCountersMin patterns ("with three or more +1/+1 counters") — HasCounter has the min field, parser support deferred until cards need it
- Compound subjects with 3+ objects ("exile X, Y, and Z") — binary split only for now
- "then" connector semantics (different from "and"?) — both produce sub_ability chains; semantic distinction deferred
- Compound subject for non-shuffle verbs (exile, return, bounce) — try_split_compound_subject is ready, verb-specific callers added when cards need them
- Counter filter with "no counters" / "without" negation — add when cards need it

</deferred>

---

*Phase: 32-add-compound-targeted-action-parsing-and-multi-object-shuffle-to-library-support-floodpits-drowner-class*
*Context gathered: 2026-03-17*
