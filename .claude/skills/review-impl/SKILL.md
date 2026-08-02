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
4. If the scope is a PR, fetch whatever external review comments exist (CodeRabbit, human reviewers) and confirm-or-refute each against the current head with code evidence, folding confirmed findings into your own. **Assume none exist by default** — Gemini Code Assist has been sunset, so no bot is guaranteed to have pre-screened this PR. Your own lenses are the complete review, not a supplement to a bot's; do not under-invest expecting a backstop. Where an external finding *does* exist, silently omitting it — or returning a verdict less severe than an open, unrefuted finding from another reviewer — is itself a defect. Review comments, checks, and uploaded/sticky artifacts count only when their evidence identifies the current PR head SHA; otherwise report the evidence as missing rather than attributing it to the current diff.
5. If the scope is a PR touching engine/parser source, the parse-diff sticky comment (marker `<!-- coverage-parse-diff -->`) is required evidence: fetch its full body and confront the card-level diff against the PR's claimed scope. Unexplained gained/lost/changed cards are findings (unintended parser blast radius). A *Baseline pending* body means the diff is unavailable — flag it so the handler brings the branch current to regenerate it; an absent comment despite changed engine source, or a comment/artifact not bound to the current PR head SHA, means CI evidence is missing for the current head. This PR-head requirement does not apply to Engine-Implementer Checkpoint Mode: before a push, validate its canonical receipt and, when forced, its direct projection/comparator artifacts against `BASE_SHA..CANDIDATE_SHA` instead.
6. Report findings only. Silence means LGTM.

When `pr-contribution-handler` explicitly requests the manual quality gate, add `Quality Gate: PASS|FAIL` before findings. PASS requires all three current-PR facts: (1) claimed parse-impact count equals the measured parse-diff count and the normalized card sets are identical, using the full artifact when the sticky comment truncates examples; (2) the change is at an existing authority/right seam and reuses its vocabulary; and (3) a production-pipeline test is demonstrated to fail when the production change is reverted. On PASS, return the applicable existing praise tokens (`right-seam`, `scope-discipline`, `discriminating-runtime-test`, `parameterized-not-proliferated`) for the ordinary review/enqueue event. Never infer quality from Tier or standing and never create a `quality_recommended` event.

Skip checks CI already enforces:

- `scripts/check-parser-combinators.sh`
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
- `scripts/coverage-regression-check.sh --fail-on-engine`
- TypeScript `pnpm type-check` and `pnpm lint`

## Engine-Implementer Checkpoint Mode

Default review output is findings-only. Exception: when `/engine-implementer` invokes this skill against a checkpointed candidate, it supplies `BASE_SHA`, `CANDIDATE_SHA`, the committed reproducible `BASE_SHA..CANDIDATE_SHA` diff, frozen scope paths, the named `START_SHA`/`IMPLEMENTATION_WORKTREE` clean-start/stable-HEAD attestations, the detached `COMPLETION_WORKTREE` start/end identity attestations, the canonical receipt, completion evidence, and the existing maintainer-simulation matrix. Validate the receipt before applying universal lenses. Confirm the first round has `START_SHA == BASE_SHA`, each fix starts from the prior reviewed candidate, the implementation worktree was clean at start and stable at checkpoint, the completion worktree was detached, clean, and exactly at `CANDIDATE_SHA` before and after completion, and the receipt's candidate/head values are exactly `CANDIDATE_SHA`.

The receipt must be the un-hashed UTF-8/LF `engine-implementer-receipt-v1` document with one final LF, no CR/NUL, percent-encoded values (only `[A-Za-z0-9._~-]` raw), the fixed fields in their mandated order, the mandatory `source_hash_record.base.*` and `.candidate.*` groups, and complete ascending indexed groups. Verify the frozen scope representation is duplicate-free NUL-delimited bytes from `printf '%s\0' "${SCOPE_PATHS[@]}" | LC_ALL=C sort -z`, and verify its recorded path and SHA256. Verify `scoped_diff_command` is exactly `git -C "$IMPLEMENTATION_WORKTREE" -c color.ui=false -c diff.noprefix=false -c core.quotepath=true -c diff.orderFile=/dev/null -c diff.interHunkContext=0 -c diff.suppressBlankEmpty=false diff --no-color --no-ext-diff --no-textconv --no-renames --diff-algorithm=myers --no-indent-heuristic --full-index --binary --src-prefix=a/ --dst-prefix=b/ --unified=3 "$BASE_SHA" "$CANDIDATE_SHA" -- "${SCOPE_PATHS[@]}"`, then reproduce and hash those scoped-diff bytes. Verify `projection_authority_diff_command` is exactly `git -C "$IMPLEMENTATION_WORKTREE" diff --name-only -z "$BASE_SHA" "$CANDIDATE_SHA" -- Cargo.toml .cargo/config.toml rust-toolchain.toml scripts/engine-source-hash.sh`, reproduce and hash its NUL-delimited output, and require its recorded path/size/SHA256. Each source-hash group must contain, in order, `command`, `expected_sha`, `head_before`, `detached_before`, `clean_before`, `head_after`, `detached_after`, `clean_after`, `exit`, `stdout_path`, `stdout_sha256`, `stderr_path`, `stderr_sha256`, `artifact_path`, `artifact_size`, and `artifact_sha256`; it must bind the exact `scripts/engine-source-hash.sh` command and output artifact to its expected SHA. Verify every source-hash, projection, and completion group contains ordered `detached_before` and `detached_after` fields, each proving `git symbolic-ref -q HEAD` exited exactly `1` with empty stdout and stderr; a measurement-group failure is `CANNOT_ANSWER`, and a completion-group failure is a gate failure. Verify every recorded artifact path, size, and SHA256, but do not expect a hash for the receipt itself. Its fixed identity fields, scoped-diff command/path/size/digest, source hashes, authority-diff command/path/size/digest, canonical `projection_forced_reason`, mandatory source-hash records, parser evidence, completion records, artifacts, and declared absent inputs are mandatory; the projection section is omitted only when source hashes are equal, the authority-diff output is empty, and `projection_forced_reason=NONE`. A missing, duplicate, unordered, malformed, or digest-mismatched field is a blocking finding.

In this mode, emit these lines before findings:

```text
Review Head: <CANDIDATE_SHA>
Receipt SHA256: <SHA256 of the validated receipt bytes>
Semantic-Impact Gate: PASS|FAIL
Completion Gate: PASS|FAIL
Maintainer-Simulation Gate: PASS|FAIL
```

After the headers and findings, emit exactly one JSON object on its own line: `{"clean":<boolean>,"findings":[<string>,...]}`. Set `clean=true` only when all three gates pass and there are no findings; otherwise set it to `false` and include every failed gate and blocking finding in `findings`.

`Review Head` must be the supplied `CANDIDATE_SHA`, the reviewed diff must reproduce from exactly `BASE_SHA..CANDIDATE_SHA`, and `Receipt SHA256` must be the SHA256 of the validated receipt bytes emitted externally in this review result; otherwise report a blocking finding. The receipt never contains or hashes itself. `Semantic-Impact Gate` passes only when the receipt validates and either (a) equal SHA-bound source hashes, an empty authority-diff output, and `projection_forced_reason=NONE` set `NO_PARSE_AFFECTING_CHANGE` with no projection section, or (b) a non-`NONE` canonical forced reason sets `PROJECTED_PARSE_DIFF` and contains all direct-projection records.

`Semantic-Impact Gate` rejects missing/mismatched source-hash, authority-diff, or projection evidence, including either mandatory source-hash record, an extra projection, missing receipt evidence, or `CANNOT_ANSWER`. It passes only when equal source hashes retain complete `source_hash_record.base` / `.candidate` groups, the exact authority-diff artifact is empty, and `projection_forced_reason=NONE` has `NO_PARSE_AFFECTING_CHANGE` with no projection section, or when a source-hash difference and/or authority-diff path has the matching canonical non-`NONE` reason and exactly one base and one candidate direct projection with the complete evidence below.

For any non-`NONE` forced-reason receipt, require the pinned `AtomicCards.json` artifact, exactly one detached clean base projection and one detached clean candidate projection, an isolated `cargo build --profile tool --features cli --bin oracle-gen --bin coverage-report --bin coverage-parse-diff` record for each side, and one base-built comparator record that contains both `--base-sha` and `--head-sha`. Every build/projection/comparison record must include command, environment, worktree, target, expected SHA, ordered `head_before`, `detached_before`, `head_after`, `detached_after`, `clean_before`, `clean_after`, exit, stdout/stderr paths and SHA256s, and produced artifacts; each detached field must prove `git symbolic-ref -q HEAD` exited exactly `1` with empty stdout and stderr, and any other result is `CANNOT_ANSWER`. A source-hash difference or excluded-authority-input change is valid; any replica, quorum, manifest, seal, ledger, provenance-envelope, stale identity, omitted indexed field, noncanonical receipt encoding/order, missing/mismatched required source-hash, authority-diff, or projection record, extra projection, a projection when `projection_forced_reason=NONE`, or missing/mismatched artifact is a blocking finding.

`Completion Gate` passes only when the receipt's completion-check groups enumerate every scope-/plan-derived mandatory check and every applicable surface-derived check from the executor's existing implementation/fix verification blocks: formatting for implementation changes; the Rust/engine/parser Tilt-first-or-isolated-direct block for Rust/engine/parser paths; the frontend Tilt-first-or-isolated-direct block for frontend paths; and the parser preparatory gate for parser paths. Validate the set against the candidate diff; do not accept a generic plan-only list or preparatory executor success in place of candidate-SHA-bound records. Each entry must give its exact command, successful exit result, detached `CANDIDATE_SHA` identity and clean status at start, and detached `CANDIDATE_SHA` identity and clean status at end, including ordered `detached_before` and `detached_after` proof that `git symbolic-ref -q HEAD` exited exactly `1` with empty stdout and stderr; any other result is an operational failure. Every completion/parser command must either use `git -C "$COMPLETION_WORKTREE"` or execute in that worktree. For parser paths, validate the candidate range was enumerated NUL-safely with `git -C "$COMPLETION_WORKTREE" diff --name-only -z "$BASE_SHA" "$CANDIDATE_SHA" -- crates/engine/src/parser/` and `IFS= read -r -d ''`. Missing or unsuccessful mandatory checks fail the gate. For Markdown-only policy work, scope-path, SHA-identity, and Markdown/diff checks are the complete mandatory set, and Cargo/Tilt are not required. The reviewer validates the receipt before gate evaluation and final acceptance validates the receipt and its artifacts after review; neither action is a self-referential `completion_check` row or receipt artifact. `Maintainer-Simulation Gate` passes only if every changed seam has a concrete row covering production entry, first production branch reached, selected authority / bound value when applicable, binding time, live vs snapshotted semantics, storage, consuming function, invalidation behavior, hostile fixtures, and serialized-surface impact. Use `FAIL` when any row is missing, superficial, or contradicted by the diff, and report the specific gap as a normal finding. Outside this scoped mode, keep silence-as-LGTM behavior.

## Universal Lenses

Two gates lead every review; apply them before the rest.

1. **Correct seam / location:** Is the change at the architecturally correct location — the layer/module/function the codebase's design says owns this responsibility — or a symptom-patch at the wrong seam that merely makes a test pass? A wrong-location fix is technical debt even when it works: it ossifies a dead or duplicate path and leaves the real seam (and the rest of the card class) untouched. This is the highest-priority check; a wrong seam is disqualifying no matter how clean the code looks. Name the correct seam in the finding.
2. **Most idiomatic change at the seam:** Given the right seam, is this the implementation a principal engineer steeped in this repository would write — the established building block reused rather than re-implemented, an existing typed enum parameterized rather than a new `bool` or sibling variant, `nom` combinators composed rather than string dispatch? "Works and is in the right place" is not enough when a cleaner house idiom exists; a correct-but-unidiomatic change is a finding, not a style nit. Three structural checks make this concrete: a *reference* enum (`HandSize`, `LifeTotal`) must not carry a `Fixed`/constant payload that belongs one level up in an expression wrapper (mixed abstraction layers); a new sibling on an `X`/`OpponentX`/`TargetX` cluster should be one parameterized variant — but only when the axis stays within a single CR rule section (don't unify across sections, e.g. life CR 119 vs power/toughness CR 208/209); and before flagging or accepting any new enum variant, grep `data/engine-inventory.json` (gitignored — run `cargo engine-inventory` to (re)generate it locally first) to confirm it doesn't already exist and to surface the cluster smell.

- **Class vs single case:** Does the change cover a reusable class? Name at least three examples in that class. If there is only one, flag a special-case smell.
- **Sibling coverage:** If one site in a class changed, name siblings that needed the same treatment and verify they were handled or intentionally unaffected.
- **Test adequacy:** Ensure tests exercise the failure path and the building block, not only one card or a constructor shortcut that bypasses production wiring.
- **Vacuous negative assertions:** For every negative assertion (`!detector(...)`, "does not parse to X", "counter NOT applied"), verify the input actually reached the code under test. An upstream short-circuit makes the negative pass for the wrong reason — canonical instance: `check_swallowed_clauses` returns early when any parsed ability contains `Effect::Unimplemented`, so `!has_swallowed_clause(...)` passes on a card that failed to parse at all. Require a paired positive reach-guard in the same test (parse succeeded, zero `Effect::Unimplemented`, expected positive shape) or a runtime assertion that flips on revert. This is the highest-frequency contributor-PR finding — check it on every test the diff adds.
- **New-field threading:** When the diff adds a field to an existing enum variant or struct, grep every construction and consumption site of that variant across the workspace. Flag any site that silently defaults or drops the field — resume/continuation paths, single-pick vs multi-pick branches, batch handlers, and WASM/adapter/serialization payload constructors are the recurring drop points. Each site must thread the field or carry an explicit reason why the default is correct there.
- **Claim-to-test map:** For every behavior the PR claims to support, identify the changed seam/function, production entry point, and test that reaches it. Flag any behavior tested only through a helper, constructor, parsed AST, or direct resolver call when production reaches it through `apply()`, `WaitingFor`, `GameAction`, stack/casting, combat declaration, replacement handling, or the scenario runner. Parser shape tests do not satisfy runtime semantics or coverage-support claims; they are acceptable for parser-only work only when unsupported semantics remain red/honest.
- **Fixture path-divergence:** A test can drive the *real* production entry point and still miss the bug if its fixtures are shaped so simply that they take a *different internal branch* than production inputs. Technique: trace the fix's entry through its first input-shape dispatches — `is_none()`/`is_some()`, `is_empty()`/`len()`, variant `match`, and "has-X" guards (e.g. `if ability_def.is_none() { return fast_path() }`). For each such branch the fix can reach, map every test fixture to the arm it triggers, and flag any production-reachable arm with **no** fixture. Smell: every fixture is degenerate in the same way (no ability/effect, no targets, empty or single-element collection, default/`None` field), so only the trivial shortcut arm runs while the arm real data takes ships untested. Name the unexercised arm and the minimal fixture change that would reach it.
- **Coverage honesty:** If parser code accepts full Oracle text while intentionally ignoring any semantic rider, replacement, delayed trigger, restriction, granted ability, continuation, or other rules-bearing clause, coverage must remain honest. Flag changes that make `cargo coverage` mark a card/class supported while those semantics are deferred or dropped instead of preserved as `Effect::unimplemented`, an equivalent strict-failure marker, or unchanged unsupported coverage.
- **Selected authority / provenance:** For "this way", "that source", "chosen", "cast using", "from among them", selected modes/targets, replacement predicates, duration-bound effects, or controller/owner-relative text, verify the selected authority is carried from the production choice point to the consuming function. Flag global rescans that can pick a different permission, source, cost, replacement, tracked set, controller, or owner unless a multi-authority fixture proves equivalence.
- **Snapshot / latch semantics:** Flag live predicates where the CR requires a value or duration to be fixed when a spell/ability resolves, a replacement applies, or an effect's duration ends. Require tests for post-resolution value changes, controller changes, zone changes, and non-revival after duration expiry when relevant.
- **Empty / decline / natural-event paths:** For optional choices and tracked sets, verify all-decline or empty selections publish/clear the same state a non-empty choice would. For resource/event counters, verify natural game progression is not counted as a created extra resource.
- **Serialized-surface contracts:** If the diff changes an enum, `GameAction`, `WaitingFor`, game-state field, card-data export shape, or serialized AI/community scenario shape, verify existing repo-owned serialized data still loads via migration/defaults/fixture updates and protocol-visible changes bump the wire contract when required.
- **Target/scope matrix:** For combat, targeting, controller, owner, protector, defender, or player-scope changes, enumerate the variants/scopes reachable at the touched production boundary. Require negative tests for semantically adjacent sibling variants that are plausibly affected, or a concrete explanation for why a sibling is unreachable/out of scope.
- **Edge cases:** Check empty inputs, multi-target/modal/repeat interactions, simultaneous events, eliminated players, `im::Vector::truncate(n)` bounds, and async races when relevant.
- **Idiomatic code:** Flag new bools that should be typed enums, wildcard match arms that should be exhaustive, verbatim Oracle strings in parser logic, hand-rolled `starts_with` + index slicing that should be `strip_prefix`/`strip_suffix`/`TextPair`, `as any`, fresh `@ts-expect-error`, and unchecked casts.

## Surface-Specific Lenses

### Engine Logic

- Verify every new or moved `// CR <rule>` by checking `docs/MagicCompRules.txt`; the cited rule must actually describe the code.
- Compound CR annotations are the project's documented convention, **not** format violations: `CR X + CR Y` for interacting rules, `CR X / CR Y` for alternatives, and range/subpart forms like `CR 702.45a/b` (see CLAUDE.md "MTG Comprehensive Rules Annotations"). Do not flag the `+` / `/` / range forms as malformed or as regex violations — external reviewers routinely raise these and they should be refuted, not echoed. Only flag a CR citation whose base number does not resolve in `docs/MagicCompRules.txt`, or one that does not describe the annotated code.
- Check reuse of building blocks in `parser/oracle_nom/`, `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`, `game/ability_utils.rs`, `game/keywords.rs`, `game/zones.rs`, and `game/targeting.rs`.
- Keep game logic in the engine. If player-visible state was added, verify multiplayer filtering.
- For non-battlefield zones, player-scoped queries usually use `owner`, not `controller`.
- Zone changes should route through replacement-aware pipelines rather than direct moves when replacements can apply.

### Parser

- Reject verbatim full-string Oracle matches and ad hoc dispatch.
- Verify plural, possessive, opponent, non-X, another, and sibling phrase variants for new parser arms — including article/quantity word-forms (`a`/`an`/`one`/`two`/`N`/`X`/`each`) when dispatch splits on a count or article boundary.
- When one parser arm rejects a value to defer to another (e.g. a multi-count arm returns `None` for count==1 so the single-count arm handles it), trace the receiving arm and prove it actually accepts that form — do not trust the comment. A deferral to a path that doesn't handle the form silently drops the card to Unimplemented (precedent: "roll one d6" — the multi-die arm rejected count==1 and the single-die arm only matched `"a "`/`"an "`, so the word "one" parsed nowhere despite a comment claiming otherwise).
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

Severity calibration: a latent bug — one not reachable today because a guard or parser blocks it — is still a finding. Rate it by what happens when the form is reached or the guard removed, not by today's reachability: if it then produces a wrong result for a card-class the feature appears to cover, it is at least MED, and "unreachable today" belongs in the evidence, not as a reason to downgrade to LOW. A feature that ships silently incorrect for a sub-class it looks like it handles (e.g. a multi-die roll that overwrites the stored result each iteration instead of supporting the aggregate) is a MED the maintainer must see — never a NIT.

Findings first. No praise, no diff recap.

Exception: in Engine-Implementer Checkpoint Mode, the `Review Head`, `Semantic-Impact Gate`, `Completion Gate`, and `Maintainer-Simulation Gate` lines precede findings.
