---
name: review-impl
description: Review an implementation in scope, such as an uncommitted diff, a just-finished agent change, a commit, or named files, for missing or wrong behavior in phase.rs. Use when Codex needs a findings-only architecture and correctness review across engine, parser, frontend, multiplayer, AI, deck, build, or release changes.
---

# Review Implementation

Review for gaps: things that are missing or wrong. Do not spend findings on style nits, CI-enforced formatting, or a diff recap.

## Workflow

1. Identify the changed surface from the diff, commit, or named files.
2. Classify the surface area: engine logic, parser, frontend/UI, multiplayer/transport, AI heuristics, deck/format/feeds, build/CI/release, or docs.
3. Apply only the relevant lenses below.
4. Report findings only. Silence means LGTM.

Skip checks CI already enforces:

- `scripts/check-parser-combinators.sh`
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
- `scripts/coverage-regression-check.sh --fail-on-engine`
- TypeScript `pnpm type-check` and `pnpm lint`

## Universal Lenses

- **Class vs single case:** Does the change cover a reusable class? Name at least three examples in that class. If there is only one, flag a special-case smell.
- **Sibling coverage:** If one site in a class changed, name siblings that needed the same treatment and verify they were handled or intentionally unaffected.
- **Test adequacy:** Ensure tests exercise the failure path and the building block, not only one card or a constructor shortcut that bypasses production wiring.
- **Edge cases:** Check empty inputs, multi-target/modal/repeat interactions, simultaneous events, eliminated players, `im::Vector::truncate(n)` bounds, and async races when relevant.
- **Idiomatic code:** Flag new bools that should be typed enums, wildcard match arms that should be exhaustive, verbatim Oracle strings in parser logic, `as any`, fresh `@ts-expect-error`, and unchecked casts.

## Surface-Specific Lenses

### Engine Logic

- Verify every new or moved `// CR <rule>` by checking `docs/MagicCompRules.txt`; the cited rule must actually describe the code.
- Check reuse of building blocks in `parser/oracle_nom/`, `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`, `game/ability_utils.rs`, `game/keywords.rs`, `game/zones.rs`, and `game/targeting.rs`.
- Keep game logic in the engine. If player-visible state was added, verify multiplayer filtering.
- For non-battlefield zones, player-scoped queries usually use `owner`, not `controller`.
- Zone changes should route through replacement-aware pipelines rather than direct moves when replacements can apply.

### Parser

- Reject verbatim full-string Oracle matches and ad hoc dispatch.
- Verify plural, possessive, opponent, non-X, another, and sibling phrase variants for new parser arms.
- Prefer composable `nom` axes over cartesian lists of full `tag()` strings.

### Frontend / UI

- The frontend renders engine-provided state; it must not infer game rules or hidden data.
- Check React effect dependencies, unmount cleanup, touch equivalents, mobile scroll containment, and empty/loading/error states.
- Type-check passing is not proof of feature correctness; say when browser verification was not performed.
- **i18n:** Flag frontend-authored user-facing text (titles, labels, buttons, tooltips, placeholders, log templates) hardcoded in JSX instead of routed through `t()`. Conversely, flag engine/card pass-through (card names, Oracle text, interpolated enum strings) that was wrongly wrapped in `t()` — it belongs to the content pipeline, not chrome. Boundary rule: a string gets `t()` iff the frontend authored it (`client/src/i18n/README.md`). Also flag hand-rolled pluralization (`count === 1 ? …`) that should use `key_one`/`key_other`, and any direct `i18n.changeLanguage` call (the preferences store owns language).

### Multiplayer / Transport

- Verify hidden-information filtering.
- Round-trip new fields across WASM, WebSocket, Tauri, and P2P adapters where applicable.
- Check reconnect and 3+ player behavior when touched.

### AI

- Classifiers must cover the full enum/category, including untargeted board wipes and non-target effects.
- Deadline-bail branches must score candidates consistently with the no-bail path.
- Cache keys must include all inputs that alter decisions.
- Combination generators should short-circuit infeasible cases before enumerating.

### Deck / Format / Feeds

- Format checks should use semantic identity, such as `Basic` supertype, not brittle name allowlists.
- Feed code must not overwrite cached state with empty or zero-deck responses.

## Output

Use this exact finding shape:

```text
**[HIGH/MED/LOW]** <short summary>. Evidence: <path:line>. Why it matters: <one sentence>. Suggested fix: <one line>.
```

Findings first. No praise, no diff recap.
