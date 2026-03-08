---
phase: 9
slug: wire-deckbuilder-game-engine
status: draft
nyquist_compliant: false
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
| **Full suite command** | `cd client && npx vitest run && cd ../crates && cargo test` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && npx vitest run --reporter=verbose`
- **After every plan wave:** Run `cd client && npx vitest run && cd ../crates/engine && cargo test && cd ../crates/engine-wasm && cargo test`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 09-01-01 | 01 | 1 | DECK-01 | unit | `cd client && npx vitest run src/services/__tests__/deckParser.test.ts -x` | ✅ (needs MTGA tests) | ⬜ pending |
| 09-01-02 | 01 | 1 | DECK-01 | unit | `cd client && npx vitest run src/data/__tests__/starterDecks.test.ts -x` | ❌ W0 | ⬜ pending |
| 09-02-01 | 02 | 1 | DECK-03 | unit | `cd client && npx vitest run src/components/deck-builder/__tests__/ManaCurve.test.ts -x` | ❌ W0 | ⬜ pending |
| 09-03-01 | 03 | 1 | AI-04 | integration | `cd crates && cargo test -p engine-wasm -- initialize_game` | ❌ W0 | ⬜ pending |
| 09-04-01 | 04 | 1 | PLAT-03 | unit | `cd client && npx vitest run src/stores/__tests__/gameStore.test.ts -x` | ✅ (needs deck data tests) | ⬜ pending |
| 09-04-02 | 04 | 1 | PLAT-03 | unit | `cd client && npx vitest run src/constants/__tests__/storage.test.ts -x` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/services/__tests__/deckParser.test.ts` — add MTGA format parse tests
- [ ] `client/src/data/__tests__/starterDecks.test.ts` — validate starter deck structure
- [ ] `crates/engine/src/game/deck_loading.rs` + tests — GameObject hydration from card data
- [ ] `crates/engine-wasm/src/lib.rs` tests — initialize_game with deck data
- [ ] `client/src/constants/__tests__/storage.test.ts` — shared storage key constants

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| DeckBuilder → Start Game navigates correctly | DECK-03 | Browser navigation with sessionStorage | 1. Build deck 2. Click "Start Game" 3. Verify /game?mode=ai loads 4. Verify deck appears in game |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
