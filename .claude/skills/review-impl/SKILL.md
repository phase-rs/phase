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

Two gates lead every review; apply them before the rest.

1. **Correct seam / location:** Is the change at the architecturally correct location — the layer/module/function the codebase's design says owns this responsibility — or a symptom-patch at the wrong seam that merely makes a test pass? A wrong-location fix is technical debt even when it works: it ossifies a dead or duplicate path and leaves the real seam (and the rest of the card class) untouched. This is the highest-priority check; a wrong seam is disqualifying no matter how clean the code looks. Name the correct seam in the finding.
2. **Most idiomatic change at the seam:** Given the right seam, is this the implementation a principal engineer steeped in this repository would write — the established building block reused rather than re-implemented, an existing typed enum parameterized rather than a new `bool` or sibling variant, `nom` combinators composed rather than string dispatch? "Works and is in the right place" is not enough when a cleaner house idiom exists; a correct-but-unidiomatic change is a finding, not a style nit. Three structural checks make this concrete: a *reference* enum (`HandSize`, `LifeTotal`) must not carry a `Fixed`/constant payload that belongs one level up in an expression wrapper (mixed abstraction layers); a new sibling on an `X`/`OpponentX`/`TargetX` cluster should be one parameterized variant — but only when the axis stays within a single CR rule section (don't unify across sections, e.g. life CR 119 vs power/toughness CR 208/209); and before flagging or accepting any new enum variant, grep `data/engine-inventory.json` to confirm it doesn't already exist and to surface the cluster smell.

- **Class vs single case:** Does the change cover a reusable class? Name at least three examples in that class. If there is only one, flag a special-case smell.
- **Sibling coverage:** If one site in a class changed, name siblings that needed the same treatment and verify they were handled or intentionally unaffected.
- **Test adequacy:** Ensure tests exercise the failure path and the building block, not only one card or a constructor shortcut that bypasses production wiring.
- **Fixture path-divergence:** A test can drive the *real* production entry point and still miss the bug if its fixtures are shaped so simply that they take a *different internal branch* than production inputs. Technique: trace the fix's entry through its first input-shape dispatches — `is_none()`/`is_some()`, `is_empty()`/`len()`, variant `match`, and "has-X" guards (e.g. `if ability_def.is_none() { return fast_path() }`). For each such branch the fix can reach, map every test fixture to the arm it triggers, and flag any production-reachable arm with **no** fixture. Smell: every fixture is degenerate in the same way (no ability/effect, no targets, empty or single-element collection, default/`None` field), so only the trivial shortcut arm runs while the arm real data takes ships untested. Name the unexercised arm and the minimal fixture change that would reach it.
- **Edge cases:** Check empty inputs, multi-target/modal/repeat interactions, simultaneous events, eliminated players, `im::Vector::truncate(n)` bounds, and async races when relevant.
- **Idiomatic code:** Flag new bools that should be typed enums, wildcard match arms that should be exhaustive, verbatim Oracle strings in parser logic, hand-rolled `starts_with` + index slicing that should be `strip_prefix`/`strip_suffix`/`TextPair`, `as any`, fresh `@ts-expect-error`, and unchecked casts.

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
