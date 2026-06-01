# `contribute-card` Workflow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `.claude/workflows/contribute-card.js`, a Workflow orchestration script that automates `docs/AI-CONTRIBUTOR.md` end-to-end (select card → plan → review → implement → review → independent cross-check → verify → open PR), looping over a batch.

**Architecture:** Approach A — the workflow script is the orchestrator. It calls the leaf skills (`engine-planner`, `review-engine-plan`, `review-impl`) directly as its own `agent()` steps rather than wrapping `engine-implementer` (which would require nested agent-spawning the harness restricts). Cards are processed sequentially in a `for...of` loop with per-card error isolation; each card gets its own collision-guarded `card/<slug>` branch and PR.

**Tech Stack:** Claude Code Workflow tool (plain ESM JavaScript, no TypeScript), `agent()` with JSON-Schema structured output, `engine-planner` / `review-engine-plan` / `review-impl` skills, `gh` CLI, the repo's Tilt-aware verification scripts.

---

## File Structure

| File | Responsibility |
|------|----------------|
| Create: `.claude/workflows/contribute-card.js` | The entire workflow: `meta`, constants, schemas, prompt builders, per-card pipeline helpers, and the main batch loop. Single self-contained file — Workflow scripts are not split across modules. |

The spec for this work is `docs/superpowers/specs/2026-05-28-contribute-card-workflow-design.md`. Re-read it if any task is ambiguous.

### Validation method (used by every task)

Workflow scripts use ESM `export` syntax but are parsed by the Workflow tool, not run by `node` directly. To validate syntax cheaply after each task **without** executing any agents or opening PRs, copy to a temporary `.mjs` file and run `node --check`:

```bash
cp .claude/workflows/contribute-card.js /tmp/_cc_check.mjs && node --check /tmp/_cc_check.mjs && echo "SYNTAX OK"; rm -f /tmp/_cc_check.mjs
```

A file containing only `export const meta = {...}` plus constant/function declarations and no top-level executing body is a valid no-op workflow, so intermediate tasks pass this check.

Function declarations hoist and are only called at runtime, so a helper that references a not-yet-written helper still passes `node --check` (syntax-only). The real end-to-end validation (a live invocation) is the final task and is run deliberately by the user.

---

## Task 1: Scaffold — `meta`, constants, and `normalizeArgs`

**Files:**
- Create: `.claude/workflows/contribute-card.js`

- [ ] **Step 1: Create the file with the meta block, constants, and arg normalizer**

```javascript
export const meta = {
  name: 'contribute-card',
  description: 'Implement MTG card(s) end-to-end and open a PR, per docs/AI-CONTRIBUTOR.md',
  whenToUse: 'Maintainer-facing automation of the AI-CONTRIBUTOR card pipeline: pick → implement → review → verify → PR, batched over N cards.',
  phases: [
    { title: 'Select', detail: 'resolve explicit card arg or auto-pick low-gap unsupported cards' },
    { title: 'Plan', detail: 'engine-planner + review-engine-plan loop' },
    { title: 'Implement', detail: 'branch + implement the card on the AI-CONTRIBUTOR §4 prompt' },
    { title: 'Review', detail: 'review-impl loop + independent fresh-context cross-check' },
    { title: 'Verify', detail: 'fmt, combinator gate, clippy/test/card-data, coverage, semantic-audit' },
    { title: 'PR', detail: 'commit, push, open PR with the §7 body template' },
  ],
}

// Maintainer runs Opus (Frontier). Per spec "Tier handling" we assume Frontier
// and always run Gate A in verify; we do not attempt model self-detection.
const TIER = 'Frontier'

// Published coverage endpoint (AI-CONTRIBUTOR.md §3).
const COVERAGE_URL = 'https://pub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev/staging/coverage-data.json'

const MAX_PLAN_REVIEW_ROUNDS = 3
const MAX_IMPL_REVIEW_ROUNDS = 3
const MAX_VERIFY_RETRIES = 2

// args may be a bare card-name string or { card?, count? }.
function normalizeArgs(a) {
  if (typeof a === 'string') {
    const c = a.trim()
    return { explicitCard: c || null, count: 1 }
  }
  if (a && typeof a === 'object') {
    const explicitCard =
      typeof a.card === 'string' && a.card.trim() ? a.card.trim() : null
    const count = explicitCard
      ? 1
      : Number.isInteger(a.count) && a.count > 0
        ? a.count
        : 1
    return { explicitCard, count }
  }
  return { explicitCard: null, count: 1 }
}
```

- [ ] **Step 2: Validate syntax**

Run:
```bash
cp .claude/workflows/contribute-card.js /tmp/_cc_check.mjs && node --check /tmp/_cc_check.mjs && echo "SYNTAX OK"; rm -f /tmp/_cc_check.mjs
```
Expected: `SYNTAX OK`

- [ ] **Step 3: Commit**

```bash
git add .claude/workflows/contribute-card.js
git commit -m "feat(workflow): scaffold contribute-card meta + arg normalizer"
```

---

## Task 2: Add the JSON-Schema definitions

**Files:**
- Modify: `.claude/workflows/contribute-card.js` (append after `normalizeArgs`)

- [ ] **Step 1: Append all structured-output schemas**

```javascript
const WORKLIST_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['cards'],
  properties: {
    cards: {
      type: 'array',
      items: { type: 'string' },
      description: 'Ordered card names to implement, exactly as they appear in coverage data',
    },
  },
}

const BRANCH_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['branch'],
  properties: {
    branch: { type: 'string', description: 'The exact git branch name created (card/<slug>[-N])' },
  },
}

const REVIEW_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['clean', 'findings'],
  properties: {
    clean: { type: 'boolean', description: 'true when there are no blocking findings' },
    findings: { type: 'array', items: { type: 'string' } },
  },
}

const IMPL_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['scopeExpansion', 'filesChanged', 'crReferences'],
  properties: {
    scopeExpansion: { type: 'string', description: 'Description of scope growth, or the literal "None."' },
    filesChanged: { type: 'array', items: { type: 'string' } },
    crReferences: { type: 'array', items: { type: 'string' }, description: 'CR XXX.Y annotations added or touched' },
  },
}

const CROSSCHECK_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['clean', 'findings'],
  properties: {
    clean: { type: 'boolean' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['category', 'detail'],
        properties: {
          category: {
            type: 'string',
            enum: ['nom-mandate', 'cr-citation', 'pattern-coverage', 'logic-placement', 'building-block-reuse', 'bool-flag'],
          },
          location: { type: 'string', description: 'file:line if known' },
          detail: { type: 'string' },
        },
      },
    },
  },
}

const VERIFY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['passed', 'commands', 'failures'],
  properties: {
    passed: { type: 'boolean' },
    commands: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['name', 'status'],
        properties: { name: { type: 'string' }, status: { type: 'string' } },
      },
    },
    coverageSupported: { type: 'boolean', description: 'card now supported:true gap_count:0' },
    semanticAuditClean: { type: 'boolean' },
    failures: { type: 'array', items: { type: 'string' } },
  },
}

const PR_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['opened'],
  properties: {
    opened: { type: 'boolean' },
    prUrl: { type: 'string' },
  },
}
```

- [ ] **Step 2: Validate syntax**

Run:
```bash
cp .claude/workflows/contribute-card.js /tmp/_cc_check.mjs && node --check /tmp/_cc_check.mjs && echo "SYNTAX OK"; rm -f /tmp/_cc_check.mjs
```
Expected: `SYNTAX OK`

- [ ] **Step 3: Commit**

```bash
git add .claude/workflows/contribute-card.js
git commit -m "feat(workflow): add contribute-card structured-output schemas"
```

---

## Task 3: Add the prompt builders

**Files:**
- Modify: `.claude/workflows/contribute-card.js` (append after the schemas)

These functions return the exact prompt strings each agent runs. The implement
prompt is the AI-CONTRIBUTOR.md §4 prompt verbatim; the cross-check prompt is
the §5 check (a)–(f); the verify prompt is the §6 Developer-track command
sequence; the PR prompt is the §7 body template.

- [ ] **Step 1: Append the prompt builders**

```javascript
function planPrompt(card) {
  return (
    `Use the \`engine-planner\` skill to produce an architecturally idiomatic ` +
    `implementation plan for full engine support of the Magic card "${card}". ` +
    `Design for the class of cards, not the single card. Return the full plan text.`
  )
}

function implementPrompt(card, plan) {
  return (
    `Implement full engine support for the card "${card}". Follow \`CLAUDE.md\` and ` +
    `\`AGENTS.md\` design principles without exception: build for the class not the ` +
    `card, nom combinators on first pass, CR annotations verified against ` +
    `\`docs/MagicCompRules.txt\` (and for each cited rule, also read its adjacent ` +
    `rules in the same section — cite the *authorizing* rule for the effect, not ` +
    `just the *layering* rule), idiomatic Rust, engine owns all logic, frontend is ` +
    `display-only. Reuse existing building blocks before writing new ones. Do not ` +
    `ask for clarification — on any ambiguity, take the architecturally idiomatic ` +
    `path. If scope expands beyond a single effect (e.g. the card requires new ` +
    `infrastructure, a new keyword, a new replacement pipeline), proceed anyway and ` +
    `explicitly note the scope expansion under a heading "Scope Expansion".\n\n` +
    `You are implementing on a branch that already exists; do NOT commit — leave ` +
    `changes in the working tree for review.\n\n` +
    `Set scopeExpansion to a one-line description if scope grew, else the literal ` +
    `"None.". List filesChanged (paths only) and crReferences (CR XXX.Y).\n\n` +
    `APPROVED PLAN:\n${plan}`
  )
}

function reviewPlanPrompt(card, plan) {
  return (
    `Use the \`review-engine-plan\` skill to review this implementation plan for the ` +
    `card "${card}". Set clean=true only if there are no blocking architectural ` +
    `findings. List each finding as a concrete string.\n\nPLAN:\n${plan}`
  )
}

function replanPrompt(card, plan, findings) {
  return (
    `Revise the implementation plan for "${card}" to address these review ` +
    `findings. Return the full revised plan text.\n\nFINDINGS:\n` +
    findings.map((f) => `- ${f}`).join('\n') +
    `\n\nCURRENT PLAN:\n${plan}`
  )
}

function reviewImplPrompt(card) {
  return (
    `Use the \`review-impl\` skill against the current uncommitted working-tree diff ` +
    `for the card "${card}". Set clean=true only if there are no defects, gaps, or ` +
    `missing cases. List each finding as a concrete string with file:line.`
  )
}

function fixImplPrompt(card, findings) {
  return (
    `Address every one of these \`review-impl\` findings for "${card}" with code ` +
    `changes in the working tree. Do not commit.\n\nFINDINGS:\n` +
    findings.map((f) => `- ${f}`).join('\n')
  )
}

function crossCheckPrompt(card) {
  return (
    `You are an INDEPENDENT reviewer with fresh context. You are given ONLY the ` +
    `unified diff (\`git diff\`), \`CLAUDE.md\`, and the skills under \`.claude/skills/\`. ` +
    `Ignore any prior conversation. Review the uncommitted change for the card ` +
    `"${card}" and check ALL of:\n` +
    `(a) nom-mandate compliance — flag any \`match\` over a stringified parser-text ` +
    `variable with string-literal arms, any chained \`if let Ok(..) = tag(..)\` ` +
    `blocks, and any string-method dispatch (.contains, .find, .split_once, ` +
    `.starts_with);\n` +
    `(b) CR-citation completeness — for each cited rule, did the implementation ` +
    `also cite the *authorizing* rule, not just the *layering* rule?\n` +
    `(c) pattern coverage — does this work for >=10 cards or just one?\n` +
    `(d) logic placement — engine vs frontend per CLAUDE.md;\n` +
    `(e) building-block reuse — did it duplicate logic an existing helper ` +
    `(oracle_util.rs, oracle_quantity.rs, game/filter.rs, game/zones.rs, etc.) ` +
    `already provides?\n` +
    `(f) bool-flag avoidance — any new bool field/param where a typed enum ` +
    `(ControllerRef, Comparator, Option<T>) fits better.\n` +
    `Set clean=true only if NONE of (a)-(f) produced a finding. Categorize each ` +
    `finding.`
  )
}

function fixCrossCheckPrompt(card, findings) {
  return (
    `A fresh-context reviewer found these issues in the "${card}" change. Fix each ` +
    `with code in the working tree. Do not commit.\n\nFINDINGS:\n` +
    findings
      .map((f) => `- [${f.category}] ${f.location || ''} ${f.detail}`)
      .join('\n')
  )
}

function verifyPrompt(card) {
  return (
    `Run Developer-track verification for the card "${card}" in this exact order, ` +
    `fixing in-loop on failure (max ${MAX_VERIFY_RETRIES} retries per command) ` +
    `before continuing:\n` +
    `1. cargo fmt --all   (always direct)\n` +
    `2. ./scripts/check-parser-combinators.sh   (Gate A; one-shot, direct)\n` +
    `3. If \`tilt get uiresource clippy >/dev/null 2>&1\` succeeds: ` +
    `./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data ; else: ` +
    `cargo clippy-strict && cargo test -p engine && ./scripts/gen-card-data.sh\n` +
    `4. cargo coverage   (confirm "${card}" is now supported:true, gap_count:0 -> ` +
    `set coverageSupported)\n` +
    `5. cargo semantic-audit   (confirm "${card}" has 0 findings -> set ` +
    `semanticAuditClean)\n` +
    `Set passed=true only if every command is clean AND coverageSupported AND ` +
    `semanticAuditClean. Record each command's status; list any unresolved ` +
    `failures in failures[].`
  )
}

function prPrompt(card, { impl, verify, partial }) {
  const verifyLines = (verify.commands || [])
    .map((c) => `  - \`${c.name}\` — ${c.status}`)
    .join('\n')
  const title = partial ? `Partial: ${card}` : `Add ${card}`
  const body =
    `## Summary\nAdds engine support for **${card}**.\n\n` +
    `## Files changed\n` +
    (impl.filesChanged || []).map((f) => `- ${f}`).join('\n') +
    `\n\n## CR references\n` +
    (impl.crReferences || []).map((c) => `- ${c}`).join('\n') +
    `\n\n## Track\nDeveloper\n\n` +
    `## LLM\nModel: claude-opus-4-8\nThinking: high\n\n` +
    `Tier: ${TIER}\n\n` +
    `## Verification\n${verifyLines}\n\n` +
    `## Scope Expansion\n${impl.scopeExpansion || 'None.'}\n\n` +
    `## Validation Failures\n${partial ? 'See review/cross-check notes below.' : 'None.'}\n\n` +
    `## CI Failures\n${(verify.failures && verify.failures.length) ? verify.failures.map((f) => `- ${f}`).join('\n') : 'None.'}\n`
  return (
    `Commit the working-tree change for "${card}", push the branch, and open a PR. ` +
    `Run:\n` +
    `git add -A && git commit -m ${JSON.stringify(`${title}`)} && git push -u origin HEAD\n` +
    `Then: gh pr create --title ${JSON.stringify(title)} --body <BODY>  ` +
    `(do NOT pass --label; the upstream auto-labeler handles it).\n\n` +
    `Use exactly this PR body:\n\n${body}\n\n` +
    `Return opened=true and the prUrl on success.`
  )
}
```

- [ ] **Step 2: Validate syntax**

Run:
```bash
cp .claude/workflows/contribute-card.js /tmp/_cc_check.mjs && node --check /tmp/_cc_check.mjs && echo "SYNTAX OK"; rm -f /tmp/_cc_check.mjs
```
Expected: `SYNTAX OK`

- [ ] **Step 3: Commit**

```bash
git add .claude/workflows/contribute-card.js
git commit -m "feat(workflow): add contribute-card prompt builders"
```

---

## Task 4: Add the per-card pipeline helpers

**Files:**
- Modify: `.claude/workflows/contribute-card.js` (append after the prompt builders)

Each helper is one stage of the per-card pipeline. They use the schemas from
Task 2 and prompts from Task 3.

- [ ] **Step 1: Append the pipeline helpers**

```javascript
async function selectCards({ explicitCard, count }) {
  if (explicitCard) return [explicitCard]
  const res = await agent(
    `WebFetch ${COVERAGE_URL} and return the ${count} best card(s) to implement. ` +
      `Selection criteria: supported == false, smallest gap_count (prefer 1-3), and ` +
      `NOT depending on deferred infrastructure (Rooms, Enchant Player, Suspend, ` +
      `Aggression). Return exactly ${count} card name(s) as they appear in the data.`,
    { label: 'select-cards', phase: 'Select', schema: WORKLIST_SCHEMA },
  )
  return (res && res.cards ? res.cards : []).slice(0, count)
}

async function createBranch(card) {
  const res = await agent(
    `Create a git branch for implementing the card "${card}". Build ` +
      `slug="card/<lowercase-hyphenated-card-name>". Collision guard: if that branch ` +
      `exists locally (git rev-parse --verify) or on origin (git ls-remote --exit-code ` +
      `origin), append -2, -3, ... until free. Then: git checkout -b "$slug". Return ` +
      `the exact branch name created.`,
    { label: `branch:${card}`, phase: 'Implement', schema: BRANCH_SCHEMA },
  )
  return res && res.branch ? res.branch : null
}

async function planCard(card) {
  let plan = await agent(planPrompt(card), { label: `plan:${card}`, phase: 'Plan' })
  for (let round = 1; round <= MAX_PLAN_REVIEW_ROUNDS; round++) {
    const review = await agent(reviewPlanPrompt(card, plan), {
      label: `review-plan:${card}#${round}`,
      phase: 'Plan',
      schema: REVIEW_SCHEMA,
    })
    if (review.clean) break
    plan = await agent(replanPrompt(card, plan, review.findings), {
      label: `replan:${card}#${round}`,
      phase: 'Plan',
    })
  }
  return plan
}

async function implementCard(card, plan) {
  return await agent(implementPrompt(card, plan), {
    label: `implement:${card}`,
    phase: 'Implement',
    schema: IMPL_SCHEMA,
  })
}

async function reviewImpl(card) {
  for (let round = 1; round <= MAX_IMPL_REVIEW_ROUNDS; round++) {
    const review = await agent(reviewImplPrompt(card), {
      label: `review-impl:${card}#${round}`,
      phase: 'Review',
      schema: REVIEW_SCHEMA,
    })
    if (review.clean) return true
    await agent(fixImplPrompt(card, review.findings), {
      label: `fix-impl:${card}#${round}`,
      phase: 'Review',
    })
  }
  return false
}

async function crossCheck(card) {
  const res = await agent(crossCheckPrompt(card), {
    label: `crosscheck:${card}`,
    phase: 'Review',
    schema: CROSSCHECK_SCHEMA,
  })
  if (!res.clean && res.findings && res.findings.length) {
    await agent(fixCrossCheckPrompt(card, res.findings), {
      label: `fix-crosscheck:${card}`,
      phase: 'Review',
    })
  }
  return res
}

async function verifyCard(card) {
  return await agent(verifyPrompt(card), {
    label: `verify:${card}`,
    phase: 'Verify',
    schema: VERIFY_SCHEMA,
  })
}

async function openPr(card, ctx) {
  return await agent(prPrompt(card, ctx), {
    label: `pr:${card}`,
    phase: 'PR',
    schema: PR_SCHEMA,
  })
}
```

- [ ] **Step 2: Validate syntax**

Run:
```bash
cp .claude/workflows/contribute-card.js /tmp/_cc_check.mjs && node --check /tmp/_cc_check.mjs && echo "SYNTAX OK"; rm -f /tmp/_cc_check.mjs
```
Expected: `SYNTAX OK`

- [ ] **Step 3: Commit**

```bash
git add .claude/workflows/contribute-card.js
git commit -m "feat(workflow): add contribute-card per-card pipeline helpers"
```

---

## Task 5: Add the main batch loop

**Files:**
- Modify: `.claude/workflows/contribute-card.js` (append at the end — this is the executing body)

Sequential `for...of` (shared parser files / single working tree / one-branch-per-PR). Per-card `try/catch` isolates failures so the batch continues.

- [ ] **Step 1: Append the main loop**

```javascript
phase('Select')
const { explicitCard, count } = normalizeArgs(args)
const cards = await selectCards({ explicitCard, count })
log(`Work-list (${cards.length}): ${cards.join(', ') || '(none)'}`)

const summary = []
for (const card of cards) {
  try {
    const branch = await createBranch(card)
    const plan = await planCard(card)
    const impl = await implementCard(card, plan)
    const implReviewClean = await reviewImpl(card)
    const cross = await crossCheck(card)
    const verify = await verifyCard(card)
    const partial = !implReviewClean || !cross.clean || !verify.passed
    const pr = await openPr(card, { impl, verify, partial })
    const status = partial ? 'partial' : 'success'
    summary.push({
      card,
      branch: branch,
      prUrl: pr && pr.prUrl ? pr.prUrl : null,
      status,
    })
    log(`${card}: ${status}${pr && pr.prUrl ? ' -> ' + pr.prUrl : ''}`)
  } catch (e) {
    summary.push({ card, branch: null, prUrl: null, status: 'aborted' })
    log(`${card}: aborted -- ${e && e.message ? e.message : 'error'}`)
  }
}

return summary
```

- [ ] **Step 2: Validate syntax**

Run:
```bash
cp .claude/workflows/contribute-card.js /tmp/_cc_check.mjs && node --check /tmp/_cc_check.mjs && echo "SYNTAX OK"; rm -f /tmp/_cc_check.mjs
```
Expected: `SYNTAX OK`

- [ ] **Step 3: Commit**

```bash
git add .claude/workflows/contribute-card.js
git commit -m "feat(workflow): wire contribute-card main batch loop"
```

---

## Task 6: End-to-end live validation (user-run) + final commit

This task is run **deliberately by the user/maintainer** because it executes real agents, mutates the working tree, and opens a real PR. Do not run it automatically inside an unattended session.

- [ ] **Step 1: Confirm the workflow is discoverable and parses**

The Workflow tool validates the `meta` literal and JS syntax at invocation. A no-op confirmation is the explicit-card path on an already-supported card with `count: 1`; abort the run (Ctrl-C / TaskStop) after you observe the Select phase resolve and the Plan phase begin — this confirms wiring without completing a PR.

Invoke via the Workflow tool:
```
Workflow({ name: "contribute-card", args: { card: "lightning bolt" } })
```
Expected: the `/workflows` view shows phases `Select → Plan → Implement → Review → Verify → PR`, the Select phase resolves the work-list to `lightning bolt`, and the Plan phase starts.

- [ ] **Step 2: One real auto-pick run**

```
Workflow({ name: "contribute-card", args: { count: 1 } })
```
Expected: Select fetches coverage data and picks one low-gap unsupported card; the pipeline runs to completion; the return value is a one-element array `[{ card, branch, prUrl, status }]` with `status` `success` or `partial` (and a real PR URL, or a recorded reason if `partial`/`aborted`).

- [ ] **Step 3: Final commit (if any tweaks were needed during validation)**

```bash
git add .claude/workflows/contribute-card.js
git commit -m "feat(workflow): finalize contribute-card after live validation" || echo "nothing to commit"
```

---

## Self-Review

**Spec coverage:**
- Form = Workflow script under `.claude/workflows/` — Task 1 (file creation). ✓
- Approach A (leaf skills as agents, no `engine-implementer` wrap) — Tasks 3–4 call `engine-planner`/`review-engine-plan`/`review-impl` directly. ✓
- `args` = string | `{card?, count?}` — `normalizeArgs`, Task 1. ✓
- Phase 0 select (explicit or coverage auto-pick, deferred-infra skip) — `selectCards`, Task 4. ✓
- Sequential per-card loop with error isolation — Task 5 `for...of` + `try/catch`. ✓
- Collision-guarded branch — `createBranch`, Task 4. ✓
- Plan + review-plan loop (max 3) — `planCard`, Task 4. ✓
- Implement on AI-CONTRIBUTOR §4 prompt verbatim — `implementPrompt`, Task 3. ✓
- review-impl loop (max 3) — `reviewImpl`, Task 4. ✓
- §5 fresh-context cross-check checks (a)–(f) — `crossCheckPrompt`, Task 3. ✓
- Verify §6 command sequence + tilt-aware gate — `verifyPrompt`, Task 3. ✓
- PR §7 body template (Tier/Model/Verification/None. defaults, no --label) — `prPrompt`, Task 3. ✓
- Partial vs Add title logic — Task 5 `partial` flag → `prPrompt`. ✓
- Return `{card,branch,prUrl,status}[]` — Task 5. ✓
- Tier Frontier assumed, Gate A always runs — `TIER` const (Task 1) + `verifyPrompt` step 2 (Task 3). ✓
- Out of scope (no mtgish, no AI-CONTRIBUTOR.md edits, no parallel, no model self-detection) — honored; no task touches those. ✓

**Placeholder scan:** No TBD/TODO; every code step contains complete code; validation commands have explicit expected output. ✓

**Type/name consistency:** Schema names (`WORKLIST_SCHEMA`, `BRANCH_SCHEMA`, `REVIEW_SCHEMA`, `IMPL_SCHEMA`, `CROSSCHECK_SCHEMA`, `VERIFY_SCHEMA`, `PR_SCHEMA`) defined in Task 2 are used in Task 4. Prompt builders (`planPrompt`, `implementPrompt`, `reviewPlanPrompt`, `replanPrompt`, `reviewImplPrompt`, `fixImplPrompt`, `crossCheckPrompt`, `fixCrossCheckPrompt`, `verifyPrompt`, `prPrompt`) defined in Task 3 are used in Task 4. Helpers (`selectCards`, `createBranch`, `planCard`, `implementCard`, `reviewImpl`, `crossCheck`, `verifyCard`, `openPr`) defined in Task 4 are called in Task 5. `normalizeArgs` (Task 1) called in Task 5. Constants (`TIER`, `COVERAGE_URL`, `MAX_*`) defined in Task 1, used in Tasks 3–4. All references resolve. ✓
