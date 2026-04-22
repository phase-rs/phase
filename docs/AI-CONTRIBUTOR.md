# Contribute a Card with Your LLM

**This document is the procedural script a large language model follows to implement a single card end-to-end and open a pull request.** It is designed so a human contributor — developer or not — can "lend their LLM" an hour and produce real engine work with minimal supervision.

If you are a human reading this: skip to [Appendix B](#appendix-b--shareable-entry-prompts) for copy-paste prompts to hand to your LLM. You do not need to read the rest.

If you are the LLM executing this: read top to bottom and follow every step. Do not skip the quality floor. Do not ask the human for clarification at any point.

---

## 0. Quality Floor — self-confirm before touching code

Before Step 1, confirm **all** of the following. If any fails, abort with a single-line message identifying the bar missed.

- **Model:** You are Claude Opus 4.7 (or later) OR Codex 5.4 (or later). Weaker models will produce non-idiomatic code and silently skip verification steps. No exceptions.
- **Thinking level:** Medium or higher. On Claude Code this is the default for Opus; on Codex CLI pass `--reasoning medium` or higher.
- **Subagent / tool support:** You can spawn subagents (Claude Code: Agent tool; Codex: equivalent), use `WebFetch`, run shell commands, and invoke skills. Without these, you cannot run `engine-implementer` and must abort.
- **Autonomy:** You will not pause for human input during the run. Every decision fork defaults to the architecturally idiomatic path as defined by `CLAUDE.md`, `AGENTS.md`, and the skills under `.claude/skills/`.

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

Create a branch:

```bash
git checkout -b card/<slug-of-card-name>
```

Then invoke the `engine-implementer` agent (Claude Code: use the Agent tool with `subagent_type: engine-implementer`; Codex: see [Appendix A](#appendix-a--codex-cli-equivalents)) with this prompt, substituting `<NAME>`:

> Implement full engine support for the card "<NAME>". Follow `CLAUDE.md` and `AGENTS.md` design principles without exception: build for the class not the card, nom combinators on first pass, CR annotations verified against `docs/MagicCompRules.txt`, idiomatic Rust, engine owns all logic, frontend is display-only. Reuse existing building blocks before writing new ones. Do not ask for clarification — on any ambiguity, take the architecturally idiomatic path. If scope expands beyond a single effect (e.g. the card requires new infrastructure, a new keyword, a new replacement pipeline), proceed anyway and explicitly note the scope expansion in your final report under a heading "Scope Expansion".

`engine-implementer`'s published contract is: plan (via `engine-planner` sub-agent) → implement → run `/review-impl`. Do **not** spawn a second reviewer on top — *if it actually ran the review.* Validate that next.

---

## 5. Validate the review actually happened and was addressed

> This is the most important step. `engine-implementer` frequently claims to have run `/review-impl` without actually doing so, or runs it and acknowledges findings without fixing them. The outside caller (you, the LLM reading this) must verify.

Apply **all three** checks:

1. **Review section exists with concrete findings.** The agent's final report must contain an explicit `/review-impl` section enumerating findings with file:line references. Generic phrasing like "review passed" or "no issues found" with no enumerated items counts as *missing*, not clean.
2. **Findings were addressed with code.** For every finding classified as a defect, gap, or missing case, there must be a corresponding change in `git diff HEAD~ HEAD` (or the working tree if not yet committed). An acknowledgement without a diff is a failure.
3. **Clean-review cross-check.** If the report claims zero findings, re-run `/review-impl` yourself once as an independent pass. If the cross-check produces findings, the original review was incomplete — feed them back and loop.

**If any check fails:** send a follow-up message to the still-running `engine-implementer` agent (Claude Code: `SendMessage` by agent name) instructing it to actually execute `/review-impl` and address every finding with code changes. Do **not** proceed to Step 6 until validation passes. Retry at most 2 times; on a third failure, abort the run and record the gap in the PR body under a "Validation Failures" heading so the maintainer can triage.

---

## 6. Verify (track-specific)

**Developer track** — run in this order. On any failure, fix in-loop (max 2 retries) before proceeding. If still failing after retries, record the failure in the PR body under "CI Failures" and continue to Step 7 — do not abort.

```bash
cargo fmt --all
cargo clippy-strict
cargo test -p engine
./scripts/gen-card-data.sh
cargo coverage
cargo semantic-audit
```

**Non-developer track** — skip this step entirely. GitHub Actions runs the same checks on the PR.

---

## 7. Open the pull request

Claude Code: invoke the `commit-push-pr` skill. Codex / other: run the equivalent shell sequence:

```bash
git add -A
git commit -m "Add <Card Name>"   # or "Partial: <Card Name>" if scope expanded
git push -u origin HEAD
gh pr create --title "<title>" --body "<body>" --label ai-contribution
```

**PR title:** `Add <Card Name>` — or `Partial: <Card Name>` if Step 4's "Scope Expansion" heading was populated, or Step 5 logged validation failures, or Step 6 logged CI failures.

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
Model: <Claude Opus 4.7 | Codex 5.4 | …>
Thinking level: <medium | high>

## Scope Expansion
<present only if engine-implementer reported scope growth — briefly describe the new infrastructure added>

## Validation Failures
<present only if Step 5 could not be made to pass after retries>

## CI Failures
<present only if Step 6 surfaced a failure the LLM could not resolve>
```

**Labels:**
- Always: `ai-contribution`
- Additionally `needs-maintainer` if ANY of: track is Non-developer, scope expanded, validation failed, or CI failed.

---

## 8. Report and exit

Print the PR URL. Print a one-line status: `success`, `partial`, or `aborted`. Exit cleanly. Do not linger for further input.

---

## Appendix A — Codex CLI equivalents

Codex CLI does not support Claude-specific subagent invocation or skill names (`engine-implementer`, `/review-impl`, `commit-push-pr`). Substitute as follows:

- **Invoking `engine-implementer`:** read `.claude/agents/engine-implementer.md` and follow its pipeline manually — first run the planning phase (read `.claude/agents/engine-planner.md`, produce a plan with its six mandatory architectural sections), then implement, then run the review step from `.claude/skills/review-impl/SKILL.md` (or its nearest equivalent under `.claude/skills/`).
- **Invoking `/review-impl`:** open `.claude/skills/review-impl/SKILL.md` (if present) and execute its instructions against the uncommitted diff. If the skill file is absent, perform an independent critical re-read of the diff checking: pattern coverage (works for 50 cards or 1?), building-block reuse, CR annotation correctness, idiomatic Rust, logic placement (engine vs frontend).
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

Requirements: You must be Claude Opus 4.7+ or Codex 5.4+ at medium+ thinking. If
not, abort and tell me which bar you missed.

Steps:
1. gh repo fork phase-rs/phase --clone --remote && cd phase
2. If I named a card, use it. Otherwise WebFetch
   https://pub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev/staging/coverage-data.json
   and pick a card with supported==false and small gap_count.
3. git checkout -b card/<slug>
4. Invoke the engine-implementer agent to implement the card. Tell it: follow
   CLAUDE.md and AGENTS.md without exception, nom combinators on first pass,
   CR annotations verified against docs/MagicCompRules.txt, do not ask for
   clarification, take the idiomatic path, proceed even if scope expands.
5. Validate engine-implementer actually ran /review-impl AND addressed every
   finding with code changes. If not, send it a follow-up to do so (max 2
   retries). If the review claims zero findings, re-run /review-impl yourself
   as a cross-check.
6. Skip local verification (I don't have a Rust toolchain).
7. git push to my fork and open a PR with title "Add <Card Name>" (or
   "Partial: <Card Name>" if scope expanded or validation failed). Label it
   "ai-contribution" and "needs-maintainer". Body must follow the template in
   docs/AI-CONTRIBUTOR.md.
8. Print the PR URL and exit.

Card: {CARD_NAME or "pick one"}
```
