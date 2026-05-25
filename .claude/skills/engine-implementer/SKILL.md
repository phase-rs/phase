---
name: engine-implementer
description: "Run the strict phase.rs engine implementation pipeline: create an idiomatic plan with engine-planner, review it independently until no gaps remain, implement surgically, run Tilt-first verification, review the implementation with review-impl until clean, and commit only the relevant files."
---

# Engine Implementer

Run the full plan -> implement -> review -> commit pipeline for phase.rs engine, parser, AI, or frontend GameAction work. The goal is the most idiomatic, maintainable, rules-correct solution that fits the repo architecture.

## Inputs

Accept either:

1. A task description: cards, CR rules, Oracle text patterns, affected subsystems, and expected behavior.
2. A pre-existing plan. Treat it as a draft unless it has already passed the full independent `$review-engine-plan` loop.

## Required Pipeline

Complete these phases in order. Do not skip review loops.

### 1. Plan With `engine-planner`

Use `engine-planner` to create the implementation plan. If an agent runner is unavailable, read `.claude/agents/engine-planner.md` and follow it exactly.

The plan must meet the strictest architectural standards:

- class-of-cards/mechanics design, not a one-card fix
- idiomatic Rust representation with typed enums instead of bool flags
- existing building-block reuse before new helpers
- exact logic placement by layer
- verified CR rules for rules behavior
- parser work designed with `nom` combinators from the start
- analogous feature trace through real files
- concrete file-level implementation steps and verification

### 2. Review The Plan Until Clean

Run `$review-engine-plan` against the full plan. Send every finding back into the plan and re-review the entire revised plan with fresh context. Repeat until a full review round returns no gaps.

Do not proceed to implementation while any plan-review gap remains. Stop only for a true human design decision, missing external access, or an environment blocker that makes review impossible.

### 3. Implement Surgically

Before editing, verify that the reviewed plan is still consistent with the current code. Re-read relevant files rather than trusting stale context.

Stop and return to planning when:

- required sections are missing or superficial
- current code contradicts the plan
- the work no longer fits existing architecture
- parser work would require ad hoc string dispatch
- CR behavior is uncertain and not verified

Then follow these implementation rules:

- Re-read a file immediately before modifying it.
- Preserve other agents' work. Never stash, reset, restore, or checkout unrelated changes.
- Use targeted edits for existing files. Avoid whole-file rewrites.
- Keep game logic in the engine, parser logic in parser modules, and frontend code as display/action dispatch only.
- Use `nom` combinators from the first parser line. Do not use `find()`, `split_once()`, `contains()`, `starts_with()`, or similar heuristics for Oracle dispatch.
- Verify every CR number against `docs/MagicCompRules.txt` before writing or changing CR annotations.
- Build the class of cards/mechanics, not a one-card special case.
- If implementation friction reveals a design flaw, stop and return the gap rather than hacking around it.

### 4. Verify

Always format directly:

```bash
cargo fmt --all
```

For Rust, engine, and parser work:

```bash
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data
else
  cargo clippy --all-targets -- -D warnings
  cargo test -p engine
  ./scripts/gen-card-data.sh
fi
```

For frontend work:

```bash
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 180 check-frontend
else
  (cd client && pnpm run type-check && pnpm lint)
fi
```

After a non-zero `tilt-wait.sh`, fetch details with:

```bash
tilt logs <resource> --tail 50 --since 2m
```

For parser work, always run the parser combinator gate and both parser audits:

```bash
./scripts/check-parser-combinators.sh
cargo coverage
cargo semantic-audit
```

## Parser Diff Gate

If any modified file is under `crates/engine/src/parser/`, inspect added lines for string dispatch:

```bash
git diff --name-only | grep 'crates/engine/src/parser/' | while read f; do
  git diff "$f" | grep '^+' | grep -v '^+++' | grep -vE '^\+\s*//' \
    | grep -E '\.(contains|starts_with|ends_with|find)\(' \
    | grep -v '#\[test\]' | grep -v '#\[cfg(test)\]'
done
```

Any output is a hard failure unless it is a test, comment, explicitly annotated non-dispatch structural use, or `oracle_util.rs` dual-string `TextPair` helper work.

### 5. Review Implementation Until Clean

Run `$review-impl` against the implementation diff after verification. Address every finding, then rerun the implementation review against the full revised diff. Repeat until a full review round returns no findings.

Prefer an independent reviewer or fresh context for this review when available. If no independent reviewer is available, apply `$review-impl` directly and record that limitation in the final report.

### 6. Commit

Commit only after:

- the plan-review loop is clean
- implementation verification passes or unrelated failures are clearly isolated
- the `$review-impl` loop is clean

Stage only relevant files. Do not sweep unrelated user or agent work into the commit:

```bash
git status --short
git diff --stat
git add <specific-files>
git commit -m "<type>: <short implementation summary>"
```

Do not push unless explicitly requested.

## Final Report

Return a structured report after the commit.

Include:

1. Plan-review rounds and final clean result.
2. What changed, grouped by subsystem and file.
3. Key architectural decisions.
4. Verification commands and results.
5. Implementation-review rounds and final clean result.
6. Commit hash and staged file list.
7. Coverage impact for parser changes.
8. Deviations from the plan.
9. Self-flagged risks and judgment calls.
10. Remaining items, if any, with reasons.
