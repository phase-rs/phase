---
phase: 9
slug: wire-deckbuilder-game-engine
status: draft
nyquist_compliant: true
wave_0_complete: false
created: 2026-03-08
---

# Phase 9 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | vitest 3.x (client), cargo test (Rust) |
| **Config file** | `client/vitest.config.ts` |
| **Quick run command** | `cd client && npx vitest run --reporter=verbose` |
| **Full suite command** | `cd client && npx vitest run && cd ../crates/engine && cargo test` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && npx vitest run --reporter=verbose`
- **After every plan wave:** Run `cd client && npx vitest run && cd ../crates/engine && cargo test && cd ../crates/engine-wasm && cargo build -p engine-wasm --target wasm32-unknown-unknown`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | Status |
|---------|------|------|-------------|-----------|-------------------|--------|
| 09-01-T1 | 01 | 1 | PLAT-03, AI-04 | unit | `cd crates/engine && cargo test deck_loading -- --nocapture` | ⬜ pending |
| 09-01-T2 | 01 | 1 | PLAT-03 | build | `cargo build -p engine-wasm --target wasm32-unknown-unknown` | ⬜ pending |
| 09-02-T1 | 02 | 1 | DECK-01 | unit | `cd client && npx vitest run src/services/__tests__/deckParser.test.ts --reporter=verbose` | ⬜ pending |
| 09-02-T2 | 02 | 1 | DECK-01 | smoke | `cd client && npx tsx -e "import { STARTER_DECKS } from './src/data/starterDecks'; ..."` | ⬜ pending |
| 09-03-T1 | 03 | 2 | DECK-03, PLAT-03 | type+unit | `cd client && npx tsc --noEmit && npx vitest run --reporter=verbose` | ⬜ pending |
| 09-03-T2 | 03 | 2 | DECK-01, DECK-03 | type+unit | `cd client && npx tsc --noEmit && npx vitest run --reporter=verbose` | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Test Creation Ownership

Tests are created within the plan tasks that need them — no separate Wave 0 plan required:

| Test File | Creating Task | Notes |
|-----------|---------------|-------|
| `crates/engine/src/game/deck_loading.rs` (inline tests) | 09-01-T1 | Unit tests for create_object_from_card_face, load_deck_into_state |
| `client/src/services/__tests__/deckParser.test.ts` | 09-02-T1 | TDD task: tests written first, then implementation |
| Existing test suite | 09-03-T1, 09-03-T2 | Verify no regressions via `npx vitest run` |

---

## Requirement Coverage

| Requirement | Description | Covered By |
|-------------|-------------|------------|
| DECK-01 | Deck data key alignment, import/parse | 09-02-T1, 09-02-T2, 09-03-T2 |
| DECK-03 | End-to-end deck -> game flow | 09-03-T1, 09-03-T2 |
| AI-04 | Game mode parameter passing | 09-01-T1, 09-03-T1 |
| PLAT-03 | WASM deck loading, adapter chain | 09-01-T1, 09-01-T2, 09-03-T1 |

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Deck tiles render with color dots | DECK-01 | Visual styling | 1. Open MenuPage 2. Verify deck tiles show with WUBRG color dots |
| Full flow: select deck -> play game | DECK-03 | Cross-page navigation with WASM | 1. Select deck on MenuPage 2. Click "Play vs AI" 3. Verify game loads with cards in library |
| GamePage redirect without deck | DECK-03 | Navigation guard | 1. Clear localStorage 2. Navigate to /game 3. Verify redirect to / |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify commands
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Test creation folded into plan tasks (no orphaned Wave 0 references)
- [x] No watch-mode flags
- [x] Feedback latency < 30s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
