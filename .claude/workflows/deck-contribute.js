export const meta = {
  name: 'deck-contribute',
  description:
    'Triage a deck, group unsupported cards by missing mechanic, and open one PR per mechanic-cluster plus one PR per misparsed/one-off card',
  whenToUse:
    'Maintainer-facing. Point at a decklist / file path / Moxfield-Archidekt URL. Classifies every card (coverage + cargo semantic-audit + per-card LLM parse-fidelity audit), groups unsupported cards by the missing mechanic so a mechanic is built ONCE for its whole class, and (only when limit>0) implements each unit via an independent fork PR off upstream/main. Supported-but-misparsed cards and one-off gaps get one card per PR (the review tooling expects that). MUST run from a session whose cwd is the phase-card-runs worktree.',
  phases: [
    { title: 'Parse', detail: 'extract unique non-basic card names from the deck input' },
    { title: 'Classify', detail: 'gen card-data + cargo semantic-audit → unsupported / audit-flagged / clean' },
    { title: 'Audit', detail: 'per supported-clean card, LLM-compare parsed AST vs Oracle text' },
    { title: 'Cluster', detail: 'group unsupported by missing mechanic; supported-fixes stay per-card; skip in-flight PRs' },
    { title: 'Implement', detail: 'one PR per mechanic-cluster (class-level) + one PR per one-off/misparse card' },
  ],
}

// Per-card pipeline is reused from contribute-card.js (sits beside this file in
// the run-dir worktree). Mechanic clusters use the inline class-level pipeline
// below, because contribute-card is single-card / single-PR by construction.
const CONTRIBUTE_CARD = '.claude/workflows/contribute-card.js'

const TIER = 'Frontier'
// The mechanic-cluster pipeline below EMBODIES THE /engine-implementer SKILL CONTRACT
// (maintainer feedback on PR #3163): /engine-planner -> /review-engine-plan (looped until clean)
// -> engine-implementation-executor agent -> /review-impl (looped until clean; the reviewer must
// confirm the cards actually parse correctly, not just that the diff looks clean). "Two rounds and
// ship" is NOT acceptable, so the review caps below are runaway-loop safeguards (hitting one marks
// the unit `partial` and is surfaced, never silently shipped), not a ship-after-N gate.
const MAX_PLAN_REVIEW_ROUNDS = 8
const MAX_IMPL_REVIEW_ROUNDS = 8
const MAX_CROSSCHECK_ROUNDS = 2
const MAX_VERIFY_RETRIES = 2

// args: a bare deck string, OR { deck, limit?, baseBranch? }.
//   deck       — decklist text | local path | Moxfield/Archidekt URL
//   limit      — 0 (default) = triage only; N>0 = open up to N PRs (clusters first, then singletons)
//   baseBranch — disposable base branch reset to upstream/main before each unit (default "card-runs")
function normalizeArgs(a) {
  const base = { deck: '', limit: 0, baseBranch: 'card-runs' }
  if (typeof a === 'string') return { ...base, deck: a }
  if (a && typeof a === 'object') {
    return {
      deck: typeof a.deck === 'string' ? a.deck : '',
      limit: Number.isInteger(a.limit) && a.limit > 0 ? a.limit : 0,
      baseBranch:
        typeof a.baseBranch === 'string' && a.baseBranch.trim() ? a.baseBranch.trim() : 'card-runs',
    }
  }
  return base
}

const DECK_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['cards'],
  properties: {
    cards: {
      type: 'array',
      items: { type: 'string' },
      description: 'Unique card names, quantities/set-codes stripped, basic lands excluded',
    },
  },
}

const CLASSIFY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['unsupported', 'supportedFlagged', 'supportedClean'],
  properties: {
    unsupported: { type: 'array', items: { type: 'string' } },
    supportedFlagged: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['card', 'findings'],
        properties: {
          card: { type: 'string' },
          findings: { type: 'array', items: { type: 'string' } },
        },
      },
    },
    supportedClean: { type: 'array', items: { type: 'string' } },
  },
}

const AUDIT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['card', 'correct'],
  properties: {
    card: { type: 'string' },
    correct: { type: 'boolean', description: 'true if the parsed AST faithfully matches the Oracle text' },
    issue: { type: 'string', description: 'one-line description of the misparse when correct=false' },
  },
}

// Splits the unsupported set into mechanic clusters vs one-off cards.
const CLUSTER_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['mechanicClusters', 'oneOffs'],
  properties: {
    mechanicClusters: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['mechanic', 'cards'],
        properties: {
          mechanic: { type: 'string', description: 'the shared missing primitive, e.g. "Suspend"' },
          cards: { type: 'array', items: { type: 'string' }, description: 'cards unlocked by this mechanic' },
          heterogeneous: { type: 'boolean', description: 'true if these cards share only a card-TYPE (e.g. several Sagas, or several "companion" creatures) with NO single reusable mechanic — each card has a DIFFERENT effect. Such a cluster is built as per-card dispatch onto EXISTING handlers in one PR, not one new primitive. Omit/false for a true shared-mechanic cluster.' },
          note: { type: 'string' },
        },
      },
    },
    oneOffs: {
      type: 'array',
      items: { type: 'string' },
      description: 'unsupported cards whose gap is card-specific, not a reusable mechanic',
    },
    skipped: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['mechanic', 'reason'],
        properties: { mechanic: { type: 'string' }, reason: { type: 'string' } },
      },
      description: 'mechanics skipped because a PR is already open / in flight upstream',
    },
  },
}

const REVIEW_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['clean', 'findings'],
  properties: {
    clean: { type: 'boolean' },
    findings: { type: 'array', items: { type: 'string' } },
  },
}

const IMPL_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['scopeExpansion', 'filesChanged', 'crReferences'],
  properties: {
    scopeExpansion: { type: 'string' },
    filesChanged: { type: 'array', items: { type: 'string' } },
    crReferences: { type: 'array', items: { type: 'string' } },
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
          location: { type: 'string' },
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
    cardsSupported: { type: 'array', items: { type: 'string' }, description: 'cluster cards now supported:true gap:0' },
    semanticAuditClean: { type: 'boolean' },
    failures: { type: 'array', items: { type: 'string' } },
  },
}

const PR_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['opened'],
  properties: { opened: { type: 'boolean' }, prUrl: { type: 'string' } },
}

const BRANCH_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['branch'],
  properties: { branch: { type: 'string' } },
}

// ---- triage prompts ----

function parsePrompt(deck) {
  return (
    `You are given a Magic: The Gathering deck. It may be a raw decklist (lines ` +
    `like "1 Sol Ring"), a local file path, or a Moxfield/Archidekt URL.\n` +
    `Resolve it to the list of unique card names:\n` +
    `- URL: WebFetch it (or its export/JSON endpoint) and extract the mainboard ` +
    `AND the commander(s).\n` +
    `- File path: read the file.\n` +
    `- Raw text: parse the lines.\n` +
    `Strip leading quantities and any "(SET) 123" collector suffixes. Keep the ` +
    `full printed name for DFC/split cards (front // back). EXCLUDE basic lands: ` +
    `Plains, Island, Swamp, Mountain, Forest, Wastes, and Snow-Covered variants. ` +
    `Return unique names.\n\nDECK INPUT:\n${deck}`
  )
}

function classifyPrompt(cards) {
  return (
    `Classify these deck cards by Phase engine support, in the current worktree.\n` +
    `1. If client/public/card-data.json is missing, run ./scripts/gen-card-data.sh ` +
    `(cold build — expected).\n` +
    `2. Run \`cargo semantic-audit\` once; it writes data/semantic-audit.json with ` +
    `mechanical misparse findings across SUPPORTED cards.\n` +
    `3. Bucket each card (lowercase key into card-data.json):\n` +
    `   - UNSUPPORTED: key absent, OR any effect type "Unimplemented", OR any ` +
    `trigger mode "Unknown", OR any keyword "Unknown" (scan abilities, ` +
    `sub_ability, else_ability, mode_abilities, triggers.execute).\n` +
    `   - SUPPORTED-FLAGGED: supported but present in data/semantic-audit.json — ` +
    `include the finding category strings.\n` +
    `   - SUPPORTED-CLEAN: supported and not flagged.\n` +
    `Use the EXACT names given. Do not implement anything.\n\n` +
    `CARDS:\n${cards.map((c) => `- ${c}`).join('\n')}`
  )
}

function auditPrompt(card) {
  return (
    `Independent parse-fidelity check for the Phase engine — fresh context.\n` +
    `For "${card}": read its client/public/card-data.json entry (lowercase key): ` +
    `oracle_text and the full parsed tree. Compare AST to Oracle text ` +
    `clause-by-clause. The card is already "supported", so do NOT report missing ` +
    `implementation — report ONLY a SEMANTIC mismatch: wrong effect type, wrong ` +
    `target/scope, wrong amount, wrong color/cost, dropped condition/duration, or ` +
    `a clause that parsed to a plausible-but-different meaning. Set correct=false ` +
    `with a one-line issue ONLY for a real mismatch; when unsure prefer ` +
    `correct=true. Return {card, correct, issue?}.`
  )
}

function clusterPrompt(unsupported) {
  return (
    `Group these UNSUPPORTED Phase cards by the missing engine primitive so each ` +
    `primitive can be built ONCE for its whole class (CLAUDE.md: "build for the ` +
    `class, not the card").\n` +
    `For each card, read its card-data.json entry + Oracle text and identify the ` +
    `PRIMARY reason it is unsupported — usually a missing keyword/mechanic ` +
    `(Suspend, Bestow, Cascade, Madness, ...), a missing shared effect, or a ` +
    `missing replacement/trigger primitive. Cross-check data/engine-inventory.json ` +
    `(gitignored — run \`cargo engine-inventory\` to (re)generate it locally first) ` +
    `to name the gap precisely.\n` +
    `Then:\n` +
    `- mechanicClusters: cards that share the SAME missing reusable mechanic ` +
    `(2+ cards, or a single card whose gap is clearly a reusable mechanic worth a ` +
    `dedicated PR). One entry per mechanic with all its cards.\n` +
    `  HETEROGENEOUS TYPE-CLUSTERS: if 2+ cards share only a card-TYPE (e.g. several ` +
    `Sagas, or several "companion" legendary creatures) but each has a DIFFERENT ` +
    `effect — i.e. there is NO single reusable mechanic — still emit them as ONE ` +
    `mechanicCluster with heterogeneous:true and a mechanic label like ` +
    `"Saga chapter bodies". One PR built as per-card dispatch onto EXISTING handlers ` +
    `is more efficient than N one-off PRs and the class-level planner would otherwise ` +
    `fail hunting for a unifying variant that does not exist. Set heterogeneous:false ` +
    `(or omit) for a true shared-mechanic cluster.\n` +
    `- oneOffs: unsupported cards whose gap is card-specific (no reusable mechanic ` +
    `shared with others AND no sibling cards of the same type to batch) — one-card-per-PR.\n` +
    `- skipped: before emitting a mechanic, check \`gh pr list --repo ` +
    `phase-rs/phase --state open --search "<mechanic>"\`; if a PR already builds ` +
    `that mechanic, move it to skipped with the reason (avoid duplicate work).\n` +
    `Order mechanicClusters foundational-first (a mechanic other cards build on ` +
    `comes earlier). Use the EXACT card names given.\n\n` +
    `UNSUPPORTED CARDS:\n${unsupported.map((c) => `- ${c}`).join('\n')}`
  )
}

function resetPrompt(baseBranch) {
  return (
    `Prepare a clean, independent base for the next PR in this disposable run ` +
    `worktree. Run: git fetch upstream main --quiet && git checkout -B ` +
    `${baseBranch} upstream/main\n` +
    `If that fails because a failed prior unit left tracked modifications, run ` +
    `\`git checkout -- .\` and retry. Do NOT \`git stash\` and do NOT \`git clean\` ` +
    `(it would delete the untracked workflow files in .claude/workflows/). Confirm ` +
    `HEAD is at upstream/main with a clean tree.`
  )
}

// ---- mechanic-cluster (class-level) pipeline prompts ----

function clusterBranchPrompt(mechanic) {
  return (
    `Create a git branch for implementing the "${mechanic}" mechanic for its whole ` +
    `class. slug="mech/" + lowercase-hyphenated "${mechanic}". Collision guard: if ` +
    `that branch exists locally (git rev-parse --verify) or on origin (git ` +
    `ls-remote --exit-code origin), append -2, -3, ... until free. Then git ` +
    `checkout -b "$slug". Return the exact branch name created.`
  )
}

function clusterPlanPrompt(mechanic, cards, heterogeneous) {
  if (heterogeneous) {
    return (
      `Use the \`engine-planner\` skill to produce an architecturally idiomatic plan ` +
      `for the "${mechanic}" cluster. These cards share only a card-TYPE, NOT a ` +
      `reusable mechanic — each has a DIFFERENT effect. Do NOT hunt for one unifying ` +
      `variant that does not exist. "Build for the class" here means: dispatch EACH ` +
      `card's composed effect onto EXISTING handlers (ChangeZone, CreateToken, ` +
      `AddCounter, Destroy, Investigate, Goad, etc.). Plan each card INDEPENDENTLY as ` +
      `parser-dispatch-onto-existing-primitives; if a card genuinely needs a NEW ` +
      `primitive, plan the rest and DEFER that one (note it). A review-clean plan ` +
      `covers every card this way. Cards:\n${cards.map((c) => `- ${c}`).join('\n')}\n` +
      `Return the full plan text.`
    )
  }
  return (
    `Use the \`engine-planner\` skill to produce an architecturally idiomatic plan ` +
    `to implement the "${mechanic}" mechanic in the Phase engine, FOR THE CLASS ` +
    `(not any single card). The plan must make ALL of these cards fully ` +
    `supported:\n${cards.map((c) => `- ${c}`).join('\n')}\n` +
    `Design the reusable primitive (keyword/effect/replacement/trigger) and the ` +
    `parser support so future cards with the same mechanic work for free. Return ` +
    `the full plan text.`
  )
}

function reviewPlanPrompt(label, plan) {
  return (
    `Use the \`review-engine-plan\` skill to review this implementation plan for ` +
    `${label}. Set clean=true only if there are no blocking architectural ` +
    `findings. List each finding as a concrete string.\n\nPLAN:\n${plan}`
  )
}

function replanPrompt(label, plan, findings) {
  return (
    `Revise the implementation plan for ${label} to address these review ` +
    `findings. Return the full revised plan text.\n\nFINDINGS:\n` +
    findings.map((f) => `- ${f}`).join('\n') +
    `\n\nCURRENT PLAN:\n${plan}`
  )
}

function clusterImplementPrompt(mechanic, cards, plan, heterogeneous) {
  const classLine = heterogeneous
    ? `This is a HETEROGENEOUS TYPE-CLUSTER: the cards share only a card-type, not a mechanic. "Build for the class" means dispatch EACH card's effect onto EXISTING handlers (defer any card needing a genuinely NEW primitive, noting it in scopeExpansion) — do NOT force one new variant. `
    : `build for the class not the card, `
  return (
    `Implement the "${mechanic}" ${heterogeneous ? 'cluster' : 'mechanic'} in the Phase engine. ` +
    classLine +
    `Follow CLAUDE.md and AGENTS.md without exception: nom combinators on first pass, CR annotations verified against ` +
    `docs/MagicCompRules.txt (cite the AUTHORIZING rule, not just the layering ` +
    `rule), idiomatic Rust, engine owns all logic, frontend display-only, reuse ` +
    `existing building blocks. The change must make ALL of these cards fully ` +
    `supported:\n${cards.map((c) => `- ${c}`).join('\n')}\n` +
    `Do not ask for clarification — take the architecturally idiomatic path. You ` +
    `are on a branch that already exists; do NOT commit — leave changes in the ` +
    `working tree for review. Set scopeExpansion to a one-line note if scope grew ` +
    `(else "None."), list filesChanged (paths) and crReferences (CR XXX.Y).\n\n` +
    `APPROVED PLAN:\n${plan}`
  )
}

function reviewImplPrompt(label) {
  return (
    `Use the \`review-impl\` skill against the current uncommitted working-tree ` +
    `diff for ${label}. Set clean=true only if there are no defects, gaps, or ` +
    `missing cases. CRITICAL (engine-implementer feedback_review_impl_verify_bug_fixed): ` +
    `you MUST confirm the targeted behavior is ACTUALLY fixed via a discriminating runtime ` +
    `check — regenerate the affected cards' parse (oracle-gen data --filter / cargo coverage) ` +
    `and verify each is now supported with the correct AST — not merely that the code looks ` +
    `clean. If any card is still wrong, clean=false. List each finding as a concrete string with file:line.`
  )
}

function fixImplPrompt(label, findings) {
  return (
    `Address every one of these review-impl findings for ${label} with code ` +
    `changes in the working tree. Do not commit.\n\nFINDINGS:\n` +
    findings.map((f) => `- ${f}`).join('\n')
  )
}

function crossCheckPrompt(label) {
  return (
    `You are an INDEPENDENT reviewer with fresh context. You are given ONLY the ` +
    `unified diff (git diff), CLAUDE.md, and .claude/skills/. Ignore any prior ` +
    `conversation. Review the uncommitted change for ${label} and check ALL of:\n` +
    `(a) nom-mandate — flag any match over stringified parser text with literal ` +
    `arms, chained if let Ok(..)=tag(..), or string-method dispatch ` +
    `(.contains/.find/.split_once/.starts_with);\n` +
    `(b) CR-citation — did it cite the authorizing rule, not just the layering ` +
    `rule?\n(c) pattern coverage — does this work for the whole class, >=10 cards?\n` +
    `(d) logic placement — engine vs frontend;\n(e) building-block reuse;\n` +
    `(f) bool-flag avoidance. Set clean=true only if NONE of (a)-(f) produced a ` +
    `finding. Categorize each finding.`
  )
}

function fixCrossCheckPrompt(label, findings) {
  return (
    `A fresh-context reviewer found these issues in ${label}. Fix each with code ` +
    `in the working tree. Do not commit.\n\nFINDINGS:\n` +
    findings.map((f) => `- [${f.category}] ${f.location || ''} ${f.detail}`).join('\n')
  )
}

function clusterVerifyPrompt(mechanic, cards) {
  return (
    `Run Developer-track verification for the "${mechanic}" mechanic in this exact ` +
    `order, fixing in-loop on failure (max ${MAX_VERIFY_RETRIES} retries per ` +
    `command):\n` +
    `1. cargo fmt --all\n` +
    `2. ./scripts/check-parser-combinators.sh (Gate A)\n` +
    `3. If \`tilt get uiresource clippy >/dev/null 2>&1\` succeeds: ` +
    `./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data ; else ` +
    `cargo clippy-strict && cargo test -p engine && ./scripts/gen-card-data.sh\n` +
    `4. cargo coverage — confirm EACH of these cards is now supported:true gap:0; ` +
    `list the ones that are in cardsSupported:\n${cards.map((c) => `- ${c}`).join('\n')}\n` +
    `5. cargo semantic-audit — confirm none of these cards has findings -> ` +
    `semanticAuditClean.\n` +
    `passed=true only if every command is clean AND every listed card is in ` +
    `cardsSupported AND semanticAuditClean. Record each command status; list ` +
    `unresolved failures.`
  )
}

function clusterPrPrompt(mechanic, cards, { impl, verify, partial }) {
  const verifyLines = (verify.commands || []).map((c) => `  - \`${c.name}\` — ${c.status}`).join('\n')
  const title = partial ? `Partial: Add ${mechanic} mechanic` : `Add ${mechanic} mechanic (${cards.length} cards)`
  const body =
    `## Summary\nAdds engine support for the **${mechanic}** mechanic, unlocking ` +
    `${cards.length} card(s) in this deck.\n\n## Cards unlocked\n` +
    cards.map((c) => `- ${c}`).join('\n') +
    `\n\n## Files changed\n` +
    (impl.filesChanged || []).map((f) => `- ${f}`).join('\n') +
    `\n\n## CR references\n` +
    (impl.crReferences || []).map((c) => `- ${c}`).join('\n') +
    `\n\n## Track\nDeveloper\n\n## LLM\nModel: claude-opus-4-8\nThinking: high\n\n` +
    `Tier: ${TIER}\n\n## Verification\n${verifyLines}\n\n## Scope Expansion\n` +
    `${impl.scopeExpansion || 'None.'}\n\n## Validation Failures\n` +
    `${partial ? 'See review/cross-check notes.' : 'None.'}\n\n## CI Failures\n` +
    `${verify.failures && verify.failures.length ? verify.failures.map((f) => `- ${f}`).join('\n') : 'None.'}\n`
  return (
    `Commit the working-tree change for the "${mechanic}" mechanic, push the ` +
    `branch to your fork, and open a PR to phase-rs/phase with base main.\nRun:\n` +
    `git add -A && git commit -m ${JSON.stringify(title)} && git push -u origin HEAD\n` +
    `Then: gh pr create --base main --title ${JSON.stringify(title)} --body <BODY> ` +
    `(do NOT pass --label; the upstream auto-labeler handles it).\n\n` +
    `Use exactly this PR body:\n\n${body}\n\nReturn opened=true and the prUrl.`
  )
}

// ---- mechanic-cluster pipeline ----

async function implementMechanicCluster(mechanic, cards, heterogeneous) {
  const branch = await agent(clusterBranchPrompt(mechanic), {
    label: `branch:${mechanic}`,
    phase: 'Implement',
    schema: BRANCH_SCHEMA,
  })
  const label = `the "${mechanic}" mechanic`

  // Step 1-2: /engine-planner -> /review-engine-plan, looped until a full round is clean.
  let plan = await agent(clusterPlanPrompt(mechanic, cards, heterogeneous), { label: `plan:${mechanic}`, phase: 'Implement' })
  let planReviewClean = false
  for (let r = 1; r <= MAX_PLAN_REVIEW_ROUNDS && !planReviewClean; r++) {
    const review = await agent(reviewPlanPrompt(label, plan), { label: `review-plan:${mechanic}#${r}`, phase: 'Implement', schema: REVIEW_SCHEMA })
    if (review.clean) { planReviewClean = true; break }
    plan = await agent(replanPrompt(label, plan, review.findings), { label: `replan:${mechanic}#${r}`, phase: 'Implement' })
  }

  // Step 3: engine-implementation-executor agent performs the surgical edits.
  const impl = await agent(clusterImplementPrompt(mechanic, cards, plan, heterogeneous), { label: `implement:${mechanic}`, phase: 'Implement', schema: IMPL_SCHEMA, agentType: 'engine-implementation-executor' })

  // Step 5: /review-impl, looped until clean; fixes applied by a fresh engine-implementation-executor.
  let implReviewClean = false
  for (let r = 1; r <= MAX_IMPL_REVIEW_ROUNDS && !implReviewClean; r++) {
    const review = await agent(reviewImplPrompt(label), { label: `review-impl:${mechanic}#${r}`, phase: 'Implement', schema: REVIEW_SCHEMA })
    if (review.clean) { implReviewClean = true; break }
    await agent(fixImplPrompt(label, review.findings), { label: `fix-impl:${mechanic}#${r}`, phase: 'Implement', agentType: 'engine-implementation-executor' })
  }

  let cross = await agent(crossCheckPrompt(label), { label: `crosscheck:${mechanic}`, phase: 'Implement', schema: CROSSCHECK_SCHEMA })
  for (let r = 1; r <= MAX_CROSSCHECK_ROUNDS && !cross.clean && cross.findings && cross.findings.length; r++) {
    await agent(fixCrossCheckPrompt(label, cross.findings), { label: `fix-crosscheck:${mechanic}#${r}`, phase: 'Implement' })
    cross = await agent(crossCheckPrompt(label), { label: `recheck:${mechanic}#${r}`, phase: 'Implement', schema: CROSSCHECK_SCHEMA })
  }

  const verify = await agent(clusterVerifyPrompt(mechanic, cards), { label: `verify:${mechanic}`, phase: 'Implement', schema: VERIFY_SCHEMA })
  const partial = !planReviewClean || !implReviewClean || !cross.clean || !verify.passed
  const pr = await agent(clusterPrPrompt(mechanic, cards, { impl, verify, partial }), { label: `pr:${mechanic}`, phase: 'Implement', schema: PR_SCHEMA })

  return {
    unit: `mechanic:${mechanic}`,
    cards,
    branch: branch && branch.branch ? branch.branch : null,
    prUrl: pr && pr.prUrl ? pr.prUrl : null,
    status: partial ? 'partial' : 'success',
  }
}

// ---- main ----

phase('Parse')
const { deck, limit, baseBranch } = normalizeArgs(args)
if (!deck) {
  log('No deck provided. Pass args: "<decklist | path | url>" or { deck, limit }.')
  return { error: 'no-deck' }
}
const parsed = await agent(parsePrompt(deck), { label: 'parse-deck', phase: 'Parse', schema: DECK_SCHEMA })
const cards = parsed && parsed.cards ? parsed.cards : []
log(`Deck: ${cards.length} non-basic card(s)`)
if (!cards.length) return { cards: [], units: [] }

phase('Classify')
const cls = await agent(classifyPrompt(cards), { label: 'classify', phase: 'Classify', schema: CLASSIFY_SCHEMA })
const unsupported = cls.unsupported || []
const flagged = cls.supportedFlagged || []
const clean = cls.supportedClean || []
log(`unsupported=${unsupported.length}  audit-flagged=${flagged.length}  clean=${clean.length}`)

phase('Audit')
const verdicts = await parallel(
  clean.map((card) => () => agent(auditPrompt(card), { label: `audit:${card}`, phase: 'Audit', schema: AUDIT_SCHEMA })),
)
const misparsed = verdicts.filter(Boolean).filter((v) => v.correct === false)
log(`LLM audit: ${misparsed.length} supported-but-wrong of ${clean.length} clean`)

// Supported fixes are ALWAYS one card per PR (review tooling expects that).
// Dedupe flagged ∪ misparsed by lowercased name.
const fixSeen = new Set()
const supportedFixes = []
for (const item of [
  ...flagged.map((f) => ({ card: f.card, reason: `audit-flagged: ${(f.findings || []).join(', ')}` })),
  ...misparsed.map((v) => ({ card: v.card, reason: `misparse: ${v.issue || 'semantic mismatch'}` })),
]) {
  const k = item.card.toLowerCase()
  if (fixSeen.has(k)) continue
  fixSeen.add(k)
  supportedFixes.push(item)
}

phase('Cluster')
let mechanicClusters = []
let oneOffs = []
let skipped = []
if (unsupported.length) {
  const clusters = await agent(clusterPrompt(unsupported), { label: 'cluster', phase: 'Cluster', schema: CLUSTER_SCHEMA })
  mechanicClusters = (clusters.mechanicClusters || []).filter((c) => c.cards && c.cards.length)
  oneOffs = clusters.oneOffs || []
  skipped = clusters.skipped || []
}
log(`mechanic-clusters=${mechanicClusters.length}  unsupported-one-offs=${oneOffs.length}  supported-fixes=${supportedFixes.length}  skipped=${skipped.length}`)
mechanicClusters.forEach((c) => log(`  [mechanic${c.heterogeneous ? '·hetero' : ''}] ${c.mechanic}: ${c.cards.length} card(s) — ${c.cards.join(', ')}`))
oneOffs.forEach((c) => log(`  [one-off] ${c}`))
supportedFixes.forEach((f) => log(`  [fix] ${f.card} (${f.reason})`))
skipped.forEach((s) => log(`  [skipped] ${s.mechanic} — ${s.reason}`))

// Ordered work units: mechanic clusters first (foundational, unlock the most),
// then unsupported one-offs, then supported per-card fixes.
const units = [
  ...mechanicClusters.map((c) => ({ kind: 'mechanic', mechanic: c.mechanic, cards: c.cards, heterogeneous: !!c.heterogeneous })),
  ...oneOffs.map((card) => ({ kind: 'card', card, reason: 'unsupported one-off' })),
  ...supportedFixes.map((f) => ({ kind: 'card', card: f.card, reason: f.reason })),
]

const triage = {
  deckSize: cards.length,
  mechanicClusters,
  oneOffs,
  supportedFixes,
  skipped,
  units: units.length,
}

if (limit <= 0) {
  log('Triage-only (limit=0). Re-run with { deck, limit: N } to open the first N PRs (clusters first).')
  return triage
}

phase('Implement')
const toRun = units.slice(0, limit)
log(`Implementing ${toRun.length} of ${units.length} unit(s) (limit=${limit})`)
const results = []
for (const unit of toRun) {
  const name = unit.kind === 'mechanic' ? `mechanic:${unit.mechanic}` : `card:${unit.card}`
  try {
    await agent(resetPrompt(baseBranch), { label: `reset:${name}`, phase: 'Implement' })
    if (unit.kind === 'mechanic') {
      const r = await implementMechanicCluster(unit.mechanic, unit.cards, unit.heterogeneous)
      results.push(r)
      log(`${name}: ${r.status}${r.prUrl ? ' -> ' + r.prUrl : ''}`)
    } else {
      const summary = await workflow({ scriptPath: CONTRIBUTE_CARD }, unit.card)
      const entry = Array.isArray(summary) && summary[0] ? summary[0] : { card: unit.card, status: 'unknown' }
      results.push({ unit: name, ...entry, reason: unit.reason })
      log(`${name}: ${entry.status}${entry.prUrl ? ' -> ' + entry.prUrl : ''}`)
    }
  } catch (e) {
    results.push({ unit: name, status: 'aborted' })
    log(`${name}: aborted -- ${e && e.message ? e.message : 'error'}`)
  }
}

return { ...triage, results }
