---
name: bug-triage
description: "Use when triaging Phase Discord bug reports, syncing reports into triage artifacts, creating or updating GitHub issues, tracking bug clusters, or applying the repository's bug-fix review workflow."
---

# Bug Triage System — Operator Reference

## Quick Commands

**Working directory — run everything from the repo root** (`/Users/matt/dev/forge.rs`),
NOT from this skill directory. The triage script lives at `./scripts/sync-bug-reports.ts`
and every path it reads/writes (`triage/…`, `client/public/card-data.json`) is resolved
relative to the current working directory. Running from `.claude/skills/bug-triage/`
fails with `Module not found "scripts/sync-bug-reports.ts"`.

```bash
# Full pipeline (fetch new Discord messages → extract → triage → render)
bun scripts/sync-bug-reports.ts fetch
bun scripts/sync-bug-reports.ts extract
bun scripts/sync-bug-reports.ts triage     # also emits triage/triage-delta.jsonl
bun scripts/sync-bug-reports.ts render

# Review ONLY the delta — the reports new since the last fetch. NEVER scan the
# full triage-items.jsonl looking for "what's new"; that is how reports get
# missed. `triage` prints the delta + a "reports to resolve" list (every
# non-skip item).
bun scripts/sync-bug-reports.ts delta      # re-emit delta without re-classifying

# CRITICAL — the script does NOT dedup against GitHub and does NOT pre-judge
# duplicates (only `create_issue` / `skip` / `needs_human_review` exist). YOU are
# the arbiter: for each non-skip delta item, decide whether it is a new bug, a
# duplicate of an existing issue, or already-known, then act on it. `publish`
# only files the threads you pass via `--thread`; every other non-skip delta item
# (`needs_human_review`, or a `create_issue` thread you did not publish) must
# still be resolved inline — see *Delta Completion Invariant* below.

# Publish: for each --thread, CREATE a new GH issue from the triage item AND
# react 👀 + post a tracking link inside the originating Discord thread.
# IMPORTANT: `publish` ALWAYS creates a NEW issue (resolveIssue(..., "created"))
# — it has NO reconcile / write-back-only mode, despite older doc claims. The
# ONLY dedup gate is `published_threads` in triage/sync-state.json: a thread
# already recorded there is skipped entirely (no issue, no write-back). So use
# `publish` ONLY for threads that have NO GH issue yet. If an issue already
# exists (incl. one you filed by hand), do NOT run `publish` — it would create
# a duplicate. Use the manual write-back procedure below instead.
bun scripts/sync-bug-reports.ts publish --thread=<id>[,<id>...] --dry-run   # preview without side effects
bun scripts/sync-bug-reports.ts publish --thread=<id>[,<id>...]             # create GH issue + Discord write-back

# Check a specific card's parser status
jq '.["card name"]' client/public/card-data.json
jq '.["card name"] | {abilities: [.abilities[]? | select(.effect.type == "Unimplemented")], triggers: [.triggers[]? | select(.mode == "Unknown")]}' client/public/card-data.json

# Regenerate card data (after parser changes)
./scripts/gen-card-data.sh

# Single card debug
cargo run --bin oracle-gen -- data --filter "card name"

# Active cluster trackers (open thematic workstreams) — see Cluster Tracking with Sub-Issues below
gh issue list --repo phase-rs/phase --label "collector" --state open

# View a tracker and its sub-issues
gh issue view <N> --repo phase-rs/phase --json subIssues,title,body

# Browse closed trackers (retrospective archive)
gh issue list --repo phase-rs/phase --label "collector" --state closed --limit 50 --json number,title,closedAt
```

## Delta Completion Invariant — Every Non-Skip Item, Same Cycle

**Hard rule.** A fetch cycle is NOT done until *every* delta item with a
non-`skip` proposed action has been resolved — either an issue created with
write-back, an existing issue linked + write-back, OR a `mark-handled`
sentinel. The bottom line: after a fetch cycle, the count of unhandled
non-skip threads must be **zero**. There is no "I'll come back to these
later" — later never comes, and the reporter sees a stale Discord thread.

**The recurring failure (do NOT regress it):**
`bun scripts/sync-bug-reports.ts publish` only files the threads you pass via
`--thread`. The script does NOT dedup against GitHub and emits only
`create_issue` / `skip` / `needs_human_review`, so the leftover slice after
publishing the obvious `create_issue` threads is `needs_human_review` plus any
`create_issue` thread you chose not to publish. Operators have repeatedly
published the obvious slice, glanced at the resolve list, and moved on —
leaving those threads with no Discord eyes / no tracking link / no
published_threads entry. This is the orphan source. Two fetch rounds in a row
hit it; the user noticed.

**Mandatory cycle close-out** — run after `publish` on the `create_issue` slice:

```bash
# 1. List every delta item that is NOT create_issue or skip — i.e. the
#    needs_human_review leftovers. These MUST all be resolved before the
#    cycle is done. (create_issue threads are handled by `publish`.)
jq -r 'select(.proposed_action != "create_issue"
              and .proposed_action != "skip")
       | "\(.thread_id) | \(.thread_name) | \(.proposed_action)"' \
  triage/triage-delta.jsonl
```

For each thread the list prints, do ONE of the following inline — do NOT
park it for a subagent unless you can sit and watch the subagent finish:

| Decision | Action |
|----------|--------|
| **NEW** — heuristic misclassified; this is a fresh report no existing issue covers | `gh issue create` + Discord write-back (Path B in *Discord Write-Back*); record `mode:"inline"` in `published_threads` |
| **DUP-OF-#N** — true duplicate of an existing open/closed issue | Discord write-back to #N (Path B); record `mode:"reconciled"` in `published_threads` |
| **APPEND-TO-#N** — followup on a still-open issue worth a comment | `gh issue comment N --body "..."` + Discord write-back; record `mode:"reconciled"` |
| **MARK-HANDLED** — chatter / self-resolved / not a bug | Write a sentinel `{issue_number:0, mode:"mark-handled", reason:"<one line>"}` in `published_threads`; no GH, no Discord |

Inline procedure for fast batch handling — a single shell session can do the
12-thread case in a couple of minutes:

```bash
set -a; . ./.env; set +a                  # load DISCORD_BOT_TOKEN
DC=https://discord.com/api/v10/channels
GH=phase-rs/phase

# Pull raw content for the threads in one pass to inform decisions:
for tid in $TID_LIST; do
  echo "=== $tid ==="
  jq -r --arg t "$tid" 'select(.thread_id==$t) | "\(.thread_name)\n  \(.content)"' \
    triage/raw/discord-messages.jsonl | head -8
done

# Then, for each decision, use the helpers documented in
# *Discord Write-Back* (Path B). The 2026-05-20 inline-batch implementation
# at git log -1 --grep "12 unpublished threads" is a worked reference.
```

**Verification — same as the published_threads invariant audit:**

```bash
# Every delta non-skip thread must now appear in published_threads with
# either a real reply_message_id (real issue) OR mode:"mark-handled".
jq -r 'select(.proposed_action != "create_issue"
              and .proposed_action != "skip")
       | .thread_id' triage/triage-delta.jsonl \
  | while read tid; do
      entry=$(jq --arg t "$tid" '.published_threads[$t]' triage/sync-state.json)
      if [ "$entry" = "null" ]; then
        echo "ORPHAN (no published_threads entry): $tid"
      fi
    done
```

This MUST print nothing before you call the cycle done.

**Why subagents alone are not sufficient:** subagents are fine for the
*decisions* (DUP vs NEW vs MARK-HANDLED), but the GH `create` + Discord
write-back calls have side effects that must succeed end-to-end in the same
window. Park a 13-thread dedupe in a subagent and you'll come back to find
the user filed two more cycles' worth of reports while those 13 still have
no eyes. Decisions can parallelize; the I/O loop closes inline.

---

## Discord Write-Back — MANDATORY After Every Issue Filing

**Invariant:** every Discord-sourced thread that ends up with a GH issue (newly
filed OR linked to an existing one) MUST finish with BOTH:
1. a 👀 reaction on the thread's starter message, AND
2. a `🔗 Tracked in <issue-url>` reply posted inside the thread.

The thread is "done" only when the reporter can see the eyes + the link. A
`published_threads` entry in `triage/sync-state.json` is the *bookkeeping* of
that — it is NOT a substitute for it.

### The recurring failure (do NOT do this)

Hand-writing a `published_threads` entry **without posting the write-back**
marks the thread as published while the Discord thread still has no reaction
and no link. An entry with empty `reacted_message_id` / `reply_message_id` is
the signature of this bug. Never record `published_threads` for a thread you
have not actually written back to.

### Two paths — pick by whether a GH issue already exists

**Path A — no GH issue exists yet for the thread.** Use `publish`. It creates
the issue, posts the 👀 + link, and records `published_threads` with the real
message ids — all in one step. Nothing else to do.

```bash
bun scripts/sync-bug-reports.ts publish --thread=<id>[,<id>...]
```

**Path B — a GH issue already exists** (you filed it by hand because heuristic
card extraction mangled the card names; or you appended to / commented on an
existing issue; or you deduped to a prior issue). `publish` would create a
DUPLICATE here — do NOT run it. Do the write-back by hand, then record state.
For a thread, the starter message id equals the thread id, and the channel id
equals the thread id. `$DISCORD_BOT_TOKEN` is in `.env`.

```bash
set -a; . ./.env; set +a            # load DISCORD_BOT_TOKEN
TID=<thread_id>; ISSUE=<issue_number>

# 1. React 👀 on the thread starter (%F0%9F%91%80 is the 👀 emoji):
curl -sf -X PUT -H "Authorization: Bot $DISCORD_BOT_TOKEN" \
  "https://discord.com/api/v10/channels/$TID/messages/$TID/reactions/%F0%9F%91%80/@me"

# 2. Post the tracking link — capture the returned message id:
REPLY_ID=$(curl -sf -X POST -H "Authorization: Bot $DISCORD_BOT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"content\":\"🔗 Tracked in https://github.com/phase-rs/phase/issues/$ISSUE\"}" \
  "https://discord.com/api/v10/channels/$TID/messages" | jq -r .id)
echo "posted reply $REPLY_ID"
```

Then — and only after both calls succeeded — record `published_threads` with
the REAL ids (never empty strings):

```bash
jq --arg t "$TID" --argjson n "$ISSUE" --arg r "$REPLY_ID" --arg ts "$(date -u +%FT%TZ)" \
  '.published_threads[$t] = {issue_number:$n, issue_url:("https://github.com/phase-rs/phase/issues/"+($n|tostring)), reacted_message_id:$t, reply_message_id:$r, published_at:$ts, mode:"reconciled"}' \
  triage/sync-state.json > triage/sync-state.json.tmp && mv triage/sync-state.json.tmp triage/sync-state.json
```

If a thread is archived, the Discord calls fail with error `50083` — archive
is the maintainer's manual "resolved" signal; leave it, do not unarchive.

### Verify before declaring triage done

For every thread touched this cycle, confirm its `published_threads` entry that
points at a real issue (`issue_number > 0`) also has a non-empty
`reply_message_id`. (`mark-handled` sentinels legitimately have `issue_number:
0` and empty write-back ids — they are not bugs and are excluded below.)

```bash
jq -r '.published_threads | to_entries[] | select(.value.issue_number > 0 and (.value.reply_message_id == "" or .value.reacted_message_id == "")) | .key' triage/sync-state.json
```

This MUST print nothing. Any thread id it prints has a real GH issue but was
marked published without a Discord write-back — go post the 👀 + link (Path B)
and re-record with the real message ids.

## GitHub Issue Workflow

```bash
# List open issues by priority
gh issue list --repo phase-rs/phase --state open --label "priority:p0-softlock"
gh issue list --repo phase-rs/phase --state open --label "priority:p1-core-mechanic"

# Close a fixed parser-gap issue only after the reported ability is semantically represented
gh issue close <N> --repo phase-rs/phase --comment "Fixed in <commit>. The reported ability now parses to the expected typed semantics with no Unimplemented fallback."

# Transition issue status
gh issue edit <N> --repo phase-rs/phase --remove-label "status:confirmed" --add-label "status:fixed-unreleased"
gh issue edit <N> --repo phase-rs/phase --remove-label "status:fixed-unreleased" --add-label "status:needs-runtime-verify"

# After runtime verification passes
gh issue close <N> --repo phase-rs/phase --comment "Verified in gameplay. Closing."
gh issue edit <N> --repo phase-rs/phase --remove-label "status:needs-runtime-verify" --add-label "status:verified"
```

### Mandatory Pre-Implementation Plan Review Gate — Independent Review ROUNDS Until Clean

Before any code is written for a triage item, the implementation plan must pass an **independent plan-review LOOP** applying `$review-engine-plan`: review → planner revises every gap → **fresh re-review of the revised plan** → repeat until a whole round returns clean. The gate is a *fixpoint* ("review until stable"), not a single pass and not "two reviewers, done." It runs *before* the post-fix gate — plan review catches design errors (wrong CR section, special-case instead of building-block, missing sibling coverage, blast-radius/registration gaps) when they cost a plan revision instead of a full re-implementation.

How to apply:

```
# After engine-planner produces a plan, run independent ROUNDS. NOTE: the
# engine-planner's own internal "architectural analysis" section does NOT
# count — run independent reviewers AFTER it finalizes.
#
# Each round:
#   - Spawn fresh-context reviewer(s) (NOT the planner). Two with complementary
#     emphases (one CR/rules-correctness, one architecture/parameterization)
#     surface disjoint error classes well in round 1; a single rigorous
#     reviewer is fine for later rounds.
#   - Send EVERY gap (consolidated) back to the planner; it revises the plan.
#   - RE-REVIEW THE WHOLE REVISED PLAN with fresh context — NOT just the
#     changed sections. Revisions routinely INTRODUCE new gaps in
#     untouched-looking areas: closing one exhaustive-match site reveals a
#     5th/6th; a spec rewrite inverts a recompute path; a "verbatim reuse"
#     turns out to need new capability. Targeted-only re-review misses these.
#   - Repeat until a FULL round returns clean (no BLOCKER).
```

Do NOT start implementation — do not run `$engine-implementer` — until a full round is clean. Real plans this codebase has needed took 4–6 rounds; each round caught a genuine compile-break or rules-correctness defect. Plans should commit to a building-block fix, carry grep-verified CR cites, enumerate the new-variant/new-field blast radius (see below), and (for runtime bugs) be discriminator-first where practical (write the failing test first; let the failing checkpoint localize the bug).

### New-Variant / New-Field Blast Radius — Enumerate in the Plan, Verify in Review

Adding a new enum variant or a new field to a struct-variant has compile-time blast radius across the WHOLE workspace. This was the single most common gap class across multi-round plan reviews — make it an explicit plan section and a mandatory reviewer check.

- **New field on a struct-variant** (e.g. `split` on `Effect::SearchLibrary`): `#[serde(default)]` does NOT make it optional for Rust *literals*. Every `Effect::Variant { … }` construction across ALL crates (engine, phase-ai, mtgish-import, tests) must set it; every non-`..` destructure must bind it or add `..`. Enumerate with `grep -rn "Effect::Variant {" crates/` — treat the grep, not a hand-list, as authoritative.
- **New `WaitingFor` / enum variant**: breaks every EXHAUSTIVE `match` (no wildcard). Enumerate ALL sites by grepping a recently-added sibling variant (`rg "WaitingFor::SomeRecentVariant"`), then classify each match as exhaustive (needs an arm) vs `_`/`..`/`if let`/`matches!` (doesn't). Span engine + phase-ai + server-core. One `WaitingFor` variant typically needs ~5 arms (e.g. `acting_player`, `ai_support/candidates`, `phase-ai/decision_kind::classify`, `phase-ai/search::fallback_action`, `game/scenario::waiting_for_kind`).
- **THE SILENT KILLER — `matches!` dispatch gates**: a handler reachable only if its variant is listed in a `matches!` guard (e.g. `engine_resolution_choices::handles`) is NOT compile-checked. Omit it and the handler is silent dead code — the action falls through to InvalidAction; clippy and the type system say nothing. Grep for `matches!`/membership predicates that gate dispatch and add the variant; add a test asserting the action is *handled* (not InvalidAction).

### Cosmetic-AST Fix Is a Non-Fix — Trace the Runtime Consumer

A misparse fix can flip the AST to *look* right while the runtime driver ignores the change. (Documented: routing "for each opponent" through `player_scope` made the AST read `ScopedPlayer`, but the `repeat_for` driver never binds `scoped_player`, so it collapsed back to the caster — zero behavioral change; and a `TargetPlayer` object spec read fine in the AST but enumerated EMPTY on the recompute path because that path only honors `controller: You`.) The planner MUST trace the effect HANDLER end-to-end, not just the parser/driver, and the discriminating test MUST be RUNTIME (drive `apply()` and observe state), never AST-only. An AST snapshot proves the parse didn't regress — not that the bug is fixed.

### Mandatory Post-Fix Review Gate — Isolated Reviewer Required

Every code fix made during bug triage must pass an **isolated reviewer agent's** application of `$review-impl` before the fix is committed, marked fixed, or described as complete.

**Self-review by the implementing agent is NOT sufficient.** Multiple commits during the 2026-05-11 bug-triage rounds passed implementer self-review but had real issues caught only by a fresh-context reviewer (CR hallucinations, tests bypassing the pipeline they claim to exercise, predicate-narrowness latent bugs, missing CR sub-parts that don't exist). Implementers rationalize their own choices; fresh-context reviewers do not.

How to apply:

```bash
# After the implementer ships a commit:
git log --oneline -1   # capture the SHA

# Spawn an isolated code-quality-reviewer agent (NOT the implementer) with:
#   - the commit SHA
#   - the review charter from $review-impl
#   - explicit "you have not seen the implementation" framing
```

The reviewer must read the diff (`git show <sha>`) with fresh context and apply the `$review-impl` checklist. Required focus areas:

- Missing sibling coverage / parameterization smells
- Overly broad parser or runtime semantics
- **CR annotation correctness** (mandatory grep-verification — see next section)
- **Test rigor** (runtime tests must drive the engine pipeline — see Runtime Test Discipline below)
- Hidden state leaks
- Card-specific fixes that should have been modeled as reusable building blocks

If the reviewer flags issues:
- Send them back to the implementer via `SendMessage` (if still alive) for inline fix in a follow-up commit
- Re-spawn isolated review on the fixup commit's diff
- Repeat until the review is clean (typically 1-2 rounds in practice)

**Trivial-fixup carve-out.** A follow-up commit that is provably **doc-comment-only** (or otherwise zero-behavior — e.g. a comment reword, a typo) does NOT need a fresh isolated review round. Verify it by inspection instead: `git show --stat <sha>` plus a grep confirming every changed line is a comment (`git show <sha> -- <file> | grep -E '^[+-]' | grep -vE '^[+-]{3}' | grep -cvE '^[+-]\s*(//|///|$)'` must print `0`). This carve-out applies ONLY to genuinely behavior-free changes; any change touching code, signatures, or tests takes the full review round.

Do NOT transition GitHub issues to `fixed-unreleased`, `needs-runtime-verify`, `verified`, or closed until the isolated review is clean.

### CR Annotation Verification — Mandatory Grep-Proof

Every CR (Comprehensive Rules) number written into engine code MUST be grep-verified against `docs/MagicCompRules.txt` before the annotation is committed. This is non-negotiable — CR hallucinations have been a recurring failure mode across multiple keyword-synthesis commits.

Documented hallucinations from the 2026-05-11 session:
- `CR 702.93b` and `CR 702.79b` for Undying/Persist multi-instance — **subparts do not exist** (both keywords have only subpart `a`)
- `CR 701.16b` for sacrifice "as many as possible" — **subpart does not exist** AND **701.16 is Investigate, not Sacrifice** (701.21 is the sacrifice rule)
- `CR 702.122` for Fabricate — **wrong rule number** (702.122 is Crew; Fabricate is 702.123)
- `CR 702.85` for Annihilator — **wrong rule number** (702.85 is Cascade; Annihilator is 702.86)
- `CR 609.3` for optional triggered abilities — **wrong rule** (609.3 is partial-execution; 603.5 is the optional-trigger rule)
- `CR 608.2b` proposed as substitute for "as many as possible" — **wrong rule** (608.2b is target legality re-checking; 609.3 is the correct rule for "do as much as possible")

The pattern: LLMs infer-by-analogy that subparts like `X.Yb` SHOULD exist describing some edge case (multi-instance redundancy, fast-path partial-execution, etc.). They frequently don't. The comp rules are sparsely structured; many keyword rules have only subpart `a`.

**Before writing any CR annotation:**

```bash
grep -n "^<rule_number>" docs/MagicCompRules.txt
```

**Briefs given to implementer agents must include**:

1. An explicit list of grep commands for every CR likely to be cited
2. The acceptance criterion: "Paste the grep output line for every CR cite in your final report"
3. The session memory pointer: `feedback_cr_subpart_hallucination.md`

**Briefs given to isolated reviewer agents must include**:

1. The full list of past hallucination patterns (above) to specifically check for
2. The acceptance criterion: "Grep-verify every CR annotation in the diff. Any cite you cannot find at the cited line is a BLOCKER."

**Safe-default citation patterns**:

| Scenario | Citation |
|----------|----------|
| Multi-instance keyword redundancy | `CR 113.2c` (objects function with all their abilities) + absence of explicit redundancy clause analogous to CR 702.2f (deathtouch) / CR 702.9c (flying) |
| Optional triggered abilities ("you may") | `CR 603.5` (NOT `CR 609.3`) |
| Sacrifice action mechanic | `CR 701.21a` (NOT `CR 701.16` — that's Investigate) |
| "Do as many as possible" partial execution | `CR 609.3` |
| Target legality at resolution | `CR 608.2b` |
| Defending player (per-attacker, not aggregate) | `CR 508.5 / 508.5a` (NOT `CR 506.3d` — that's a specific creature-ETB scenario) |
| LKI for dies-trigger conditions | `CR 603.10a` (leaves-the-battlefield look-back) + `CR 400.7` (LKI semantics) |
| As-enters replacement timing | `CR 614.1c` |
| Counters lost on zone change | `CR 122.2` |

If you find a cite the implementer wrote that isn't in this table or in `MagicCompRules.txt`, treat it as a hallucination until proven otherwise.

### Runtime Test Discipline — Drive the Pipeline

Runtime tests for synthesized definitions (replacements, triggers, effects) **MUST drive the engine through the pipeline the synthesis is consumed by**. Tests that pre-construct expected state — bypassing the pipeline — prove nothing about pipeline correctness; they pass for the wrong reasons.

Documented anti-patterns from the 2026-05-11 session:
- **Fabricate runtime tests** injected `GameEvent::ZoneChanged` directly into `process_triggers`, bypassing cast → stack → resolve → ETB-replacement-window. Filed #357 to retrofit real end-to-end tests.
- **Modular `etb_replacement_starts_object_with_n_p1p1_counters`** directly inserted counters into `obj.counters` via a helper, bypassing the synthesized `ReplacementEvent::Moved` entirely. Test asserted both the replacement's shape AND the helper's manual mutation — proving consistency between two things the implementer wrote, not that the engine fires the replacement.
- **Modular `dies_transfers_modified_counter_count_after_hardened_scales`** manually mutated `obj.counters = 2` before death, never installing a Hardened Scales replacement. Proved LKI captures the live count, but NOT that Hardened Scales interacts correctly with Modular's ETB.
- **Modular `in_multiplayer_can_target_opponents_artifact_creature`** used `GameState::new_two_player`, not 3+ players. The name overpromised multiplayer-correctness.

The decision rule:

| Test type | What it asserts | What it proves |
|-----------|----------------|----------------|
| **SHAPE test** | The synthesized `ReplacementDefinition` / `TriggerDefinition` has the expected fields (correct event, valid_card, execute body) | The AST emitter produces the right structure. Valuable but limited. |
| **RUNTIME test** | After driving the engine through the relevant action (`move_to_zone`, `cast_spell`, `process_triggers` triggered by a real action, SBA resolution), the observable game state matches expectations | The engine pipeline consumes the synthesis correctly. The only kind of test that proves integration. |

**Rules for runtime tests**:

1. Identify the pipeline entry point you're testing (e.g., `move_to_zone(obj_id, Battlefield)` for ETB replacements; `state.declare_attackers(...)` for attack triggers).
2. Install the synthesized definition on the relevant `CardFace` / `GameObject` BEFORE driving the engine.
3. Drive the engine through the entry point — let it produce the observable state.
4. Assert against state the engine produced. Do NOT manually mutate `obj.counters`, `obj.tapped`, `obj.controller`, etc. to satisfy preconditions the engine should have produced.

**Every test must be proven discriminating (mutation-check).** A test that drives the pipeline can still pass for the wrong reason. Before a fix is considered complete, prove the test would FAIL without the fix: revert the fix (or the relevant arm), confirm the new test fails, then restore. The implementer's brief must require this and the reviewer must confirm it — phrase it as "reverted-fix-discriminating": with the fix reverted the test gets the *old wrong* result and the assertion fires. A test that passes both with and against the fix proves nothing. Run the mutation-check through Tilt's `test-engine` resource, never a direct `cargo test` (target-lock contention).

**Every fix is verified two ways — AST and runtime.** A fix is not complete until it is proven at *both* levels:
- **Parser AST** — `cargo run --bin oracle-gen -- data --filter "card name"` shows the reported clause lowering to the expected typed AST/IR (no `Unimplemented`, correct subject/controller/target/zone/condition). Required even for runtime bugs — it confirms the fix did not regress the parse.
- **Runtime** — a discriminating test (above) drives the engine pipeline and observes correct state.
Both proofs go in the implementer's report and are re-checked by the isolated reviewer.

**Specific anti-patterns to reject in review**:

- Helper functions that insert game-state values to satisfy a precondition the engine should have produced
- "Multiplayer" tests using a 2-player `GameState`
- Trigger tests calling `process_triggers(SyntheticEvent)` directly instead of producing the event via the game action that should emit it
- Replacement tests asserting the replacement's shape and assuming that proves the engine fires it
- LKI tests mutating the live counter map then asserting LKI reads it — proves the LKI cache reads from the live map, NOT that LKI captured pre-death state

When the pipeline-driving harness doesn't exist yet, **build it as part of the work** (per the No Default Deferral rule below). Cascade synthesis has such a harness; mirror it. Do not split "real tests" into a follow-up issue when the harness can be built in the same commit.

Session memory pointer: `feedback_runtime_tests_must_drive_pipeline.md`.

### Fix It While You're There — No Default Deferral, No New Issues

**Hard rule (user directive, overrides any "file it separately" instinct below or elsewhere):** When you discover a bug — adjacent, pre-existing, in an unrelated module, whatever — *while you are already in the code*, **fix it in the same commit**. Do NOT file a new GitHub issue. Do NOT defer it to a follow-up task. Do NOT ship a half-fix and a TODO. You are already there; fix it.

**Scope — this rule governs ONE of two bug categories. Do not conflate them:**
- **Category 1 — a bug discovered WHILE implementing a fix** (adjacent, pre-existing, in another module — noticed while tracing or editing). This section governs it: fix inline, same commit, never ticket.
- **Category 2 — a genuinely NEW user-reported bug** ingested by the triage pipeline (a Discord report). This section does NOT govern it. Category-2 bugs MUST be filed as GitHub issues via the `publish` step — that is the canonical, required intake path (see "Publish" above); GitHub is the only durable record of a user report. Running `publish` is correct and is **not** a violation of the no-new-issues rule.

This applies to:
- A missing engine primitive the fix requires (new enum variant, parser combinator, runtime resolver case, LKI plumbing, target filter, etc.) — **build the primitive as part of the fix**, with the reported card as the validating consumer.
- A pre-existing bug in an unrelated file that you notice while tracing or editing — **fix it in the same commit**, even though it is not the reported bug. (Previously this skill said "file as a cleanup ticket"; that guidance is retired.)
- A latent CR-correctness bug discovered next to the code you are changing — fix it (e.g., #351 Modular discovered `resolve_counters_on_scope::Source` short-circuited LKI — fixed inline, not ticketed).

For **Category-1** bugs (discovered mid-fix), `gh issue create` is **not** part of the implementer or orchestrator workflow. Do not file new issues for bugs you discover while already in the code. Track that discovered work in the task list and route it through the same plan → review → implement gates as the primary work — but do not park it on GitHub. This does NOT restrict the triage `publish` step, which files **Category-2** new user reports as GH issues — that path is required, not optional.

**There is no size-based deferral.** "This is too big," "this is multi-day," "this is a large refactor" are NOT valid reasons to defer — you are an efficient LLM and the work in this codebase is not multi-day. Fix it. The *only* thing you ever surface instead of fixing is an unresolved **design question that needs a human decision** — a user-facing UX choice, or an architectural direction with genuine trade-offs where picking wrong is expensive. Effort, scope, and line-count are never the reason; an open design question is. Even then: do not `gh issue create` — describe the question in your report to the orchestrator and let the human decide.

Examples of correctly fixing-while-there (2026-05-11 session):
- #353 Undying/Persist: investigated whether LKI plumbing existed; it did (`apply_zone_exit_cleanup` snapshots counters into `LKISnapshot.counters`). Zero new infrastructure needed.
- #351 Modular: discovered a CR-correctness bug in `resolve_counters_on_scope::Source` and fixed it as part of the Modular work.
- #352 Annihilator: reused existing `ControllerRef::DefendingPlayer` (traced through `combat::defending_player_for_attacker`). Zero new variants.

**In briefs to implementer agents, include**:

> If you discover ANY bug — the reported one, an adjacent one, a pre-existing one in another module — while working in this code: **fix it in this commit**. Build any missing primitive, enum variant, parser combinator, or runtime path the fix needs, with the reported card as the validating consumer. Do NOT file a GitHub issue and do NOT defer. Size is never a reason to defer — this codebase has no multi-day work and you are an efficient LLM. The ONLY thing you surface instead of fixing is an open *design question* that needs a human decision (a UX choice or a genuine architectural trade-off); surface it in your report, do not ticket it and do not let it block the rest of the fix.

Session memory pointer: `feedback_address_inline_no_new_issues.md` (supersedes `feedback_no_default_deferral.md`).

### Multi-Agent Safe Staging

When other `$engine-implementer` runs are active on shared files (especially `crates/engine/src/database/synthesis.rs`, `types/ability.rs`, parser modules), **never use `git add <file>` for surgical edits** — it sweeps any concurrent in-progress edits into your commit, polluting the audit trail.

Surgical staging options:

```bash
# Interactive hunk selection
git add -p crates/engine/src/database/synthesis.rs

# Non-interactive: write the patch and apply through the index
git diff crates/engine/src/database/synthesis.rs > /tmp/my-edit.patch
# (manually trim /tmp/my-edit.patch to only your hunks)
git apply --cached /tmp/my-edit.patch
```

If a `git add <file>` collision happens anyway:

1. Don't `git reset --hard` — preserves working-tree but reset can race with concurrent file writes
2. Do `git commit --amend -m "<honest message describing both swept-in changes>"` to update the commit narrative
3. SendMessage the other agent so it knows part of its work landed in your commit and to trust `git diff HEAD` for what remains to commit

Documented collision from 2026-05-11: a small Fabricate-timing comment annotation (#358) staged via `git add crates/engine/src/database/synthesis.rs` swept the #353 Undying/Persist agent's in-progress synthesis scaffold into the same commit. Recovery: amended commit message to honestly describe both changes; agent finished its remaining work (tests + registration) in a follow-up commit.

### Committing From a Shared Multi-Agent Checkout — Classify Every File

The hunk-collision case above is for small overlaps. When `$engine-implementer` runs for a long time on shared `main`, the working tree can contain your feature's files plus other agents' whole files. Isolate at the FILE level before committing:

1. Classify every modified file: `git diff -- <f> | grep -qE "<your feature identifiers + blast-radius marker, e.g. 'SearchPartition|split: None'>"`. Files containing a marker are yours (including blast-radius `field: None` edits); files without are foreign.
2. Spot-confirm the "blast-radius" files contain ONLY the field addition (every added line matches the marker — no foreign hunks rode along).
3. `git add <explicit file list>` (NEVER `-A`). Then assert isolation: `git diff --cached --name-only` — every staged file must carry a feature marker; zero foreign. Print the unstaged leftovers and confirm they are *exactly* the foreign set you identified.
4. **Commit via `git commit -F <msgfile>`**, never `-m` with a message containing embedded double-quotes — the shell breaks the quote and git mis-reads the words as pathspecs ("pathspec '…' did not match"). Write the message to a temp file first.
5. After commit: verify HEAD is attached to a branch (`git symbolic-ref --short HEAD`) and the foreign files remain dirty/uncommitted (you didn't sweep them).

### Implementer Crash / Sub-Agent Overload — Tilt Is the Source of Truth

If `$engine-implementer` crashes (API `529 Overloaded`) or returns no final report, do NOT blindly relaunch — it may have run on the shared `main` checkout and its edits may already be in the working tree. Recover by assessment, not re-execution:

1. `git status --short` + `git diff --stat` to see what landed.
2. Tilt resources are ground truth for completeness: `test-engine` / `card-data` / `check-frontend` green ⇒ the implementation compiles and its tests pass, regardless of whether the agent reported. A dead agent can't lie about test status; the green resource can't be faked. (You can then do the post-fix review yourself from the diff — you are the orchestrator, a legitimate non-implementer reviewer — when spawning a fresh reviewer also fails to overload.)
3. Read the LATEST Tilt build only — `tilt logs <r> --since Nm` mixes many churned builds; intermediate "could not compile" lines from while files were mid-edit are noise. Use `tilt get uiresource <r> -o jsonpath='{.status.updateStatus}'` for current status; diagnose only after `updateStatus == error` with no in-flight build.
4. **Foreign red doesn't block you**: if clippy/test errors are all in files/crates you didn't touch (e.g. pre-existing `phase-server/main.rs` lints), they're another agent's. Proceed on your-crate-green + dependency-order reasoning (clippy reaching a downstream crate proves the upstream crates it depends on passed). Don't fix foreign red; don't get stuck on it. Confirm a suspected-foreign file is unmodified-by-you with `git diff --stat HEAD -- <file>`.

### Sequential Implementation for Overlapping Plans

Two approved plans whose file sets overlap (shared `ability_utils.rs`, `oracle_effect/mod.rs`, `types/*`, phase-ai builders) must be implemented SEQUENTIALLY — commit the first before launching the second — to avoid edit collisions and doubled Tilt build contention. Only run implementers in parallel when their file sets are provably disjoint.

### GitHub Comment Standard

GitHub comments must be concise, user-facing status updates. Do not paste local command output, long command transcripts, local machine paths, target directories, or exhaustive verification command lists into issues. Summarize the evidence at the semantic level instead:
- Good: "Fixed in <commit>. The reported ability now parses as a typed ProduceMana replacement with a tapped-for-mana scope, and regression tests cover both multiplied and non-multiplied mana production."
- Bad: "Verification: `CARGO_TARGET_DIR=... cargo test ...`, `cargo run ...`, `git diff --check`" followed by command details or output.

Keep raw command details in the local working notes or final Codex response when useful, not in GitHub. For issue updates, mention only the commit, the reported behavior now covered, and whether targeted parser/runtime evidence exists.

## Status Lifecycle

```
needs-triage → confirmed → in-progress → fixed-unreleased → needs-runtime-verify → verified → closed
                         → stale → closed
                         → wont-fix → closed
                         → duplicate → closed
```

## Cluster Tracking with Sub-Issues

**Principle**: priority labels are perpetual buckets (queryable, auto-clean as issues close). Sub-issue trackers are *thematic workstreams with finite lifespans*. Trackers capture grouping rationale and ordering; they do NOT replace labels.

### Decision rule

Run at session end on newly filed issues, and at session start on the unclustered `status:confirmed` backlog (rate-limited: at most once per session).

1. Standalone issue? → labels only.
2. 2 related? → labels only; reassess at 3.
3. 3+ with a one-paragraph rationale **beyond what labels say** AND a finite end state? → tracker.
4. No finite end ("all P1 work", "all engine bugs")? → label query, not a tracker.

### When NOT to file (anti-patterns)

- **Singletons** — labels only.
- **Label-queryable groups** — `priority:p1-* + area:engine` is a CLI query, not a tracker.
- **Perpetual tier buckets** — NEVER invent `tier:1` / `tier:2` labels or "Tier N" parent issues. Tier is relative; trackers are durable; the mismatch creates name drift.
- **Cross-tracker membership** — one parent per child is an API constraint. Pick the dominant theme (see Tiebreaker below).
- **Invented label families** — NEVER invent `cluster:*`, `theme:*`, or other grouping labels. Structural label families are FIXED: `priority`, `area`, `mechanic`, `source`, `resolution`, `special`, `status`. Clustering is expressed through sub-issue parentage on a `collector`-labelled tracker, period.
- **Deferral mechanism** — filing a sub-issue under a tracker is NOT a substitute for fixing a bug while you are in the code (see *Fix It While You're There* above). Do not create new issues for discovered bugs — fix them in the same commit. Trackers are a read/organize view over issues that already exist, not a place to park discovered work.

### Tracker format

```
Title:  Cluster: <theme> (<scope>)
Label:  collector
Body uses fixed H2 headings (machine-extractable):

  ## Rationale   — 1 paragraph; why grouped beyond what labels already say
  ## Ordering    — 1 line per child if non-obvious
  ## Children    — auto-rendered by GitHub when sub-issues are attached
```

### Lifecycle commands

```bash
# Create a tracker
gh issue create --repo phase-rs/phase --label "collector" \
  --title "Cluster: <theme> (<scope>)" \
  --body "..."

# Attach a child — API requires the REST id integer, NOT the issue number, NOT the node_id.
# Use -F (typed field) — NOT -f (raw string). The -f form will fail with
#   "Invalid property /sub_issue_id: \"<id>\" is not of type `integer`" (HTTP 422)
CHILD_ID=$(gh api repos/phase-rs/phase/issues/<child_number> --jq .id)
gh api -X POST repos/phase-rs/phase/issues/<parent_number>/sub_issues \
  -F sub_issue_id=$CHILD_ID

# Inspect a tracker + children
gh issue view <parent_number> --repo phase-rs/phase --json subIssues,title,body
```

Reference: https://docs.github.com/en/rest/issues/sub-issues

### Session integration

**Session start**: list open trackers and work in this order:

1. Trackers with any `priority:p0-softlock` child
2. Then trackers older than 30 days (force-resolution pressure)
3. Then trackers with the fewest remaining open children (closeout bias)

After picking from open trackers, scan the unclustered `status:confirmed` backlog **once per session** for new thematic groupings of 3+ passing the decision rule. File retroactive trackers and attach matching issues. Do NOT re-scan on every tool call.

**Session end**: review newly filed issues. Any 3+ with a shared theme passing the decision rule → file tracker + attach children. Singletons stay unattached.

**Closure is MANUAL.** GitHub does NOT auto-close parents when sub-issues close. When the last active child closes, manually close the tracker with a brief retrospective comment summarizing the cluster outcome and any reusable primitives produced (e.g., "5 shipped, 1 RFC-deferred (#367). Reusable primitives: LKI snapshot for dies-trigger inspection, ChangeZone.enter_with_counters."). That comment IS the retrospective archive future agents will read.

**Dissolution**: keep a tracker open if any active child remains AND its rationale still applies. Do NOT close a tracker just because count is 1 — a tracker with a single open RFC child remains structurally useful as the cluster → follow-up link.

**Exhausted-cluster rule**: when all children are closed but more theme work is expected (e.g., Tier 1 keyword cluster closes and Tier 2 keywords are next), close the existing tracker with its retrospective comment and file a NEW tracker for the next batch. NEVER repurpose a closed-theme tracker as a perpetual queue — that violates the finite-end principle and merges history with active work.

**Split rule**: when a tracker grows past ~10 children with diverging themes, file two new trackers, reparent the children, and close the original with `resolution:split` and a comment pointing at the two replacements.

**Merge rule**: NEVER retroactively merge two open trackers. Each closed tracker is its retrospective archive. For genuinely converging themes, file a new forward-only tracker; cross-reference both originals in its Rationale.

**Cross-cluster tiebreaker** (when a child fits two themes): pick the tracker whose Rationale more specifically predicts the fix shape. A "build-time synthesis" tracker beats a generic "Π-round refactor" tracker for a keyword bug because the synthesis tracker scopes the fix. If genuinely co-equal, prefer the tracker closing sooner so the child doesn't outlive its parent.

### Worked example

**Cluster: Keyword Synthesis (Tier 1, May 2026)** — CLOSED. Children: #346, #351, #352, #353, #354, #355 (all closed; #355 spawned RFC #367).

```
## Rationale
Build-time synthesis pattern for highest-ROI keywords. Each child shares the
synthesis.rs entry point and primitives (LKI snapshot, ReplacementEvent::Moved
+ PutCounter, ChangeZone.enter_with_counters, ControllerRef::DefendingPlayer).
Cluster end: all keywords shipped or deferred to RFC.

## Ordering
Fabricate (baseline synthesis pattern) → dies-trigger family (Modular,
Undying/Persist, Bloodthirst) → per-attacker family (Annihilator) →
cross-cutting pair-binding (Soulbond, deferred to RFC #367).
```

**Cluster: Architectural Follow-ups from Keyword Synthesis** — OPEN. Open children: #357, #359, #364, #367. Closed: #358.

```
## Rationale
Cross-cutting follow-ups discovered during the Tier 1 keyword cluster: LKI
symmetry, CounterType Π-lift, end-to-end ETB-pipeline testing harness,
KeywordTriggerInstaller registry. Each is too cross-cutting to land inline
with the originating fix, but smaller than the RFC threshold. Cluster end:
all follow-ups landed or escalated.

## Ordering
#359 registry first (front-load — unblocks future keyword work), then #357
E2E test harness, then #364 Π-lift (waits for #359), then #367 RFC pickup.
```

### Cross-references

- `feedback_no_default_deferral.md` — trackers do not park in-scope work; build the primitive inline as part of the fix.
- GitHub sub-issues REST API: https://docs.github.com/en/rest/issues/sub-issues

## Resync Workflow (periodic maintenance)

Run this after parser/engine changes to update triage state:

### Step 1: Regenerate card data
```bash
./scripts/gen-card-data.sh
```

### Step 2: Re-run coverage cross-reference
Spawn a Sonnet agent to re-read `triage/llm-triage-items.jsonl` and cross-reference against the updated `client/public/card-data.json`. Write results to `triage/coverage-crossref.jsonl` and `triage/coverage-crossref-summary.md`.

### Step 3: Identify candidates for verification
Compare the new cross-reference against open GitHub issues. Parser coverage is only a candidate signal:
- If the bug was a parser gap → inspect the reported ability and verify the typed AST/IR represents the reported semantics. Close only after that targeted semantic check passes.
- If the bug was a runtime issue → do not mark fixed from parser coverage. Inspect the relevant runtime code and preferably add/run a reproduction test. Transition only after targeted evidence exists.

Also at this step: audit open `collector` trackers. When a resync pass closes children of an open tracker, evaluate the tracker against the dissolution / exhausted-cluster rules in *Cluster Tracking with Sub-Issues* and manually close or split as appropriate. Tracker state otherwise drifts: children close, parents stay open with no remaining work.

### Step 4: Fetch new Discord messages
```bash
bun scripts/sync-bug-reports.ts fetch
```
If new messages exist, re-run extract → triage → render. Then review **`triage/triage-delta.jsonl`** — and ONLY that file. It contains exactly the triage items from the latest fetch window (messages with `fetched_at > prev_fetch_at`). Do not re-process every historical Discord thread as new work, and do not hand-filter `triage-items.jsonl` by snowflake/timestamp guesses — that is how orphaned reports get missed. The raw store and dashboards regenerate from the full message archive for determinism, but GitHub issue work is delta-based:
- The `triage` command prints the delta breakdown + a **"reports to resolve" list**: every non-skip delta item. Each must be filed (`publish --thread=`), linked/deduped to an existing issue, or `mark-handled`. Never ignore one.
- Use Discord cursors in `triage/sync-state.json` and the `fetch` command's "New messages fetched" count to decide whether there is new Discord input.
- Treat `report_id` (`discord:<thread_id>:<message_id>:<item_index>`) as the stable idempotency key. The script does NOT dedup against GitHub — **you** are the arbiter. Before creating work, search GitHub issues/comments for that report id or thread/message URL.
- Your manual dedupe checks MUST include closed issues: use `--state all`, not `--state open`. Closed `status:fixed-unreleased`, `stale`, `duplicate`, and `wont-fix` issues are still authoritative triage records and must prevent duplicate creation.
- When you confirm a delta report already has a GitHub issue (open or closed), do not refile it — `mark-handled --notes="dup of #N"` (closed) or link/comment it (open). Recreate only if the Discord thread contains a newer unmatched `report_id`.
- Existing GitHub issues, comments, labels, and sub-issue parentage are the persistent triage state. Update those records instead of rediscovering or refiling old reports.
- If an old report appears in the regenerated dashboard but already has a GH issue/comment or a documented stale/duplicate decision, skip it unless the Discord thread has a newer message with a new `report_id`.

**Hard rule:** `parser_status: fully_parsed` is parser metadata only. It must never classify a user report as `likely_fixed`, stale, skipped, or ignorable. Runtime, frontend, AI, deckbuilder, multiplayer, and UI reports still require subsystem evidence or a GH issue even when all referenced cards are fully parsed.

### Step 5: Update dashboard
```bash
bun scripts/sync-bug-reports.ts render
```

## Oracle Text Sourcing — MANDATORY

**Every Oracle text reference in a GitHub issue, comment, or triage note MUST be copied verbatim from `client/public/card-data.json`.** Never quote Oracle text from memory, the user's Discord message, Scryfall, or training data. The card database is the only authoritative source — using anything else risks filing issues against the wrong card text and wasting fix cycles.

```bash
# REQUIRED before quoting Oracle text in any issue body or comment:
jq -r '.["card name"] | .oracle_text' client/public/card-data.json
```

If `oracle_text` is `null` or the card key is missing, do NOT guess — flag the card-data lookup failure in the issue and stop. A missing entry is itself a bug worth reporting (likely a card-data pipeline gap).

When filing or updating an issue, include an explicit **Oracle text (verified from `client/public/card-data.json`)** section quoting the text you looked up. This makes the verification visible to reviewers and prevents downstream agents from re-introducing wrong text.

If you discover an existing issue references wrong Oracle text, fix it as part of the next triage pass — wrong card text in an issue is worse than no quote, because it sends fixers chasing the wrong semantics.

## Investigating Whether a Bug Is Fixed

### Evidence Standard

User reports are presumed real unless there is strong contradictory evidence. Do not mark an issue `likely_fixed`, `fixed-unreleased`, `verified`, stale, skipped, or closed from parser coverage alone.

`fully_parsed` only means the parser did not emit `Unimplemented` or `Unknown`. It does not prove the card behaves correctly: text can be swallowed, parsed into overly generic effects, attached to the wrong subject/controller/zone, represented with the wrong typed semantics, or fail at runtime/UI/AI/deckbuilding. A fresh user report with `fully_parsed` cards should normally become `status:confirmed` or `status:needs-repro` unless there is targeted contradictory evidence.

Acceptable evidence depends on the report type:
- Parser-gap report: the specific reported Oracle clause parses into the expected typed AST/IR/effect, with correct subject, controller, target, zone, condition, quantity, and optional/otherwise wiring.
- Runtime/engine report: a targeted runtime code inspection or regression test proves the reported behavior is handled correctly.
- AI/frontend/deckbuilder report: inspect the subsystem that owns the behavior; card parser coverage is not evidence for these.

When evidence is weaker than this, keep or create the GitHub issue and label it `status:confirmed` or `status:needs-repro`. In notes, say what evidence is missing instead of calling it fixed.

Before calling any bug fixed, run the mandatory post-fix review gate above. Regressions discovered by review are part of the same bug-triage task and must be resolved before issue status changes.

### Already-Known Unimplemented — Dismiss Only When the Gap Explains the Report

A Discord report does NOT need a new GitHub issue when the card data ALREADY records the *exact reported behavior* as unimplemented. We already know about that gap; a new issue would just duplicate a known limitation. This is a `mark-handled` (with a note), not a `create_issue`.

**The gate is a behavior match, not a card match.** It is NOT enough that the card has *some* `Unimplemented` effect or `Unknown` trigger. A card can be half-parsed — one ability lowers to a typed effect while another falls back to `Unimplemented`. The reported symptom must be the specific thing the `Unimplemented` / `Unknown` marker covers. If the user reports the *typed* ability misbehaving, an `Unimplemented` marker on a *different* ability does not explain the report — file it.

Procedure:

1. Look up the card's unimplemented abilities/triggers:
   ```bash
   jq '.["card name"] | {abilities: [.abilities[]? | select(.effect.type == "Unimplemented")], triggers: [.triggers[]? | select(.mode == "Unknown")]}' client/public/card-data.json
   ```
2. Read the reported behavior from the Discord thread.
3. Dismiss (`mark-handled --thread=<id> --notes="known unimplemented: <ability>"`) ONLY if the reported behavior maps onto one of the `Unimplemented` effects / `Unknown` triggers above.
4. If the reported behavior is anything the marker does NOT cover — a different ability on the same card, a runtime crash, a *wrong result* on an ability that DID parse to a typed effect, or a UI/AI/deckbuilder problem — the known gap is irrelevant. File the issue.

This is the inverse of the `fully_parsed` hard rule: `fully_parsed` never proves a bug is fixed, and `has_gaps` only dismisses a report when the gap *is* the reported behavior.

### Parser-gap bugs (area:parser)
1. Check the card: `jq '.["card name"]' client/public/card-data.json`
2. Look for `Unimplemented` effects or `Unknown` triggers
3. Verify the specific ability mentioned in the bug has the expected typed semantics, not just a real effect type
4. If the ability is represented by `GenericEffect`, overly broad filters, wrong controller/target/zone, missing conditions, or swallowed clauses, the parser gap is still open

### Runtime/engine bugs (area:engine)
1. Read the bug description
2. Find the relevant handler in `crates/engine/src/game/effects/` or `crates/engine/src/game/`
3. Check if the described behavior is handled correctly, including the exact subject/controller/zone/timing from the report
4. Best: write a test that reproduces the bug scenario → if the test proves the reported bad behavior cannot occur, the bug is fixed

### AI bugs (area:ai)
1. Check `crates/phase-ai/` for the relevant evaluation/action-generation logic
2. AI bugs are rarely caught by parser coverage — they need gameplay testing

## Triage Data Files

All paths are relative to the **repo root** — the `triage/` directory is created
on first `fetch` (it does not exist in a fresh clone). The `fetch`/`extract`/`triage`
commands generate the `.jsonl` files; `triage/llm-triage-items.jsonl`,
`triage/p0-verification.md`, `triage/unknown-card-mapping.json`, and
`triage/no-card-bugs.md` are produced by LLM/manual passes, not the script.

| File | Description | Produced by | Gitignored |
|------|-------------|-------------|------------|
| `triage/raw/discord-messages.jsonl` | Raw Discord messages (775+) | `fetch` | yes |
| `triage/report-items.jsonl` | Heuristic-extracted report items | `extract` | yes |
| `triage/triage-items.jsonl` | Heuristic triage classifications | `triage` | yes |
| `triage/triage-delta.jsonl` | Triage items from the latest fetch window ONLY — the slice to review each cycle | `triage` / `delta` | yes |
| `triage/llm-triage-items.jsonl` | LLM (Sonnet) triage — 333 items, best quality | LLM pass | yes |
| `triage/coverage-crossref.jsonl` | Cross-reference against parser coverage | `crossref` | yes |
| `triage/coverage-crossref-summary.md` | Human-readable summary | LLM pass | yes |
| `triage/p0-verification.md` | Manual spot-check of P0 likely-fixed bugs | manual | yes |
| `triage/unknown-card-mapping.json` | Card name corrections | manual | yes |
| `triage/no-card-bugs.md` | Engine/UI bugs not tied to cards | manual | yes |
| `triage/threads-compact.json` | Compact thread data for LLM agent input | `fetch` | yes |
| `triage/sync-state.json` | Incremental fetch cursors + `published_threads` map | `fetch` / `publish` | yes |
| `triage/dashboard.md` | Generated dashboard | `render` | yes |
| `triage/triage-dashboard.md` | Triage-classified dashboard (only when triage data exists) | `render` | yes |

## Label Taxonomy

| Group | Labels | Purpose |
|-------|--------|---------|
| status | needs-triage, needs-repro, confirmed, in-progress, fixed-unreleased, needs-card-data-regen, needs-runtime-verify, verified, stale, duplicate, wont-fix | Lifecycle |
| area | engine, parser, frontend, ui, ai, card-data, deckbuilder, multiplayer, infra | Ownership |
| priority | p0-softlock, p1-core-mechanic, p1-infinite-loop, p2-wrong-game-result, p2-interaction, p3-card-specific, p3-edge-case | Urgency |
| mechanic | triggered-abilities, mana, combat, tokens, costs, zone-change, continuous-effects, keyword, replacement-effects, counters, layers, attachments, modal, search, card-data-regen, ai-policy, targeting | Subsystem |
| source | discord, github, playtesting | Provenance |
| resolution | split, merged, upstream, cant-reproduce, by-design | Closure reason |
| special | collector | Sub-issue tracker for a thematic cluster of 3+ related issues. Open trackers represent active workstreams; closed trackers are retrospective archive. See *Cluster Tracking with Sub-Issues* for the decision rule and lifecycle. |
