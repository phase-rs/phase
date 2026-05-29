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
