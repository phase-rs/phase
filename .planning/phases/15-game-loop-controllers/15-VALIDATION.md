---
phase: 15
slug: game-loop-controllers
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-08
---

# Phase 15 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | vitest (jsdom environment) |
| **Config file** | `client/vitest.config.ts` |
| **Quick run command** | `cd client && pnpm test -- --run --reporter=verbose` |
| **Full suite command** | `cd client && pnpm test -- --run` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run --reporter=verbose`
- **After every plan wave:** Run `cd client && pnpm test -- --run && cd client && pnpm run type-check`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 15-01-01 | 01 | 0 | LOOP-04 | unit | `cd client && pnpm test -- --run src/game/__tests__/autoPass.test.ts` | ❌ W0 | ⬜ pending |
| 15-01-02 | 01 | 0 | LOOP-02 | unit | `cd client && pnpm test -- --run src/game/__tests__/dispatch.test.ts` | ❌ W0 | ⬜ pending |
| 15-01-03 | 01 | 0 | LOOP-02 | unit | `cd client && pnpm test -- --run src/game/controllers/__tests__/gameLoopController.test.ts` | ❌ W0 | ⬜ pending |
| 15-01-04 | 01 | 0 | LOOP-01 | unit | `cd client && pnpm test -- --run src/game/controllers/__tests__/opponentController.test.ts` | ❌ W0 | ⬜ pending |
| 15-01-05 | 01 | 0 | LOOP-03 | unit | `cd client && pnpm test -- --run src/providers/__tests__/GameProvider.test.tsx` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/game/__tests__/autoPass.test.ts` — stubs for LOOP-04 auto-pass heuristics
- [ ] `client/src/game/__tests__/dispatch.test.ts` — stubs for standalone dispatch function
- [ ] `client/src/game/controllers/__tests__/gameLoopController.test.ts` — stubs for LOOP-02 game loop
- [ ] `client/src/game/controllers/__tests__/opponentController.test.ts` — stubs for LOOP-01 opponent controller
- [ ] `client/src/providers/__tests__/GameProvider.test.tsx` — stubs for LOOP-03 context provider

*Existing vitest infrastructure covers framework requirements.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| AI "thinking" pause feels natural (300-800ms) | LOOP-01 | Perceptual timing | Start AI game, observe AI turn pacing feels deliberate |
| Phase stop toggles are visible and responsive | LOOP-04 | Visual UX | Click phases on indicator strip, verify toggle state |
| Auto-passed phases show brief visual beat | LOOP-02 | Animation timing | Watch untap-upkeep-draw auto-pass, verify phase indicator updates visibly |
| GamePage reduced to ~half size | LOOP-03 | Architecture quality | Check GamePage line count < 350 |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
