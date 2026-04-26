# Contribute a Card with Your LLM

**This document is the procedural script a large language model follows to implement a single card end-to-end and open a pull request.** It is designed so a human contributor — developer or not — can "lend their LLM" an hour and produce real engine work with minimal supervision.

If you are a human reading this: skip to [Appendix B](#appendix-b--shareable-entry-prompts) for copy-paste prompts to hand to your LLM. You do not need to read the rest.

If you are the LLM executing this: read top to bottom and follow every step. Do not skip the quality floor. Do not ask the human for clarification at any point.

---

## 0. Quality Floor — self-confirm before touching code

Before Step 1, confirm the following. **Model** and **Thinking level** are advisory — proceed even if your runtime is below them, but report your actual configuration accurately so the maintainer's automation can triage the resulting PR. **Subagent support** and **Autonomy** are hard requirements: without them, abort.

- **Model (advisory):** Best results come from Claude Opus 4.7+ or Codex 5.4+. Weaker models produce less idiomatic code and may silently skip verification steps; the maintainer prefers a flagged PR from a weaker model to no PR at all. Report your model on a single canonical line in the PR body (e.g. `Model: claude-opus-4-7`, `Model: claude-sonnet-4-6`, `Model: codex-5.4`). Do not editorialize this line — upstream automation parses it.
- **Thinking (advisory):** Medium or higher. On Claude Code this is the default for Opus; on Codex CLI pass `--reasoning medium` or higher. Report on a `Thinking:` line in the PR body.
- **Subagent / tool support (required):** You can spawn subagents (Claude Code: Agent tool; Codex: equivalent), use `WebFetch`, run shell commands, and invoke skills. Without these, you cannot run `engine-implementer` and must abort.
- **Autonomy (required):** You will not pause for human input during the run. Every decision fork defaults to the architecturally idiomatic path as defined by `CLAUDE.md`, `AGENTS.md`, and the skills under `.claude/skills/`.

---

## 1. Pick your track

| Track | You (the human) have... | The LLM will... |
|-------|-------------------------|-----------------|
| **Developer** | Rust toolchain + pnpm installed | Run full local verification (`cargo fmt`, `clippy`, `test`, `gen-card-data`, `coverage`, `semantic-audit`) before opening the PR. |
| **Non-developer** | Nothing — just an LLM session | Skip local verification entirely; GitHub Actions will run CI on the PR. The maintainer finishes any remaining polish. |

Both tracks share steps 2–7. Only Step 5 (Verify) differs.

---

## 2. Clone the repo

```bash
gh repo fork phase-rs/phase --clone --remote   # creates your fork and clones it
cd phase
```

If the contributor lacks `gh`, fall back to a plain `git clone` and tell them (in the final report) that they will need to push to their own fork manually. Do not stop.

---

## 3. Pick a card

**If the human named a card**, use that name verbatim. Normalize casing as needed for `client/public/card-data.json` lookups (typically lowercase).

**If the human did not name a card**, fetch the latest coverage data directly from the published R2 endpoint (no local `cargo coverage` needed):

```
WebFetch: https://pub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev/staging/coverage-data.json
```

From the JSON, select a card where:
- `supported == false`, and
- `gap_count` is small (prefer 1–3 — these are the lowest-risk wins), and
- the card has no known deferred-infrastructure dependency (skip anything referencing Rooms, Enchant Player, Suspend Aggression — see `memory/` notes in the repo if available, otherwise ignore).

Record the chosen card name. It will appear in the branch name, commit message, and PR title.

---

## 4. Implement with `engine-implementer`

Create a branch (with a collision guard so re-runs on the same fork don't fail):

```bash
slug="card/<slug-of-card-name>"
n=2
while git rev-parse --verify "refs/heads/$slug" >/dev/null 2>&1 \
   || git ls-remote --exit-code origin "$slug" >/dev/null 2>&1; do
  slug="card/<slug-of-card-name>-$n"
  n=$((n + 1))
done
git checkout -b "$slug"
```

Then invoke the `engine-implementer` agent (Claude Code: use the Agent tool with `subagent_type: engine-implementer`; Codex: see [Appendix A](#appendix-a--codex-cli-equivalents)) with this prompt, substituting `<NAME>`:

> Implement full engine support for the card "<NAME>". Follow `CLAUDE.md` and `AGENTS.md` design principles without exception: build for the class not the card, nom combinators on first pass, CR annotations verified against `docs/MagicCompRules.txt` (and for each cited rule, also read its adjacent rules in the same section — cite the *authorizing* rule for the effect, not just the *layering* rule), idiomatic Rust, engine owns all logic, frontend is display-only. Reuse existing building blocks before writing new ones. Do not ask for clarification — on any ambiguity, take the architecturally idiomatic path. If scope expands beyond a single effect (e.g. the card requires new infrastructure, a new keyword, a new replacement pipeline), proceed anyway and explicitly note the scope expansion in your final report under a heading "Scope Expansion".

`engine-implementer`'s published contract is: plan (via `engine-planner` sub-agent) → implement → run `/review-impl`. Do **not** spawn a second reviewer on top — *if it actually ran the review.* Validate that next.

---

## 5. Validate the review actually happened and was addressed

> This is the most important step. `engine-implementer` frequently claims to have run `/review-impl` without actually doing so, or runs it and acknowledges findings without fixing them. The outside caller (you, the LLM reading this) must verify.

Apply **all three** checks:

1. **Review section exists with concrete findings.** The agent's final report must contain an explicit `/review-impl` section enumerating findings with file:line references. Generic phrasing like "review passed" or "no issues found" with no enumerated items counts as *missing*, not clean.
2. **Findings were addressed with code.** For every finding classified as a defect, gap, or missing case, there must be a corresponding change in `git diff HEAD~ HEAD` (or the working tree if not yet committed). An acknowledgement without a diff is a failure.
3. **Clean-review cross-check (fresh context).** If the report claims zero findings, **spawn a new subagent** for an independent pass — do NOT re-message the still-running `engine-implementer`, and do NOT call `/review-impl` from the same context. Claude Code: invoke the `Agent` tool with `subagent_type: feature-dev:code-reviewer` (or `code-quality-reviewer`); Codex: open a fresh sandbox session. Hand it ONLY the unified diff (`git diff HEAD~ HEAD`), `CLAUDE.md`, and the relevant skills under `.claude/skills/`. No prior conversation. The subagent must explicitly check: (a) **nom-mandate compliance** — flag any `match` over a stringified parser-text variable with string-literal arms, any chained `if let Ok(..) = tag(..)` blocks, and any string-method dispatch (`.contains("…")`, `.find("…")`, `.split_once`, etc.); (b) **CR-citation completeness** — for each cited rule, did the implementation also cite the *authorizing* rule, not just the *layering* rule? (c) **pattern coverage** — does this work for ≥10 cards or just one? (d) **logic placement** — engine vs frontend per `CLAUDE.md`; (e) **building-block reuse** — did the implementation duplicate logic an existing helper already handles? Re-implementing what `oracle_util.rs`, `oracle_quantity.rs`, `game/filter.rs`, `game/zones.rs`, etc. already provide is a defect even if the new code works; (f) **bool-flag avoidance** — any new `bool` field/parameter where a typed enum (`ControllerRef`, `Comparator`, `Option<T>`, etc.) would express the design space better is a defect. If the cross-check produces findings, the original review was incomplete — feed them back and loop.

**If any check fails:** send a follow-up message to the still-running `engine-implementer` agent (Claude Code: `SendMessage` by agent name) instructing it to actually execute `/review-impl` and address every finding with code changes. Do **not** proceed to Step 6 until validation passes. Retry at most 2 times; on a third failure, abort the run and record the gap in the PR body under a "Validation Failures" heading so the maintainer can triage.

---

## 6. Verify (track-specific)

**Developer track** — run in this order. On any failure, fix in-loop (max 2 retries) before proceeding. If still failing after retries, record the failure in the PR body under "CI Failures" and continue to Step 7 — do not abort.

```bash
cargo fmt --all
cargo clippy-strict
./scripts/check-parser-combinators.sh         # nom-mandate gate (also runs in pre-commit + CI)
cargo test -p engine
./scripts/gen-card-data.sh
cargo coverage                                # confirm the named card now has supported: true, gap_count: 0
cargo semantic-audit                          # confirm the named card surfaces zero findings
```

**Non-developer track** — skip this step entirely. GitHub Actions runs the same checks on the PR.

---

## 7. Open the pull request

Claude Code: invoke the `commit-push-pr` skill. Codex / other: run the equivalent shell sequence:

```bash
git add -A
git commit -m "Add <Card Name>"
git push -u origin HEAD
gh pr create --title "<title>" --body "<body>"   # no --label arg; upstream auto-labeler handles it
```

**PR title:** `Add <Card Name>` for the default case, including runs that grew in scope — a clean `Add` may legitimately ship new infrastructure as a building block, and size alone is not a signal of incompleteness. Use `Partial: <Card Name>` only if Step 5 logged validation failures or Step 6 logged CI failures the run could not resolve.

**PR body template:**

```markdown
## Summary
Adds engine support for **<Card Name>**.

## Files changed
<brief bulleted list — paths only, no prose>

## CR references
<list of `CR XXX.Y` annotations added or touched>

## Track
<Developer | Non-developer>

## LLM
Model: <claude-opus-4-7 | claude-sonnet-4-6 | codex-5.4 | …>
Thinking: <medium | high | max>

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

**Labels:** Applied automatically by the `auto-label-ai-contribution` workflow on PR open/edit/synchronize. Fork PRs cannot self-label, so do not pass `--label` to `gh pr create`.

- `ai-contribution` — applied to any PR with a `card/*` head branch or matching template signature.
- `needs-maintainer` — added when **any** of:
  - `Track:` is `Non-developer`, or
  - the `## Scope Expansion`, `## Validation Failures`, or `## CI Failures` heading has content other than the literal default `None.`, or
  - the `## Verification` heading is missing/empty on a Developer-track PR.

---

## 8. Report and exit

Print the PR URL. Print a one-line status: `success`, `partial`, or `aborted`. Exit cleanly. Do not linger for further input.

---

## Appendix A — Codex CLI equivalents

Codex CLI does not support Claude-specific subagent invocation or skill names (`engine-implementer`, `/review-impl`, `commit-push-pr`). Substitute as follows:

- **Invoking `engine-implementer`:** read `.claude/agents/engine-implementer.md` and follow its pipeline manually — first run the planning phase (read `.claude/agents/engine-planner.md`, produce a plan with its six mandatory architectural sections), then implement, then run the review step from `.claude/commands/review-impl.md`.
- **Invoking `/review-impl`:** open `.claude/commands/review-impl.md` and execute its checklist against the uncommitted diff.
- **Invoking `commit-push-pr`:** run the raw `git` + `gh` sequence shown in Step 7.

Every other step (quality floor, track selection, clone, card pick, validation, verify, report) is tool-agnostic and applies to Codex identically.

---

## Appendix B — Shareable entry prompts

Paste one of these into your LLM. That is the entire interaction.

### B.1 — Developer track, URL-only (shortest)

```
Read https://raw.githubusercontent.com/phase-rs/phase/main/docs/AI-CONTRIBUTOR.md
and follow the Developer track end-to-end to implement the card {CARD_NAME, or
say "pick one" and let the LLM choose}. Use medium thinking. Do not stop for
my input. Open a PR when done.
```

### B.2 — Non-developer track, URL-only

```
Read https://raw.githubusercontent.com/phase-rs/phase/main/docs/AI-CONTRIBUTOR.md
and follow the Non-developer track end-to-end to implement the card {CARD_NAME,
or say "pick one"}. Skip local verification — GitHub Actions will run CI on the
PR. Use medium thinking. Do not stop for my input. Open a PR when done.
```

### B.3 — Non-developer track, fully self-contained (for UIs without web fetch)

```
You are going to implement one Magic: The Gathering card in the phase-rs/phase
repository end-to-end and open a pull request. Do not pause to ask me anything.

Requirements: Best results with Claude Opus 4.7+ or Codex 5.4+ at medium+
thinking, but proceed even if your runtime is below that — just report your
actual model on a single canonical "Model:" line in the PR body (e.g.
"Model: claude-sonnet-4-6"). Do NOT editorialize that line. Hard requirements:
you can spawn subagents, run shell commands, and you will not pause for input.

Steps:
1. gh repo fork phase-rs/phase --clone --remote && cd phase
2. If I named a card, use it. Otherwise WebFetch
   https://pub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev/staging/coverage-data.json
   and pick a card with supported==false and small gap_count.
3. git checkout -b card/<slug>  (if the branch already exists locally or on
   origin, append "-2", "-3", etc. — see Step 4 in docs/AI-CONTRIBUTOR.md).
4. Invoke the engine-implementer agent to implement the card. Tell it: follow
   CLAUDE.md and AGENTS.md without exception, nom combinators on first pass,
   CR annotations verified against docs/MagicCompRules.txt (and cite the
   authorizing rule, not just the layering rule), do not ask for clarification,
   take the idiomatic path, proceed even if scope expands.
5. Validate engine-implementer actually ran /review-impl AND addressed every
   finding with code changes. If not, send it a follow-up to do so (max 2
   retries). If the review claims zero findings, spawn a NEW subagent (Claude
   Code: Agent tool with subagent_type feature-dev:code-reviewer or
   code-quality-reviewer) and hand it only the diff + CLAUDE.md — do not
   re-review from inside the same context.
6. Skip local verification (I don't have a Rust toolchain).
7. git push to my fork and open a PR with title "Add <Card Name>" (or
   "Partial: <Card Name>" only if validation or CI failures were unresolved).
   Body must follow the template in docs/AI-CONTRIBUTOR.md. Do NOT pass
   --label flags — the upstream auto-labeler applies ai-contribution and
   needs-maintainer automatically based on the branch name and body content.
8. Print the PR URL and exit.

Card: {CARD_NAME or "pick one"}
```
