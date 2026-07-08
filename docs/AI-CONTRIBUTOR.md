# Contribute a Card with Your LLM

**This document is the procedural script a large language model follows to implement or fix a single card end-to-end and open a pull request.** It is designed so a human contributor — developer or not — can "lend their LLM" an hour and produce real engine work with minimal supervision.

If you are a human reading this: skip to [Appendix B](#appendix-b--shareable-entry-prompts) for copy-paste prompts to hand to your LLM. You do not need to read the rest.

If you are the LLM executing this: read top to bottom and follow every step. Do not skip the quality floor. Do not ask the human for clarification at any point.

---

## 0. Quality Floor — self-confirm before touching code

Before Step 1, confirm the following. **Tool support** and **Autonomy** are hard requirements: without them, abort. **Model** is load-bearing — see §0.1 for tier routing; report your actual model accurately on a `Model:` line in the PR body. **Thinking level** is advisory.

- **Model (load-bearing):** §0.1 assigns your capability tier; the §0.1.2 pre-PR gates apply to every tier, and the tier line sets PR processing priority. Report your model on a single canonical line in the PR body (e.g. `Model: claude-opus-4-7`, `Model: claude-sonnet-4-6`, `Model: codex-5.4`). Do not editorialize this line — `/pr-contribution-handler` parses it (and the matching `Tier:` line in §0.1.4) to prioritize PRs. Claiming Frontier when you are Standard wastes maintainer time on a PR that will fail the §0.1.2 gates anyway.
- **Thinking (advisory):** High or higher. On Claude Code this is available for Opus; on Codex CLI pass `--reasoning high` or higher. Report on a `Thinking:` line in the PR body.
- **Tool support (required):** You can invoke skills, use `WebFetch`, run shell commands, and use an independent reviewer or fresh context when requested. Without these, you cannot run `$engine-implementer` and must abort.
- **Autonomy (required):** You will not pause for human input during the run. Every decision fork defaults to the architecturally idiomatic path as defined by `CLAUDE.md`, `AGENTS.md`, and the skills under `.claude/skills/`.

---

## 0.1. Capability tier and pre-PR gates

Skill references in this section use the `$skill` / `/skill` convention defined in §0.25 — the forward reference is intentional so tier routing precedes notation.

### 0.1.1. Tier table

| Tier     | Models | Procedure |
|----------|--------|-----------|
| Frontier | `claude-opus-4-7`+, `gpt-5-5`+, `codex-5-5`+ | Full pipeline per §4 onward. |
| Standard | `claude-sonnet-4-6`, `claude-haiku-4-5`, `gpt-5-3` through `gpt-5-4`, `codex-5-3` through `codex-5-4` | Full pipeline. |

If you cannot determine your model, assume Standard. Below Standard (no tools, no autonomy), abort per §0. The tier affects PR processing priority (§0.1.4), not which gates apply — **both §0.1.2 gates are universal**.

### 0.1.2. Pre-PR gates (all tiers)

Both gates run on your diff before you push and open a PR, regardless of tier — review data shows combinator violations and unanchored patterns come from every model class. Failure on either → stop, do not open the PR, trigger §0.1.3 honesty clause.

**Gate A — Combinator-purity script.** Run from the repo root:

```bash
./scripts/check-parser-combinators.sh
```

Paste the full output (including the success line or the violation list) into the PR body under a `## Gate A` heading. Non-zero exit = stop. The script catches the patterns Standard-tier models most frequently violate despite the `CLAUDE.md` nom mandate: `.contains("…")`, `.split_once`, `.starts_with("…")`, match-arm string literals, and chained `if let Ok = tag(...)` blocks. Do not skip this step. Do not edit the pasted output.

**Gate B — Pattern anchoring.** Before writing your change, identify ≥2 existing analogous implementations in the same module(s) you are about to edit. Cite them in the PR body under `## Anchored on` with `file:line` references and a one-line description of what pattern you are following:

```
## Anchored on
- crates/engine/src/parser/oracle_static.rs:412 — existing `alt()` extension for keyword granting
- crates/engine/src/parser/oracle_static.rs:687 — existing continuous-modification wiring
```

Your new code must visibly mirror these analogs — same combinator family, same naming convention, same module placement. `/pr-contribution-handler` audits these citations (paths must exist, cited code must use the same combinator family as the new code, cited module class must match the modified module class). Fabricated, broken, or unrelated citations signal the maintainer to apply elevated scrutiny and increase the inline cleanup cost — they slow your PR down rather than helping it across the finish line.

### 0.1.3. Honesty clause

When a gate fails or you cannot find compliant analogs to anchor on, do NOT open a partial/WIP PR and do NOT edit Gate A output to mask violations. Stop and report to the user with:

- The gate that failed and its raw output.
- The missing primitive or pattern (e.g. "no existing parser arm in `oracle_trigger.rs` handles this triggering-condition shape").
- File paths inspected + relevant CR section.
- Recommendation to re-run the task on a Frontier-tier model.

### 0.1.4. PR-body tier declaration

Every PR body must include a single canonical line on its own line:

```
Tier: Frontier
```

or

```
Tier: Standard
```

`/pr-contribution-handler` parses this to sort processing order (Frontier PRs first — higher base quality, faster to merge). Missing or malformed → treated as Standard. Do not editorialize.

---

## 0.25. Notation — skill invocation

Throughout this document, skills are written with a leading `$` (Codex convention), e.g. `$engine-implementer`, `$review-impl`, `$review-engine-plan`, `$engine-planner`. If you are running under Claude Code, substitute a leading `/` instead — `/engine-implementer`, `/review-impl`, etc. Both forms invoke the same skill file under `.claude/skills/<name>/SKILL.md`. Pick the form your runtime understands; do not mix them in a single command.

---

## 0.5. Out-of-scope paths — `mtgish` is dormant

`mtgish/`, `crates/mtgish-import/`, and `data/mtgish-*` are **dormant** — they are NOT live consumers of the engine, parser, or card data. The runtime pipeline is MTGJSON → `crates/engine/src/parser/` → `client/public/card-data.json`.

Do not modify any mtgish path. Do not mirror new engine variants, struct-variant fields, or parser changes into `mtgish-import` "for consistency." PRs that only touch mtgish files will be rejected on sight. If a tool, audit, or skill steers you toward mtgish, treat the reference as historical and stay in `crates/engine/`.

---

## 1. Pick your track

| Track | You (the human) have... | The LLM will... |
|-------|-------------------------|-----------------|
| **Developer** | Rust toolchain + pnpm installed | Run full local verification (`cargo fmt`, `clippy`, `test`, `gen-card-data`, `coverage`, `semantic-audit`) before opening the PR. |
| **Non-developer** | Nothing — just an LLM session | Skip local verification entirely; GitHub Actions will run CI on the PR. The maintainer finishes any remaining polish. |

Both tracks share steps 2–7. Only Step 5 (Verify) differs.

**Prefer Developer track even if the toolchain isn't installed yet.** If your runtime has shell access and can install software (e.g. `rustup`/`winget`/`apt`/`homebrew` are reachable), attempt to install the Rust toolchain and default to Developer track rather than silently falling back to Non-developer. If installation fails, requires unavailable permissions, or needs interactive input you cannot provide, use Non-developer track and say why. A session that opens a PR on the Non-developer track's word alone can push a change that fails CI on things a two-minute local `cargo test` would have caught — that costs a maintainer's review cycle and looks worse than the extra setup time. Only use Non-developer track when installing a toolchain is genuinely not possible in your environment (no shell/package-manager access at all), and say so explicitly rather than defaulting to it out of convenience.

---

## 2. Clone the repo

```bash
gh repo fork phase-rs/phase --clone --remote   # creates your fork and clones it
cd phase
```

If the contributor lacks `gh`, fall back to a plain `git clone` and tell them (in the final report) that they will need to push to their own fork manually. Do not stop.

---

## 2.1. Sync your fork with upstream — every run

Run this on **every** invocation, not just the first clone. A fork's `main` goes stale the moment upstream advances; working from a stale base produces spurious diffs against `origin/main`, risks textual merge-queue conflicts, and can revert other contributors' landed work. Always start from an up-to-date codebase.

```bash
git remote get-url upstream >/dev/null 2>&1 || git remote add upstream https://github.com/phase-rs/phase.git
git fetch upstream main
git checkout main && git merge --ff-only upstream/main   # fast-forward fork main to upstream
git push origin main                                     # keep the fork's main in sync (best-effort)
```

If `git merge --ff-only` fails, your fork's `main` has diverged from upstream — do **not** force it. Proceed to §4 regardless: that step cuts your working branch directly from `upstream/main`, so a diverged fork `main` never contaminates your change.

---

## 2.5. Bootstrap the repo (Developer track only)

Run **once per fresh clone** before invoking `$engine-implementer`. This downloads MTGJSON, generates `client/public/card-data.json`, fetches the local copy of the Comprehensive Rules, installs frontend deps, and configures git hooks:

```bash
./scripts/setup.sh --agent
```

The `--agent` flag skips the three Scryfall image sidecars (`scryfall-data.json`, `scryfall-token-images.json`, `scryfall-printings.json`). They are runtime-only image data for the React frontend in a browser — no Rust integration test, parser tool, `cargo coverage`, `cargo semantic-audit`, or vitest test depends on them. Skipping saves a ~500 MB Scryfall bulk download with zero impact on the signal §6 verification consumes.

**Required for §6 verification:**
- `client/public/card-data.json` — without this, integration tests in `crates/engine/tests/integration/*.rs` self-skip with `"skipping: client/public/card-data.json not generated"` and `cargo coverage` / `cargo semantic-audit` / `cargo parser-gaps` cannot read parsed AST shape for any card. Agents without this file have no signal beyond unit tests.
- `client/public/card-names.json`, `coverage-data.json`, `coverage-summary.json`, `card-data-meta.json`, `set-list.json`, `decks.json` — sidecars consumed by `cargo coverage` and the parser audit binaries.
- `docs/MagicCompRules.txt` — gitignored. Required for the CR-annotation rule (`grep -n "^701.21" docs/MagicCompRules.txt`); without it you cannot verify CR numbers and §0.1 honesty applies.
- `.git/config` git-hooks include — applies the repo's pre-commit hooks (including the `check-parser-combinators.sh` gate).

**Also produced, but not consumed by Developer-track §6:**
- `client/src/wasm/*` — WASM artifacts. Required by `pnpm run type-check` / vitest because TypeScript files import their generated `.d.ts`, but §6 doesn't run either. Safe to ignore unless your card touches frontend code.
- `client/node_modules/` — required by `pnpm` commands. Same caveat.

Agent mode also implies `--no-tilt` internally: even if `tilt` is on your PATH, setup.sh runs `gen-card-data.sh` and `build-wasm.sh` inline rather than deferring them to `tilt up`, so the required artifacts above are guaranteed present when the script exits.

Skip this section entirely on the Non-developer track — CI runs everything `--agent` mode produces.

---

## 3. Pick your work

Most runs start with the human pointing their LLM at the repo and expecting *you* to find the highest-value work — no card named. This section is the menu that makes that self-directed. The three tiers below (§3.1 → §3.3) are listed in **priority order**: work them top-down, dropping to a lower tier only when the ones above yield nothing you can complete cleanly. Every tier resolves to one card's change and flows through the same pipeline from §4 onward — only *how you find the target* and *what "done" looks like* differ between them.

**Override — the human named something.** If the human named a specific card, issue, or task, do that verbatim and skip the ladder. Normalize card-name casing for `client/public/card-data.json` lookups (typically lowercase). Otherwise, self-select down the ladder below.

Record your target card name up front — it appears in the branch name, commit message, and PR title regardless of which tier you picked from.

### 3.1 Fix a misparse

`docs/parser-misparse-backlog.md` is the canonical worklist and the first place to look. It catalogs cards the coverage system marks `supported: true` — no `Unimplemented` effects, so they *look* finished — but whose parsed AST is semantically **wrong**: a dropped intervening-if condition, a `for each` count collapsed to a fixed number, an anaphor bound to the wrong referent. These are the highest-harm gaps in the engine because they ship silently-wrong game behavior that nothing flags at runtime.

The backlog is clustered and ranked — about 30 **root-cause categories**, each naming the parser module that most likely owns the fix and the full list of cards that share that failure *shape*. Read these as categories, not one identical bug: the headline counts (753, 606, …) aggregate many sub-patterns, so a single combinator arm typically clears a *sub-cluster*, not the whole count. That is still the best ROI available — one fix unlocks a batch — but size your claim to what you actually change, and confirm it by regenerating card data and re-checking (that re-check is also your list-hygiene step below).

- **How to pick:** don't all start at row 1 — every contributor converging on the top category guarantees duplicate PRs. Pick a category from the ranked table **at random** (bias toward higher-ranked ones for ROI, but spread out), then pick a card from its list, preferring a category whose fix hint points at a parser module you can extend (typically an `alt()` arm or delegating to an existing combinator — consult the `oracle-parser` skill). **Before starting, run the §3.4 in-flight check for both the card and the category's mechanic** — someone may already have the class on a branch. Fix for the *class* the category names, never the single card.
- **How to know it's done:** the card is already `supported: true`, so `cargo coverage` won't move — the load-bearing signal is `cargo semantic-audit` reporting **zero findings** for the card after you regenerate card data, plus the card no longer parsing to the wrong shape.
- **List hygiene — required, in the same PR:** remove every card your change actually fixes from its root-cause list in `docs/parser-misparse-backlog.md`. A root-cause fix usually clears several cards at once — regenerate card data and re-check them to see which moved. If a root cause's card list becomes empty, delete that whole `### N.` section and update the ranked table plus the counts at the top of the file. The backlog is a live worklist for the next contributor; a fix that leaves stale entries misdirects the next run, so this cleanup is part of the deliverable, not optional.

When you invoke `$engine-implementer` in §4, frame the task as *correcting the parser so `<NAME>` and its root-cause class parse to the right shape* — a targeted fix at the root-cause seam, not a greenfield add.

### 3.2 Resolve an open GitHub issue

If the backlog has nothing you can land cleanly, take an open issue. Issues are human-curated, user-facing priorities — work a maintainer or player has already flagged as mattering — so they outrank the open-ended coverage tail.

```bash
gh issue list --repo phase-rs/phase --state open --limit 50 \
  --json number,title,labels,assignees
```

Pick an issue that (a) is unassigned, (b) has no open linked PR already resolving it, and (c) names a card or a concrete parser/engine behavior you can implement end-to-end. Skip open-ended design discussions and anything gated on deferred infrastructure. Run the §3.4 in-flight check before you start so you don't duplicate work already on someone's branch. If the issue names a card, that card is your target; put `Closes #<number>` in the PR body so the issue auto-closes on merge.

### 3.3 Fill a coverage gap

The open-ended long tail: cards with no support yet (`supported: false`). Always available, lowest coordination cost, and a clean greenfield add — the fallback when the two tiers above are exhausted. Fetch the coverage data from the published R2 endpoint (no local `cargo coverage` needed):

```bash
curl -sL https://data.phase-rs.dev/staging/coverage-data.json -o coverage-data.json   # ~60 MB — download, then jq
```

The payload is a **single object**, not a bare card list — do not iterate its top level. Two fields drive card selection:

- **`.cards`** — an array with one entry per card, each `{card_name, set_code, supported, gap_count, oracle_text, parse_details, printings}`. Pick a card where `supported == false` and `gap_count` is small (prefer 1–3 — the lowest-risk wins):

  ```bash
  jq -r '[.cards[] | select(.supported == false and .gap_count >= 1 and .gap_count <= 3)]
         | sort_by(.gap_count)[] | "\(.gap_count)  \(.card_name)"' coverage-data.json | head
  ```

- **`.top_gaps`** — the ROI ranking, and the best place to start. Each entry is a missing parser handler with `single_gap_cards` (how many cards become supported if you implement *just* that one handler), per-format unlock counts, and `oracle_patterns[].example_cards`. Pick a high-`single_gap_cards` handler, implement it **for the class**, then take one of its `example_cards` as your concrete target card. `.gap_bundles` pairs handlers by how many cards fixing them *together* unlocks. This is the coverage-tier analogue of the misparse categories — and the same caveat applies: `single_gap_cards` is the *ceiling* for fully implementing the handler (which spans many sub-patterns), so one combinator arm clears a slice, not the whole number. Let `cargo coverage` tell you which cards actually flipped.

Skip any card whose remaining gap is deferred infrastructure — oracle text referencing Rooms, Enchant Player, or Suspend Aggression (a judgment call from the Oracle text; there is no structured flag for it — see `memory/` notes in the repo if available, otherwise ignore). `cargo parser-gaps` and the `parser-velocity` skill compute the same `top_gaps` ranking locally. Here the "done" signal is `cargo coverage` flipping the card to `supported: true, gap_count: 0`.

### 3.4 Confirm the work isn't already in flight

Before implementing, confirm no open PR already covers the selected card **or its core mechanic**. Duplicate PRs for the same issue waste reviewer and CI effort and one will lose the merge-queue race (recurring: two PRs adding the same crew/saddle contribution static, two adding the same prevention-recipient scope). Scan by card name *and* by mechanic — the keyword you would add may already be in flight under a different card:

```bash
gh pr list --repo phase-rs/phase --state open --search "<Card Name>" --json number,title,headRefName
gh pr list --repo phase-rs/phase --state open --search "<keyword-or-mechanic>" --json number,title
```

If an open PR already implements the card or the core mechanic, **stop and report it to the human** rather than opening a duplicate — offer to review or extend the existing PR instead.

---

## 4. Implement with `$engine-implementer`

Base your branch on the **current upstream `main`**, not your fork's (possibly stale) `main`. A branch cut from a stale base shows spurious diffs against `origin/main`, risks textual merge-queue conflicts, and can revert other contributors' landed work. Fetch upstream first, then create the branch (with a collision guard so re-runs on the same fork don't fail):

```bash
git remote get-url upstream >/dev/null 2>&1 || git remote add upstream https://github.com/phase-rs/phase.git
git fetch upstream main
slug="card/<slug-of-card-name>"
n=2
while git rev-parse --verify "refs/heads/$slug" >/dev/null 2>&1 \
   || git ls-remote --exit-code origin "$slug" >/dev/null 2>&1; do
  slug="card/<slug-of-card-name>-$n"
  n=$((n + 1))
done
git checkout -b "$slug" upstream/main   # cut from current upstream, not a stale fork main
```

If your work spans more than a few minutes and upstream `main` advances, keep current with `git fetch upstream main && git merge --no-edit upstream/main` so the final diff contains only your change.

Then invoke the `$engine-implementer` skill with this prompt, substituting `<NAME>`:

> Implement full engine support for the card "<NAME>". Follow `CLAUDE.md` and `AGENTS.md` design principles without exception: build for the class not the card, nom combinators on first pass, CR annotations verified against `docs/MagicCompRules.txt` (and for each cited rule, also read its adjacent rules in the same section — cite the *authorizing* rule for the effect, not just the *layering* rule), idiomatic Rust, engine owns all logic, frontend is display-only. Reuse existing building blocks before writing new ones. Do not ask for clarification — on any ambiguity, take the architecturally idiomatic path. If scope expands beyond a single effect (e.g. the card requires new infrastructure, a new keyword, a new replacement pipeline), proceed anyway and explicitly note the scope expansion in your final report under a heading "Scope Expansion".

`$engine-implementer`'s published contract is: plan with `engine-planner` → review the plan with `$review-engine-plan` until clean → implement → verify → review the implementation with `$review-impl` until clean → commit. Validate that next.

**All tiers:** the §0.1.2 gates apply to whatever diff `$engine-implementer` produces. Run both Gate A and Gate B before §5; if either fails, do NOT continue to §7 — return to fix the violations, or stop per §0.1.3 if they cannot be fixed.

---

## 5. Validate the review actually happened and was addressed

> This is the most important step. `$engine-implementer` must actually run `$review-impl` and address findings before committing. The outside caller (you, the LLM reading this) must verify.

**A final `$review-impl` pass is mandatory before the PR opens.** Whatever produced the diff, the last action before pushing is a `$review-impl` review whose findings are addressed *with code* — an acknowledgement with no corresponding diff does not satisfy it. The two checks that lead that review are non-negotiable: (1) the change is at the architecturally correct seam, and (2) the change at that seam is the most idiomatic one the codebase allows. The three checks below confirm that pass happened and was acted on.

Apply **all three** checks:

1. **Review section exists with concrete findings.** The final report must contain an explicit `$review-impl` section enumerating findings with file:line references, or a clear clean-review result that states an implementation review ran against the full diff.
2. **Findings were addressed with code.** For every finding classified as a defect, gap, or missing case, there must be a corresponding change in `git diff HEAD~ HEAD` (or the working tree if not yet committed). An acknowledgement without a diff is a failure.
3. **Clean-review cross-check (fresh context).** If the report claims zero findings, run an independent pass when your environment supports it; otherwise note the limitation in the PR body. Hand the reviewer ONLY the unified diff (`git diff HEAD~ HEAD`), `CLAUDE.md`, and the relevant skills under `.claude/skills/`. No prior conversation. The reviewer must explicitly check: (a) **correct seam / location** — is the change at the layer/module/function the design says owns this responsibility, or a symptom-patch at the wrong seam that merely makes a test pass? A wrong-location fix is debt even when green; flag it as disqualifying and name the correct seam; (b) **most idiomatic change at the seam** — given the right location, is this the implementation a principal engineer steeped in this repo would write (established building-block reuse over re-implementation, combinator composition over string dispatch, enum parameterization over a new bool/sibling)? A correct-but-unidiomatic change is a finding, not a nit; (c) **nom-mandate compliance** — flag any `match` over a stringified parser-text variable with string-literal arms, any chained `if let Ok(..) = tag(..)` blocks, and any string-method dispatch (`.contains("…")`, `.find("…")`, `.rfind("…")`, `.split(`, `.split_once`, `.splitn`, etc. — `.rfind`/`.split` are not caught by `check-parser-combinators.sh`, so grep the diff for them by hand); (d) **CR-citation completeness** — for each cited rule, did the implementation also cite the *authorizing* rule, not just the *layering* rule? (e) **pattern coverage** — does this work for ≥10 cards or just one? (f) **logic placement** — engine vs frontend per `CLAUDE.md`; (g) **building-block reuse** — did the implementation duplicate logic an existing helper already handles? Re-implementing what `oracle_util.rs`, `oracle_quantity.rs`, `game/filter.rs`, `game/zones.rs`, etc. already provide is a defect even if the new code works; (h) **bool-flag avoidance** — any new `bool` field/parameter where a typed enum (`ControllerRef`, `Comparator`, `Option<T>`, etc.) would express the design space better is a defect; (i) **test discrimination** — does at least one test drive the real pipeline (`apply()` / scenario runner / cast harness) and FAIL if the fix were reverted? A test that only asserts parsed AST shape — an `assert_eq!` on a parsed `Effect` / `StaticMode` / `AbilityDefinition` without resolving it — is a shape test, not a regression test, and is the single most common gap on keyword and parser PRs; name it as a defect and require a discriminating runtime test before the PR opens. Negative assertions (`!detector(...)`, "does not parse to X") are vacuous unless the same test carries a positive reach-guard proving the input got past upstream short-circuits (e.g. `check_swallowed_clauses` early-returns on `Effect::Unimplemented`) — flag any bare negative as a defect; If the cross-check produces findings, feed them back into `$engine-implementer` and loop.

**If any check fails:** rerun `$engine-implementer` or continue the same skill workflow with explicit instructions to execute `$review-impl` and address every finding with code changes. Do **not** proceed to Step 6 until validation passes. Retry at most 2 times; on a third failure, abort the run and record the gap in the PR body under a "Validation Failures" heading so the maintainer can triage.

---

## 6. Verify (track-specific)

**Developer track** — run in this order. On any failure, fix in-loop (max 2 retries) before proceeding. If still failing after retries, record the failure in the PR body under "CI Failures" and continue to Step 7 — do not abort.

Step 2.5 (`./scripts/setup.sh --agent`) is a prerequisite for this section — `cargo coverage` and `cargo semantic-audit` both read `client/public/card-data.json`, and the integration suite self-skips without it.

If Tilt is running locally (`tilt get uiresource clippy >/dev/null 2>&1` succeeds), prefer `tilt-wait.sh` for clippy/tests/card-data — it reuses Tilt's already-warm rebuild loop instead of fighting it for the cargo target lock. See CLAUDE.md § "Canonical verification pattern".

```bash
cargo fmt --all                               # always direct — Tilt doesn't auto-format
./scripts/check-parser-combinators.sh         # nom-mandate gate (one-shot — direct in both modes)

if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data
else
  cargo clippy-strict
  cargo test -p engine
  ./scripts/gen-card-data.sh
fi

# One-shot audit binaries (always direct — not Tilt resources):
cargo coverage                                # every track: card is supported: true, gap_count: 0 — and no other card regressed
cargo semantic-audit                          # every track: zero new findings for the card (a §3.1 fix also removes it from parser-misparse-backlog.md)
```

**Non-developer track** — skip this step entirely. GitHub Actions runs the same checks on the PR.

---

## 7. Open the pull request

**Scope gate — run before committing.** The branch must contain ONLY this card's change:

```bash
git status --short                       # nothing unrelated in the working tree
git diff --stat upstream/main...HEAD     # every listed file belongs to THIS card's change
```

If unrelated files appear (another card's test, a stray `mod` line, editor/tool artifacts, regenerated data beyond your card's delta), remove them from the branch before committing — bundled unrelated changes are one of the most common review rejections and will force a rebase/split.

Claude Code: invoke the `commit-push-pr` skill. Codex / other: run the equivalent shell sequence:

```bash
git add -A
git commit -m "Add <Card Name>"
git push -u origin HEAD
gh pr create --title "<title>" --body "<body>"   # no --label arg; upstream auto-labeler handles it
```

**PR title:** `Add <Card Name>` for a coverage-gap add (the default case), including runs that grew in scope — a clean `Add` may legitimately ship new infrastructure as a building block, and size alone is not a signal of incompleteness. For a **misparse fix (§3.1)** or an **issue (§3.2)**, use `Fix <Card Name>` instead of `Add` (and for an issue, add `Closes #<number>` to the body). Use `Partial: <Card Name>` only if Step 5 logged validation failures or Step 6 logged CI failures the run could not resolve.

**PR body template:**

```markdown
## Summary
Adds engine support for **<Card Name>**.
<!-- Misparse fix (§3.1): "Fixes the <root cause> misparse for <Card Name>" and note the backlog entries removed. Issue (§3.2): add "Closes #<number>". -->

## Files changed
<brief bulleted list — paths only, no prose>

## CR references
<list of `CR XXX.Y` annotations added or touched>

## Track
<Developer | Non-developer>

## LLM
Model: <claude-opus-4-7 | claude-sonnet-4-6 | codex-5.4 | …>
Thinking: <high | max>

## Verification
<Developer track — checklist confirming each Step 6 command ran clean:
  - `cargo fmt --all` — clean
  - `cargo clippy-strict` — clean
  - `./scripts/check-parser-combinators.sh` — clean
  - `cargo test -p engine` — N pass / 0 fail
  - `./scripts/gen-card-data.sh` — `<card>`: 0 Unimplemented entries
  - `cargo coverage` — `<card>`: `supported: true`, `gap_count: 0`
  - `cargo semantic-audit` — `<card>`: 0 findings
Non-developer track — write: "Local verification skipped — see CI status checks.">

## Scope Expansion
None.
<!-- If engine-implementer reported scope growth, replace `None.` above with a brief description of the new infrastructure added. The literal text `None.` (case-insensitive, optional period) is the only spelling the labeler treats as absent — any other content here triggers `needs-maintainer`. -->

## Validation Failures
None.
<!-- If Step 5 could not be made to pass after retries, replace `None.` above with the failure details. -->

## CI Failures
None.
<!-- If Step 6 surfaced a failure the LLM could not resolve, replace `None.` above with the failure details. -->
```

**Labels:** The `auto-label-ai-contribution` workflow may apply `needs-maintainer` on PR open/edit/synchronize. Fork PRs cannot self-label, so do not pass `--label` to `gh pr create`.

- `needs-maintainer` — added when **any** of:
  - `Track:` is `Non-developer`, or
  - the `## Scope Expansion`, `## Validation Failures`, or `## CI Failures` heading has content other than the literal default `None.`, or
  - the `## Verification` heading is missing/empty on a Developer-track PR.

---

## 8. Report and exit

Print the PR URL. Print a one-line status: `success`, `partial`, or `aborted`. Exit cleanly. Do not linger for further input.

---

## Appendix A — Skill equivalents

Use skills when the runtime supports them:

- **Invoking `$engine-implementer`:** load `.claude/skills/engine-implementer/SKILL.md` and follow its full plan → review → implement → verify → review → commit pipeline.
- **Invoking `$review-impl`:** load `.claude/skills/review-impl/SKILL.md` and execute its checklist against the uncommitted diff or commit diff.
- **Invoking `commit-push-pr`:** run the raw `git` + `gh` sequence shown in Step 7.

Every other step (quality floor, track selection, clone, card pick, validation, verify, report) is tool-agnostic.

---

## Appendix B — Shareable entry prompts

Paste one of these into your LLM. That is the entire interaction.

### B.1 — Developer track, URL-only (shortest)

```
Read https://raw.githubusercontent.com/phase-rs/phase/main/docs/AI-CONTRIBUTOR.md
and follow the Developer track end-to-end to implement or fix the card
{CARD_NAME, or say "pick one" and let the LLM self-select via the §3 priority
ladder: misparse fix → open issue → coverage gap}. Use high thinking. Do not stop for
my input. Apply the §0.1 tier routing — BOTH §0.1.2
gates must pass before opening the PR (all tiers). Open a PR when done.
```

### B.2 — Non-developer track, URL-only

```
Read https://raw.githubusercontent.com/phase-rs/phase/main/docs/AI-CONTRIBUTOR.md
and follow the Non-developer track end-to-end to implement or fix the card
{CARD_NAME, or say "pick one" and let the LLM self-select via the §3 priority
ladder: misparse fix → open issue → coverage gap}. Skip local verification — GitHub Actions will run CI on the
PR. Use high thinking. Do not stop for my input. Apply the §0.1 tier routing
— BOTH §0.1.2 gates must pass before opening the PR (all tiers).
Open a PR when done.
```

### B.3 — Non-developer track, fully self-contained (for UIs without web fetch)

```
You are going to implement one Magic: The Gathering card in the phase-rs/phase
repository end-to-end and open a pull request. Do not pause to ask me anything.

Requirements: Best results with Claude Opus 4.7+ or Codex 5.4+ at high+
thinking, but proceed even if your runtime is below that — just report your
actual model on a single canonical "Model:" line in the PR body (e.g.
"Model: claude-sonnet-4-6"). Do NOT editorialize that line. Hard requirements:
you can invoke skills, run shell commands, and you will not pause for input.

Steps:
1. gh repo fork phase-rs/phase --clone --remote && cd phase
1a. Sync your fork with upstream EVERY run (never work from a stale base):
   git remote add upstream https://github.com/phase-rs/phase.git 2>/dev/null;
   git fetch upstream main; git checkout main &&
   git merge --ff-only upstream/main && git push origin main
2. If I named a card or issue, use it. Otherwise self-select via the §3
   priority ladder in docs/AI-CONTRIBUTOR.md, top-down: (1) fix a misparse from
   docs/parser-misparse-backlog.md — and remove the fixed card(s) from that list
   in your PR; (2) resolve an open issue (gh issue list --repo phase-rs/phase
   --state open) and add "Closes #<number>" to the body; (3) fall back to a
   coverage gap — fetch https://data.phase-rs.dev/staging/coverage-data.json
   (a single object; cards live under .cards[]) and pick a .cards[] entry with
   supported==false and small gap_count, or a high-single_gap_cards handler
   from .top_gaps and one of its example_cards.
3. git checkout -b card/<slug> upstream/main  (cut the branch from CURRENT
   upstream/main, not stale fork main; if the branch already exists locally or
   on origin, append "-2", "-3", etc. — see Step 4 in docs/AI-CONTRIBUTOR.md).
4. Invoke the $engine-implementer skill to implement the card. Tell it: follow
   CLAUDE.md and AGENTS.md without exception, plan with engine-planner, review
   the plan with $review-engine-plan until clean, use nom combinators on first
   pass, verify CR annotations against docs/MagicCompRules.txt (and cite the
   authorizing rule, not just the layering rule), do not ask for clarification,
   take the idiomatic path, proceed even if scope expands, review the
   implementation with $review-impl until clean, then commit.
5. Validate $engine-implementer actually ran $review-impl AND addressed every
   finding with code changes. If not, send it a follow-up to do so (max 2
   retries). If the review claims zero findings, use an independent reviewer
   or fresh context when available and hand it only the diff + CLAUDE.md.
6. Skip local verification (I don't have a Rust toolchain).
7. git push to my fork and open a PR with title "Add <Card Name>" (use
   "Fix <Card Name>" for a misparse fix (§3.1) or an issue (§3.2), and
   add "Closes #<number>" to the body for an issue; "Partial: <Card Name>"
   only if validation or CI failures were unresolved).
   Body must follow the template in docs/AI-CONTRIBUTOR.md. Do NOT pass
   --label flags — the upstream auto-labeler may apply needs-maintainer
   automatically based on the branch name and body content.
8. Print the PR URL and exit.

Tier gates (ALL tiers): BEFORE pushing the PR you MUST: (a) run
./scripts/check-parser-combinators.sh and paste the full output under a
`## Gate A` heading in the PR body, (b) include a `## Anchored on` section
with at least 2 file:line citations to existing analogous implementations in
the same module(s) you edited, (c) include a `Tier:` line — `Tier: Frontier`
for claude-opus-4-7+/gpt-5-5+/codex-5-5+, `Tier: Standard` otherwise (the
tier affects processing priority only; the gates apply to everyone). If
either gate fails, do NOT open the PR — stop and report the failed gate
output to the user with a recommendation to re-run on a Frontier-tier model.

Card / issue: {CARD_NAME, issue number, or "pick one"}
```
