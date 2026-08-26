# Contribute a Card with Your LLM

**This document is the procedural script a large language model follows to implement or fix a single card end-to-end and open a pull request.** It is designed so a human contributor — developer or not — can "lend their LLM" an hour and produce real engine work with minimal supervision.

If you are a human reading this: skip to [Appendix B](#appendix-b--shareable-entry-prompts) for copy-paste prompts to hand to your LLM. You do not need to read the rest.

If you are the LLM executing this: read top to bottom and follow every step. Do not skip the quality floor. Do not ask the human for clarification at any point.

---

## 0. Quality Floor — self-confirm before touching code

Before Step 1, confirm the following. **Tool support** and **Autonomy** are hard requirements: without them, abort. **Model** is load-bearing — see §0.1 for tier routing; report your actual model accurately on a `Model:` line in the PR body. **Thinking level** is advisory.

- **Model (load-bearing, and now a hard gate):** §0.1 requires a **Frontier-tier** model; if you are not one, abort. Report your model on a single canonical line in the PR body (e.g. `Model: claude-opus-4-8`, `Model: gpt-5-5`, `Model: codex-5-5`). If your harness never exposes an exact identifier, §0.1.1 gives you a hedged form to declare instead — that case is not an abort. Do not editorialize this line — `/pr-contribution-handler` parses it (and the matching `Tier:` line in §0.1.4). **Report it accurately.** Misreporting your model to clear the gate is the one thing here that will get the account itself blocked rather than the PR closed: your commit trailers and output are legible to us, so a false `Model:` line is caught, and it converts an out-of-policy PR into a trust problem.
- **Thinking (advisory):** High or higher. On Claude Code this is available for Opus; on Codex CLI pass `--reasoning high` or higher. Report on a `Thinking:` line in the PR body.
- **Tool support (required):** You can invoke skills, use `WebFetch`, run shell commands, and use an independent reviewer or fresh context when requested. Without these, you cannot run `$engine-implementer` and must abort.
- **Autonomy (required):** You will not pause for human input during the run. Every decision fork defaults to the architecturally idiomatic path as defined by `CLAUDE.md`, `AGENTS.md`, and the skills under `.claude/skills/`.

---

## 0.1. Capability tier and pre-PR gates

Skill references in this section use the `$skill` / `/skill` convention defined in §0.25 — the forward reference is intentional so tier routing precedes notation.

### 0.1.1. Tier table

| Tier     | Models | Procedure |
|----------|--------|-----------|
| Frontier | **Anthropic:** `claude-opus-4-8`+ (including `claude-opus-5`+), `claude-sonnet-5`+ · **OpenAI:** `gpt-5-5`+ (including the `gpt-5.6` family) · **Cursor/Codex:** `codex-5-5`+ | Full pipeline per §4 onward. |

**Frontier-tier models only.** There is no longer a Standard tier. The floor is per-vendor and is stated by exact model, not by family wildcard — `claude-sonnet-5` is accepted while `claude-sonnet-4-6` is not, so a `claude-sonnet-*` reading of this table is wrong (but a *newer* version than the one named does qualify — see the reading rule below). **Not accepted:** `claude-opus-4-7` and below, `claude-sonnet-4-6` and below, every `claude-haiku-*` including `claude-haiku-4-5`, every `composer-*`, `gpt-5-4` and below including `gpt-5-3`, and `codex-5-4` and below. If that is your model, abort per §0 rather than opening a PR. A PR declaring a non-Frontier model, or whose commits show one, will be closed as out-of-policy without an implementation review. This is not a judgement about those models generally; it reflects that review capacity here is the scarce resource, and sub-Frontier runs have consistently consumed several maintainer rounds per PR to reach a standard a Frontier run reaches on the first pass.

**How commit evidence is read.** The gate is about the model that *wrote the change*, so a `Co-Authored-By:` trailer is read against the commits that carry the implementation. A session that starts on a Frontier model and falls back to a sub-Frontier one part-way through — a usage limit, a harness default — leaves sub-Frontier trailers on later commits without the PR having been generated below the floor. That is a fixable declaration problem, not dishonesty: expect to be asked to confirm which model did the work. What earns a close is the whole run sitting below the floor; what escalates to an account-level problem is a `Model:`/`Tier:` line that contradicts the trailers in a direction that clears the gate.

A trailer that names a *harness* rather than a model — `Copilot`, `Cursor`, and similar — neither corroborates nor contradicts the declaration, because those harnesses never expose the underlying model to the trailer. Its silence is not evidence of misreporting and is not grounds for a close; ask which model did the work if it matters. Only a trailer naming an actual sub-floor model can contradict a declaration.

**Reading the table — `+` means that version or anything later.** `claude-sonnet-5`+ admits `claude-sonnet-5` and every later version in that same family from that vendor, so a model that postdates the last edit of this table qualifies without being enumerated. That includes `claude-opus-5` under the `claude-opus-4-8`+ floor. Compare versions after normalizing separators — `gpt-5.6` and `gpt-5-6` are the same version — and treat a vendor variant suffix (`-sol`, `-thinking`, `-preview`, a date stamp) as still inside its family: the `gpt-5.6` family, including `gpt-5.6-sol`, sits above the `gpt-5-5` floor and qualifies. The "not a family wildcard" rule points *downward* only: it forbids reading `claude-sonnet-*` as admitting `claude-sonnet-4-6`, which sits below the floor. Do not abort merely because your exact identifier is not printed above — normalize it and check it against the floor for your family instead.

**If your harness does not expose an exact model identifier.** The gate is on capability, not on your ability to emit a canonical id string. Several harnesses — GitHub Copilot, IDE assistants, and hosted chat UIs with a model picker — never hand the running model its own identifier. That is not a disqualification. Route as follows:

- You can establish vendor, family, and enough version detail to place yourself at or above a floor in the table above → **proceed**, and declare the canonical id if you have one.
- You know which model the harness selected (its picker name, the name in your system context, or the name your user stated) but not a canonical id → **proceed**, and declare it in exactly that form on a single line: `Model: <name as your harness reports it> (via <harness>; canonical id not exposed)`, e.g. `Model: gpt-5.6-sol (via GitHub Copilot; canonical id not exposed)`. `Tier: Frontier` still applies.
- You cannot establish vendor, family, and version at all, or the version you can establish sits below the floor → **abort** per §0.

Do not guess upward. Report the name your harness reports; the hedged form above clears the gate on its own, so inventing a canonical id you were never given buys nothing and lands in the §0 misreporting case. Tier cannot satisfy the artifact gate or authorize architecture scope.

**Applies to PRs opened on or after 2026-07-24.** Pull requests opened before that date are judged on their code, not their declared tier — the Standard tier was accepted policy when they were written, and a contributor who reported a Sonnet, Haiku, or Composer run accurately was following the rules as published. `claude-haiku-4-5` in particular was named in the Standard row of the tier table until 2026-07-24T02:03:45Z, so Haiku trailers on a PR opened before that are evidence of compliance, not of a violation. Do not close an older PR for a declaration that was correct when it was made. This grandfathering covers the declaration only: every other gate in this document applies to open PRs regardless of age.

**Agentic harnesses (Cursor, and similar).** Using an agentic harness is allowed, but a `Co-authored-by: Cursor <cursoragent@cursor.com>` trailer (or equivalent) on your commits **raises the review bar rather than lowering it**, and the underlying model must still be Frontier tier. The pattern that earns an immediate close is pushing changes you have not verified and using repo CI and maintainer review as your correction loop — reverting a fix to push a diagnostic commit so CI prints a value for you, or deleting an assertion to turn a job green, are both treated as that pattern. Run the gates locally, understand the change, then push.

### 0.1.2. Pre-PR gates

Both gates run on your diff before you push and open a PR — review data shows combinator violations and unanchored patterns come from every model class, Frontier included. Failure on either → stop, do not open the PR, trigger §0.1.3 honesty clause.

**Gate A — Combinator-purity script.** Run from the repo root after the final read-only review in §5:

```bash
./scripts/check-parser-combinators.sh
```

Run this only after the final local commit and final read-only review exist. Paste the full output under `## Gate A`; success is exactly `Gate A PASS head=<40-hex-sha> base=<40-hex-sha>`, and `head` must equal the PR's current head. Non-zero exit, missing output, or a later commit means stop and rerun both the final review and Gate A. Do not edit the output.

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

`Frontier` is the only accepted value — see §0.1.1. `/pr-contribution-handler` reads this line, and it is never evidence of quality or authority on its own. A missing, malformed, or non-`Frontier` line means the PR is closed as out-of-policy without an implementation review, subject to the 2026-07-24 cutoff in §0.1.1. Do not editorialize.

---

## 0.25. Notation — skill invocation

Throughout this document, skills are written with a leading `$` (Codex convention), e.g. `$engine-implementer`, `$review-impl`, `$review-engine-plan`, `$engine-planner`. If you are running under Claude Code, substitute a leading `/` instead — `/engine-implementer`, `/review-impl`, etc. Both forms invoke the same skill file under `.claude/skills/<name>/SKILL.md`. Pick the form your runtime understands; do not mix them in a single command.

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

> Implement full engine support for the card "<NAME>". Follow `CLAUDE.md` and `AGENTS.md` design principles without exception: build for the class not the card, nom combinators on first pass, CR annotations verified against `docs/MagicCompRules.txt` (and for each cited rule, also read its adjacent rules in the same section — cite the *authorizing* rule for the effect, not just the *layering* rule), idiomatic Rust, engine owns all logic, frontend is display-only. Reuse existing building blocks before writing new ones. Do not ask for clarification — on ordinary implementation ambiguity, take the architecturally idiomatic path. If the card requires protected architecture scope, stop without opening a PR unless a maintainer explicitly appointed you to that work beforehand or the PR closes an issue labeled `accepted`.

`$engine-implementer`'s published contract is: plan with `engine-planner` → review the plan with `$review-engine-plan` until clean → implement → verify → review the implementation with `$review-impl` until clean → commit. Validate the committed result next.

**All tiers:** Gate B and its anchors must exist before implementation. After `$engine-implementer` completes and commits, run the final read-only review in §5 against that committed head, then run Gate A. This is one post-commit loop: if the review finds anything or any later change creates a commit, address it and rerun both the final review and Gate A against the new head. If either gate fails, do NOT continue to §7 — return to fix the violations, or stop per §0.1.3 if they cannot be fixed.

---

## 5. Validate the review actually happened and was addressed

> This is the most important step. `$engine-implementer` must actually run `$review-impl` and address findings before committing. The outside caller (you, the LLM reading this) must verify.

**A final read-only `$review-impl` pass is mandatory against the committed head before Gate A and before the PR opens.** Address findings with code, amend or add the final commit, and rerun until the reviewer reports clean. Then run Gate A against that same committed head. Record the exact line `Final review-impl PASS head=<40-hex-sha>` under `## Final review-impl`; that SHA must equal the PR's current head. Acknowledgement without a diff, a dirty-tree review, or a later push does not satisfy the gate. Any later commit invalidates both records and requires rerunning the final review followed by Gate A.

Apply **all three** checks:

1. **Review section exists with concrete findings.** The final report must contain an explicit `$review-impl` section enumerating findings with file:line references, or a clear clean-review result that states an implementation review ran against the full diff.
2. **Findings were addressed with code.** For every finding classified as a defect, gap, or missing case, there must be a corresponding change in `git diff HEAD~ HEAD` (or the working tree if not yet committed). An acknowledgement without a diff is a failure.
3. **Clean-review cross-check (fresh context).** If the report claims zero findings, run an independent pass when your environment supports it; otherwise note the limitation in the PR body. Hand the reviewer ONLY the unified diff (`git diff HEAD~ HEAD`), `CLAUDE.md`, and the relevant skills under `.claude/skills/`. No prior conversation. The reviewer must explicitly check: (a) **correct seam / location** — is the change at the layer/module/function the design says owns this responsibility, or a symptom-patch at the wrong seam that merely makes a test pass? A wrong-location fix is debt even when green; flag it as disqualifying and name the correct seam; (b) **most idiomatic change at the seam** — given the right location, is this the implementation a principal engineer steeped in this repo would write (established building-block reuse over re-implementation, combinator composition over string dispatch, enum parameterization over a new bool/sibling)? A correct-but-unidiomatic change is a finding, not a nit; (c) **nom-mandate compliance** — flag any `match` over a stringified parser-text variable with string-literal arms, any chained `if let Ok(..) = tag(..)` blocks, and any string-method dispatch (`.contains("…")`, `.find("…")`, `.rfind("…")`, `.split(`, `.split_once`, `.splitn`, etc. — `.rfind`/`.split` are not caught by `check-parser-combinators.sh`, so grep the diff for them by hand); (d) **CR-citation completeness** — for each cited rule, did the implementation also cite the *authorizing* rule, not just the *layering* rule? (e) **pattern coverage** — does this work for ≥10 cards or just one? (f) **logic placement** — engine vs frontend per `CLAUDE.md`; (g) **building-block reuse** — did the implementation duplicate logic an existing helper already handles? Re-implementing what `oracle_util.rs`, `oracle_quantity.rs`, `game/filter.rs`, `game/zones.rs`, etc. already provide is a defect even if the new code works; (h) **bool-flag avoidance** — any new `bool` field/parameter where a typed enum (`ControllerRef`, `Comparator`, `Option<T>`, etc.) would express the design space better is a defect; (i) **test discrimination** — does at least one test drive the real pipeline (`apply()` / scenario runner / cast harness) and FAIL if the fix were reverted? A test that only asserts parsed AST shape — an `assert_eq!` on a parsed `Effect` / `StaticMode` / `AbilityDefinition` without resolving it — is a shape test, not a regression test, and is the single most common gap on keyword and parser PRs; name it as a defect and require a discriminating runtime test before the PR opens. Negative assertions (`!detector(...)`, "does not parse to X") are vacuous unless the same test carries a positive reach-guard proving the input got past upstream short-circuits (e.g. `check_swallowed_clauses` early-returns on `Effect::Unimplemented`) — flag any bare negative as a defect; If the cross-check produces findings, feed them back into `$engine-implementer` and loop.

**If any check fails:** rerun `$engine-implementer` or continue the same skill workflow with explicit instructions to execute `$review-impl` and address every finding with code changes. Do **not** proceed to Step 6 until validation passes. Retry at most 2 times; on a third failure, abort the run and record the gap in the PR body under a "Validation Failures" heading so the maintainer can triage.

---

## 6. Record verification and run Gate A (track-specific)

**Developer track** — the implementation workflow must run the mechanical checks below before its final commit. On any failure, fix in-loop (max 2 retries) before committing. If still failing after retries, record the failure in the PR body under "CI Failures" and continue to Step 7 — do not abort. After §5's clean read-only review, run only the Gate A command shown after the mechanical checks; if it finds a problem, change and commit the fix, rerun §5, and then rerun Gate A.

Step 2.5 (`./scripts/setup.sh --agent`) is a prerequisite for this section — `cargo coverage` and `cargo semantic-audit` both read `client/public/card-data.json`, and the integration suite self-skips without it.

If Tilt is running locally (`tilt get uiresource clippy >/dev/null 2>&1` succeeds), prefer `tilt-wait.sh` for clippy/tests/card-data — it reuses Tilt's already-warm rebuild loop instead of fighting it for the cargo target lock. See CLAUDE.md § "Canonical verification pattern".

```bash
cargo fmt --all                               # always direct — Tilt doesn't auto-format

if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data
else
  cargo clippy-strict
  cargo test -p phase-engine
  ./scripts/gen-card-data.sh
fi

# One-shot audit binaries (always direct — not Tilt resources):
cargo coverage                                # every track: card is supported: true, gap_count: 0 — and no other card regressed
cargo semantic-audit                          # every track: zero new findings for the card (a §3.1 fix also removes it from parser-misparse-backlog.md)
```

After the final commit and §5 review, run Gate A against that exact head:

```bash
./scripts/check-parser-combinators.sh
```

**Non-developer track** — GitHub Actions owns the mechanical checks, but Gate A is still local and mandatory after §5 against the committed head.

---

## 7. Open the pull request

**Scope gate — run after the final-review/Gate-A loop and before pushing.** The committed branch must contain only the selected card/class change or protected architecture work covered by an explicit prior maintainer appointment or a linked issue labeled `accepted`:

```bash
git status --short                       # clean; nothing remains after the reviewed commit
git diff --stat upstream/main...HEAD     # every listed file belongs to THIS card's change
```

If unrelated files appear, remove them. If any intended change remains uncommitted, commit it and rerun §5 followed by Gate A. If protected architecture scope is required and neither authorization exists, stop and report it. Do not open a partial PR, relabel it, or use Tier/quality claims to bypass the gate. Private appointment identities are maintainer state and never belong in the PR or repository policy.

Claude Code: invoke the `commit-push-pr` skill. Codex / other: run the equivalent shell sequence:

```bash
# $engine-implementer already committed the implementation. Confirm §5's final
# review and Gate A both name this exact HEAD; do not create another commit here.
git rev-parse HEAD
git push -u origin HEAD
gh pr create --title "<title>" --body "<body>"   # no --label arg; upstream auto-labeler handles it
```

**PR title:** `Add <Card Name>` for a coverage-gap add. For a misparse fix or issue, use `Fix <Card Name>` and add `Closes #<number>` for the issue. Never use a card title to conceal architecture expansion; unauthorized expansion stops before PR creation.

**PR body template:**

Start with the repository's .github/PULL_REQUEST_TEMPLATE.md. The GitHub UI
prepopulates it; when using gh pr create --body, copy that file's completed
contents into the body. Fill every section rather than writing an ad-hoc body
or omitting a placeholder. The repository template is the source of truth for
fields measured by the review loop, artifact audit, and triage workflow.

```markdown
## Summary
Adds engine support for **<Card Name>**.
<!-- Misparse fix (§3.1): "Fixes the <root cause> misparse for <Card Name>" and note the backlog entries removed. Issue (§3.2): add "Closes #<number>". -->

## Files changed
<brief bulleted list — paths only, no prose>

## CR references
<list of `CR XXX.Y` annotations added or touched>

## Implementation method (required)
Method: /engine-implementer
<!-- Or: Method: not-applicable — <specific non-engine reason> -->

## Track
<Developer | Non-developer>

## LLM
Model: <claude-opus-4-8 | gpt-5-5 | codex-5-5 | …>   # Frontier tier only — see §0.1.1
<!-- No exact id from your harness? Use the §0.1.1 hedged form instead:
     Model: gpt-5.6-sol (via GitHub Copilot; canonical id not exposed) -->
Tier: Frontier
Thinking: <high | max>

## Verification
- [ ] Required checks ran clean, or the exact CI-owned alternative is stated below.
- [ ] Gate A output below is for the current committed head.
- [ ] Final review-impl below is clean for the current committed head.
- [ ] Both anchors cite existing analogous code at the same seam.

- `<exact command or CI check>` — <exact result>

<commands and exact results; every fixed required box must appear exactly once and be checked>

## Gate A
Gate A PASS head=<40-hex-sha> base=<40-hex-sha>

## Anchored on
- path/to/existing.rs:123 — analogous authority/pattern
- path/to/existing.rs:456 — second analogous authority/pattern

## Final review-impl
Final review-impl PASS head=<40-hex-sha>

## Claimed parse impact
None.
<!-- List exact card names only when the parse-diff changes cards. -->

## Scope Expansion
None.
<!-- Describe any change that crosses the issue/card's stated scope. -->

## Validation Failures
None.
<!-- Replace with the unresolved validation failure and its evidence. -->

## CI Failures
None.
<!-- Replace with the unresolved CI failure and its evidence. -->
```

**Labels classify behavior; they do not grant authority.** The upstream workflows and maintainer apply type labels (`bug`, `enhancement`, `feature`, `test`, or `refactor`), `needs-maintainer` for operational attention, and the existing `quality` label only after manual evidence review. Fork PRs must not pass `--label` to `gh pr create`. No label waives artifact or architecture scope gates.

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
Use the repository's .github/PULL_REQUEST_TEMPLATE.md for the PR body and fill
every section; do not write an ad-hoc body.
If the work requires protected architecture scope without a prior maintainer
appointment or a linked issue labeled `accepted`, stop instead of opening a PR.
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
Use the repository's .github/PULL_REQUEST_TEMPLATE.md for the PR body and fill
every section; do not write an ad-hoc body.
If the work requires protected architecture scope without a prior maintainer
appointment or a linked issue labeled `accepted`, stop instead of opening a PR.
```

### B.3 — Non-developer track, fully self-contained (for UIs without web fetch)

```
You are going to implement one Magic: The Gathering card in the phase-rs/phase
repository end-to-end and open a pull request. Do not pause to ask me anything.

Requirements: Frontier-tier model REQUIRED — Claude Opus 4.8+ (including Claude
Opus 5+), Claude Sonnet 5+, GPT-5.5+ (including the GPT-5.6 family), or Codex
5.5+ at high+ thinking. "+" means that version or anything later, so a newer
model than the ones named here qualifies; compare versions ignoring separator
style ("5.6" == "5-6"), and a variant suffix like "-sol" or "-thinking" stays
in its family (gpt-5.6-sol qualifies). If your runtime is
below that floor (Claude Sonnet 4.6 or older, any Haiku, any Composer, GPT-5.4
or older, Codex 5.4 or older), STOP and tell me rather than opening a PR; it
will be closed as out-of-policy. Report your actual model on a single canonical
"Model:" line and the exact "Tier: Frontier" line in the PR body (e.g.
"Model: claude-opus-4-8").
If your harness never exposes an exact model identifier (GitHub Copilot, IDE
assistants, and picker-based chat UIs typically do not), do NOT abort for that
reason alone — declare the name your harness reports, in the form
"Model: gpt-5.6-sol (via GitHub Copilot; canonical id not exposed)", and
keep "Tier: Frontier". Abort only if you cannot establish your vendor, family,
and version at all, or you know you are below the floor. Do NOT editorialize
either line or overstate the model. Hard requirements: you can
invoke skills, run shell commands, and you will not pause for input.

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
   take the idiomatic path, and stop without opening a PR if the work expands
   protected architecture without a prior maintainer appointment or a linked
   issue labeled `accepted`. Review the
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
   Body must use the repository's .github/PULL_REQUEST_TEMPLATE.md exactly;
   fill every section rather than writing an ad-hoc body. Do NOT pass
   --label flags — the upstream auto-labeler may apply needs-maintainer
   automatically based on the branch name and body content.
8. Print the PR URL and exit.

All-tier gates: after the final commit and before pushing, run the final
$review-impl and then Gate A against that exact HEAD; include their SHA-bound PASS lines,
at least two file:line anchors and no unchecked required verification boxes.
A later commit invalidates both PASS
lines. Tier cannot satisfy these gates. If protected architecture lacks a prior
maintainer appointment or linked issue labeled `accepted`, do not open the PR.

Card / issue: {CARD_NAME, issue number, or "pick one"}
```
