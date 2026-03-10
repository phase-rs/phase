---
phase: 13
slug: foundation-board-layout
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-08
---

# Phase 13 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Vitest 3.x + @testing-library/react 16.x |
| **Config file** | `client/vitest.config.ts` |
| **Quick run command** | `cd client && pnpm test -- --run` |
| **Full suite command** | `cd client && pnpm test -- --run --coverage` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run`
- **After every plan wave:** Run `cd client && pnpm test -- --run --coverage`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 13-01-01 | 01 | 1 | INTEG-02 | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/cardProps.test.ts` | ❌ W0 | ⬜ pending |
| 13-01-02 | 01 | 1 | BOARD-01 | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/cardSizing.test.ts` | ❌ W0 | ⬜ pending |
| 13-01-03 | 01 | 1 | BOARD-02, BOARD-03, BOARD-04 | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/battlefieldGrouping.test.ts` | ❌ W0 | ⬜ pending |
| 13-01-04 | 01 | 1 | BOARD-07, BOARD-08 | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/ptDisplay.test.ts` | ❌ W0 | ⬜ pending |
| 13-01-05 | 01 | 1 | BOARD-09 | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/dominantColor.test.ts` | ❌ W0 | ⬜ pending |
| 13-01-06 | 01 | 1 | BOARD-05 | unit | `cd client && pnpm test -- --run src/components/card/__tests__/CardImage.test.tsx` | ❌ W0 | ⬜ pending |
| 13-02-01 | 02 | 1 | HAND-03 | unit | `cd client && pnpm test -- --run src/components/hand/__tests__/PlayerHand.test.tsx` | ❌ W0 | ⬜ pending |
| 13-02-02 | 02 | 1 | HUD-03 | unit | `cd client && pnpm test -- --run src/components/hud/__tests__/LifeTotal.test.tsx` | ❌ W0 | ⬜ pending |
| 13-02-03 | 02 | 1 | LOG-02, LOG-03 | unit | `cd client && pnpm test -- --run src/viewmodel/__tests__/logFormatting.test.ts` | ❌ W0 | ⬜ pending |
| 13-02-04 | 02 | 1 | INTEG-03 | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | ❌ W0 | ⬜ pending |
| 13-03-01 | 03 | 2 | HAND-01, HAND-02 | manual-only | Visual inspection + interaction | N/A | ⬜ pending |
| 13-03-02 | 03 | 2 | HAND-04 | manual-only | Visual inspection | N/A | ⬜ pending |
| 13-03-03 | 03 | 2 | BOARD-06 | manual-only | Visual inspection | N/A | ⬜ pending |
| 13-03-04 | 03 | 2 | HUD-01, HUD-02 | smoke | Visual inspection | N/A | ⬜ pending |
| 13-03-05 | 03 | 2 | ZONE-01, ZONE-02, ZONE-03 | smoke | Visual inspection + click | N/A | ⬜ pending |
| 13-03-06 | 03 | 2 | LOG-01 | smoke | Visual inspection | N/A | ⬜ pending |
| 13-03-07 | 03 | 2 | INTEG-01 | integration | Existing adapter tests cover | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/viewmodel/__tests__/cardProps.test.ts` — stubs for INTEG-02
- [ ] `client/src/viewmodel/__tests__/battlefieldGrouping.test.ts` — stubs for BOARD-02, BOARD-03, BOARD-04
- [ ] `client/src/viewmodel/__tests__/ptDisplay.test.ts` — stubs for BOARD-07, BOARD-08
- [ ] `client/src/viewmodel/__tests__/dominantColor.test.ts` — stubs for BOARD-09
- [ ] `client/src/viewmodel/__tests__/logFormatting.test.ts` — stubs for LOG-02, LOG-03
- [ ] `client/src/stores/__tests__/preferencesStore.test.ts` — stubs for INTEG-03
- [ ] `client/src/viewmodel/__tests__/cardSizing.test.ts` — stubs for BOARD-01

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Fan layout from bottom edge | HAND-01 | Visual layout + gesture interaction | Open game, verify hand fans from bottom, cards overlap naturally |
| Drag-to-play with threshold | HAND-02 | Gesture interaction testing | Drag card from hand upward, verify snap-back on short drag, play on long drag |
| Opponent card backs | HAND-04 | Visual inspection | Verify opponent hand shows card backs only |
| Aura/equipment visual attachment | BOARD-06 | Visual attachment positioning | Play an aura, verify it visually attaches to target permanent |
| Player HUD layout | HUD-01 | Layout/visual design | Verify life, mana pool, phase indicator visible |
| Opponent HUD layout | HUD-02 | Layout/visual design | Verify opponent life and mana visible |
| Graveyard viewer modal | ZONE-01 | Modal interaction | Click graveyard, verify modal opens with card list |
| Exile viewer modal | ZONE-02 | Modal interaction | Click exile, verify modal opens with card list |
| Zone count indicators | ZONE-03 | Visual badge display | Verify graveyard/exile show card counts |
| Scrollable log panel | LOG-01 | Scroll behavior | Verify log scrolls, auto-scrolls on new entries |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
