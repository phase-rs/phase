# Bug Triage System — Operator Reference

## Quick Commands

```bash
# Full pipeline (fetch new Discord messages → extract → triage → render)
bun scripts/sync-bug-reports.ts fetch
bun scripts/sync-bug-reports.ts extract
bun scripts/sync-bug-reports.ts triage     # also emits triage/triage-delta.jsonl
bun scripts/sync-bug-reports.ts render

# Review ONLY the delta — the reports new since the last fetch. NEVER scan the
# full triage-items.jsonl looking for "what's new"; that is how orphaned
# reports get missed. `triage` prints the delta + an orphan roll-call.
bun scripts/sync-bug-reports.ts delta      # re-emit delta without re-classifying

# Publish: create GH issues for `create_issue` triage items AND react 👀 +
# post a tracking link inside the originating Discord thread. Also reconciles
# threads whose triage item is already linked to an open issue (write-back
# only — no duplicate creation). Once-per-thread; mapping persisted in
# triage/sync-state.json under `published_threads`.
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

### Mandatory Post-Fix Review Gate — Isolated Reviewer Required

Every code fix made during bug triage must pass an **isolated reviewer agent's** application of `.claude/commands/review-impl.md` before the fix is committed, marked fixed, or described as complete.

**Self-review by the implementing agent is NOT sufficient.** Multiple commits during the 2026-05-11 bug-triage rounds passed implementer self-review but had real issues caught only by a fresh-context reviewer (CR hallucinations, tests bypassing the pipeline they claim to exercise, predicate-narrowness latent bugs, missing CR sub-parts that don't exist). Implementers rationalize their own choices; fresh-context reviewers do not.

How to apply:

```bash
# After the implementer ships a commit:
git log --oneline -1   # capture the SHA

# Spawn an isolated code-quality-reviewer agent (NOT the implementer) with:
#   - the commit SHA
#   - the review charter from .claude/commands/review-impl.md
#   - explicit "you have not seen the implementation" framing
```

The reviewer must read the diff (`git show <sha>`) with fresh context and apply the `/review-impl` checklist. Required focus areas:

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

**Specific anti-patterns to reject in review**:

- Helper functions that insert game-state values to satisfy a precondition the engine should have produced
- "Multiplayer" tests using a 2-player `GameState`
- Trigger tests calling `process_triggers(SyntheticEvent)` directly instead of producing the event via the game action that should emit it
- Replacement tests asserting the replacement's shape and assuming that proves the engine fires it
- LKI tests mutating the live counter map then asserting LKI reads it — proves the LKI cache reads from the live map, NOT that LKI captured pre-death state

When the pipeline-driving harness doesn't exist yet, **build it as part of the work** (per the No Default Deferral rule below). Cascade synthesis has such a harness; mirror it. Do not split "real tests" into a follow-up issue when the harness can be built in the same commit.

Session memory pointer: `feedback_runtime_tests_must_drive_pipeline.md`.

### No Default Deferral — Build the Missing Infrastructure

When a card bug requires a missing engine primitive (new enum variant, parser combinator, runtime resolver case, LKI plumbing, target filter, etc.), **build the primitive as part of the fix**. Use the reported card as the validating consumer. Do NOT file a deferred follow-up issue and ship a half-fix.

Deferral is reserved for genuinely massive work:
- Multi-day rewrites cross-cutting through stack / SBA / replacement pipelines
- Architectural primitives that need their own RFC (e.g., Soulbond pair-binding, DSK Rooms door-unlock)
- Work that requires user-facing UI design decisions

A few hundred LOC of typed plumbing in the engine crate is NOT deferral-worthy. Examples from the 2026-05-11 session where the agent (correctly) built infrastructure instead of deferring:

- #353 Undying/Persist: investigated whether LKI plumbing existed for dies-trigger counter inspection. It did (`apply_zone_exit_cleanup` snapshots counters into `LKISnapshot.counters`). Zero new infrastructure needed.
- #351 Modular: discovered `resolve_counters_on_scope::Source` had a CR-correctness bug (live-state short-circuit bypassing LKI). Fixed it as part of the Modular work rather than filing as a separate ticket.
- #352 Annihilator: needed "defending player for this attack" target wiring. Reused existing `ControllerRef::DefendingPlayer` (verified by tracing through `combat::defending_player_for_attacker`). Zero new variants.

What gets filed as a separate issue:
- Architectural design choices that affect multiple keywords/cards uniformly (e.g., #359 KeywordTriggerInstaller registry — affects all build-time-synthesized triggered keywords)
- Pre-existing bugs in unrelated files discovered during review (file as a cleanup ticket; don't expand the current commit's scope into other modules)
- Pi-round-class refactors lifting stringly-typed AST fields to typed enums (e.g., #364 CounterType Π-8 lift)

**In briefs to implementer agents, include**:

> If your work requires a missing primitive, enum variant, parser combinator, or runtime path: **build it as part of this commit**. Use the reported card as the validating consumer. Defer ONLY if the work is genuinely multi-day cross-cutting (and explain why in your report).

Session memory pointer: `feedback_no_default_deferral.md`.

### Multi-Agent Safe Staging

When other engine-implementer agents are running concurrently on shared files (especially `crates/engine/src/database/synthesis.rs`, `types/ability.rs`, parser modules), **never use `git add <file>` for surgical edits** — it sweeps any concurrent in-progress edits into your commit, polluting the audit trail.

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
- **Deferral mechanism** — filing a sub-issue under a tracker is NOT a substitute for building missing infrastructure during the originating fix (see *No Default Deferral* above). Trackers organize work that is legitimately separate (architectural follow-ups discovered post-commit, RFC-class items, Π-round refactors), not in-scope plumbing punted to "later."

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
- The `triage` command prints the delta breakdown + an **orphan roll-call**: delta items that are `primary_report`/`additional_report` with a non-skip action but no `github_issue`. An `append_to_existing` or `needs_human_review` item with no `github_issue` is a contradiction — it means an *unfiled* report. Every orphan in the delta must be filed (`publish --thread=`), deduped, or `mark-handled`. Never ignore one.
- Use Discord cursors in `triage/sync-state.json` and the `fetch` command's "New messages fetched" count to decide whether there is new Discord input.
- Treat `report_id` (`discord:<thread_id>:<message_id>:<item_index>`) as the stable idempotency key. Before creating work, search GitHub issues/comments for that report id or thread/message URL.
- GitHub dedupe checks MUST include closed issues: use `--state all`, not `--state open`. Closed `status:fixed-unreleased`, `stale`, `duplicate`, and `wont-fix` issues are still authoritative triage records and must prevent duplicate creation.
- The `triage` command performs a GitHub issue-index dedupe pass against all issues by exact Discord report id/source URL/message path. If it marks a report `skip_existing_closed`, do not recreate it unless the Discord thread contains a newer unmatched report id.
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

| File | Description | Gitignored |
|------|-------------|------------|
| `triage/raw/discord-messages.jsonl` | Raw Discord messages (775+) | yes |
| `triage/report-items.jsonl` | Heuristic-extracted report items | yes |
| `triage/triage-items.jsonl` | Heuristic triage classifications | yes |
| `triage/llm-triage-items.jsonl` | LLM (Sonnet) triage — 333 items, best quality | yes |
| `triage/triage-delta.jsonl` | Triage items from the latest fetch window ONLY — the slice to review each cycle | yes |
| `triage/coverage-crossref.jsonl` | Cross-reference against parser coverage | yes |
| `triage/coverage-crossref-summary.md` | Human-readable summary | yes |
| `triage/p0-verification.md` | Manual spot-check of P0 likely-fixed bugs | yes |
| `triage/unknown-card-mapping.json` | Card name corrections | yes |
| `triage/no-card-bugs.md` | Engine/UI bugs not tied to cards | yes |
| `triage/threads-compact.json` | Compact thread data for LLM agent input | yes |
| `triage/sync-state.json` | Incremental fetch cursors | yes |
| `triage/dashboard.md` | Generated dashboard | yes |

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
