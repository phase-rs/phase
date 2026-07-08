---
name: engine-planner
description: Produce an architecturally idiomatic implementation plan for a phase.rs parser or engine change. Design for the class of cards, not the single card. Use this when you need a plan that will survive `/review-engine-plan` without bandaids or workarounds.
---

# Engine Planner

Produce an implementation plan for the phase.rs engine. Design for the class, not the card. Never propose bandaids, workarounds, or shortcuts — everything lives in its rules-correct place.

This skill produces the plan only. The plan-review loop belongs to the caller — when invoked from `/engine-implementer`, the orchestrator owns the loop. When invoked standalone, run `/review-engine-plan` against the plan yourself and iterate until clean.

> **⚠️ `mtgish` is dormant — out of scope for ALL plans.** Never plan changes to `mtgish/`, `crates/mtgish-import/`, or `data/mtgish-*`. The import pipeline is not a live consumer of the engine or parser; new variants, parser patterns, and effects do NOT need to be mirrored there. If a task description references mtgish, surface the contradiction and stop — do not silently include mtgish in the plan.

## Input

A task description: parser enhancement/fix, or engine mechanic enhancement/fix. May reference cards, Oracle text patterns, CR rules, or coverage gaps.

## Process

Complete every step. Do not skip any.

### Step 0: Verify the premise — confirm the card's actual Oracle text

**Hard gate, before any other step.** If the task references a specific card's abilities, fetch that card's real, current Oracle text from an authoritative source (Scryfall API: `https://api.scryfall.com/cards/named?exact=<URL_ENCODED_NAME>`, or MTGJSON) and compare it verbatim against what the task description claims. Do not proceed on memory, on assumed similarity to other cards, or on a task brief's paraphrase of the card's abilities without this independent check.

A downloaded game-state's stored ability `description` field is a second, usually-reliable source, but it is not a substitute for checking Scryfall — a game state only reflects abilities the parser already produced (correctly or not), and can be silent about clauses that don't exist at all.

**Why this is a hard gate:** a wrong premise about what a card does invalidates every subsequent step even if plan review, implementation review, and CI all pass — those loops verify that a design is *executed correctly*, not that its starting premise is *real*. A fabricated ability can survive multiple rounds of architectural review because reviewers by default trust the task brief's description of what the card does; they are not designed to fact-check the card itself. If the plan or implementation review process turns up something that looks off (e.g. a clause with no analogous card, or a CR citation that doesn't fit any existing rule), re-verify the premise before re-deriving the design.

### Step 1: Identify applicable skills

Determine which skill(s) apply and read each that does:

| Skill | When it applies |
|-------|----------------|
| `/add-engine-effect` | New effects or stub completions |
| `/oracle-parser` | Parser-only changes (authoritative parser reference) |
| `/add-keyword` | Keyword abilities |
| `/add-trigger` | Triggered abilities |
| `/add-static-ability` | Static/continuous effects |
| `/add-replacement-effect` | Replacement effects |
| `/add-interactive-effect` | Effects requiring player choices (WaitingFor + GameAction continuations) |
| `/casting-stack-conditions` | Casting flow or stack changes |
| `/add-ai-feature-policy` | Deck-aware AI features — new `DeckFeatures` axis + `TacticalPolicy`/`MulliganPolicy` wiring |
| `/add-frontend-component` | React components for WaitingFor overlays, board elements, or any UI that dispatches `GameAction`s |
| `/add-card-data-pipeline` | Card export shape changes, synthesis functions, coverage-report changes |
| `/add-engine-variant` | Any new enum variant on engine types (mandatory gate) |
| `/card-test` | Any plan whose verification matrix includes a cast-pipeline runtime test (canonical GameScenario/GameRunner recipe + the six test foot-guns) |

Use the skill checklist(s) as the skeleton of the final plan. Every checklist step must appear.

### Step 2: Trace an analogous feature

Find the existing feature most similar to what you're implementing. Trace it end-to-end through every layer it touches: types → parser → resolver → effect handler → tests. Record each file path you followed. **Hard gate** — the plan must name the traced feature and list the full trace path.

### Step 3: Read every file you will touch

Before proposing changes, read every file you plan to modify. Understand existing patterns, abstractions, and conventions in each.

### Step 4: Answer architectural questions

The plan MUST include these sections with substantive, specific answers:

- **Pattern Coverage** — What class of cards/patterns does this cover? Estimate card count. If the answer is 1, stop and find the general pattern.
- **Building Blocks** — Which existing modules and helpers will you compose from? Reference specific functions by name from `parser/oracle_nom/`, `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`, `game/ability_utils.rs`, `game/keywords.rs`, etc. Justify any new helper.
- **Logic Placement** — Where does each piece of logic belong (parser vs game vs effects vs types)? Justify each choice.
- **Rust Idioms** — Most idiomatic representation. Typed enums not bools. Exhaustive match not wildcards. Existing type reuse over new types.
- **Nom Compliance** (mandatory if any file under `crates/engine/src/parser/` changes) — For every detection, dispatch, or classification step, specify the exact nom combinator or existing parser function. If the plan describes `contains()`/`starts_with()`/`find()` for parsing dispatch, **STOP and redesign**. The parser IS the detector — try `parse_static_line(text).is_some()` instead of `text.contains("gets ")`.
- **Extension vs Creation** — Does this extend an existing pattern or create a new one? Justify any new pattern.
- **Analogous Trace** — Name the traced feature and the full file path (e.g., "Traced `Scry` through `types/ability.rs` → `parser/oracle_effect/imperative.rs` → `game/effects/scry.rs` → `game/effects/mod.rs`").
- **Variant Discoverability** (if adding any enum variant) — Confirm `cargo engine-inventory` was consulted and run the `/add-engine-variant` checklist.
- **Verification Matrix** — For every behavioral claim, specify the changed seam/function, production entry point, runtime test to add or update, revert-failing assertion, sibling/negative cases, hostile fixtures, and coverage status impact. Cast-pipeline runtime tests must follow the `/card-test` recipe (GameScenario + `GameRunner::cast(..).resolve()` + `CastOutcome` deltas, verbatim Oracle text). Every planned negative assertion must name its paired positive reach-guard — a bare negative that an upstream short-circuit (e.g. `Effect::Unimplemented` early-return) can satisfy vacuously is not a test. Hostile fixtures are per-claim/per-seam, not a single global negative test: include the applicable negative sibling / adjacent grammar or enum variant, empty/decline/no-legal-choice path, multi-authority case (two permissions/sources/costs, source or controller change, owner vs controller, prior tracked-set producer, etc.), and the first production branch the fixture reaches (`is_empty`, `is_none`, enum match arm, variant guard). If a hostile row is unreachable, prove why from code. For parser changes, explicitly state whether any Oracle text is accepted while semantics remain deferred; if yes, plan how coverage remains red/honest via `Effect::unimplemented`, an equivalent strict-failure marker, or unchanged unsupported coverage.
- **Identity / Provenance Contract** — For any "this way", "that source", "chosen", "cast using", "from among them", selected target/mode, replacement predicate, duration-bound effect, or controller/owner-relative text, specify the source phrase/rules concept, selected authority type and id/value, binding time, live vs snapshotted/latched semantics, storage location, consuming function, invalidation/expiration behavior, and the multi-authority hostile fixture that proves the binding.

### Step 5: Write the plan

Step-by-step implementation plan using the skill checklist as your guide. For each step:

- Exact file path to modify
- Specific changes (executable without ambiguity)
- Any CR rules that apply, verified by grepping `docs/MagicCompRules.txt`

## Output

Return the finalized plan including every mandatory architectural section. The caller will run it through `/review-engine-plan` (and loop until clean).
