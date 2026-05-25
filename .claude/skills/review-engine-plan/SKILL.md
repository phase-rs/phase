---
name: review-engine-plan
description: Review phase.rs engine, parser, AI, frontend, or rules implementation plans before code is written. Use when Codex needs an architectural gate for plans involving parser changes, engine mechanics, MTG Comprehensive Rules behavior, new variants, targeting, replacement effects, stack/casting flow, AI policy, or frontend GameAction workflows.
---

# Review Engine Plan

Review the plan as an architectural gate. Reject the plan if any required dimension is missing, superficial, or contradicted by code evidence.

## Required Checks

1. **Class vs card**
   - Identify how many cards or patterns the plan covers.
   - Reject one-card plans unless the card is only the validating consumer of a reusable building block.

2. **Building-block reuse**
   - Confirm the plan consulted relevant existing modules from the CLAUDE.md building-block table.
   - Reject duplicated logic already covered by `parser/oracle_nom/`, `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`, `game/ability_utils.rs`, `game/keywords.rs`, or nearby helpers.
   - Require justification for every new helper.

3. **Trace verification**
   - The plan must name an analogous existing feature and list the file path trace followed end to end.
   - Reject plans that did not trace an existing feature.

4. **Abstraction layer correctness**
   - Parser logic belongs in `parser/`.
   - Runtime rules belong in `game/` or `game/effects/`.
   - Types belong in `types/`.
   - Game logic must not leak into frontend or WASM bridge.
   - Display formatting must not leak into the engine.

5. **Idiomatic Rust**
   - Prefer typed enums such as `ControllerRef`, `Comparator`, and `Option<T>` over bool fields.
   - Prefer exhaustive matches over wildcard catch-alls when the type set is known.
   - Prefer existing `strip_prefix`/parser helpers over `format!()` plus matching.

6. **Nom compliance for parser plans**
   - If any parser file changes, the plan must specify exact `nom` combinators or existing parser functions for every detection, dispatch, or classification step.
   - Reject plans using `contains()`, `starts_with()`, `ends_with()`, `find()`, or heuristics for Oracle parsing.
   - The parser is the detector; try the real parser rather than duplicating detection logic.

7. **CR verification**
   - Every referenced CR number must be verified against `docs/MagicCompRules.txt`.
   - If CR comments are added or changed, the plan must say how they will be verified.

8. **Skill checklist adherence**
   - Identify applicable skills, such as `$add-engine-effect`, `$oracle-parser`, `$add-keyword`, `$add-trigger`, `$add-static-ability`, `$add-replacement-effect`, `$add-interactive-effect`, or `$casting-stack-conditions`.
   - Reject plans that omit required checklist steps from applicable skills.

## Review Loop

Return every gap to the planner. Require a revised full plan, then re-review the entire revised plan with fresh context. Repeat until a full round returns clean or the caller stops the process.

## Output

Lead with blockers and material gaps. For each issue, include evidence and the required revision. If the plan is clean, say that no blocking gaps were found and name any residual assumptions.
