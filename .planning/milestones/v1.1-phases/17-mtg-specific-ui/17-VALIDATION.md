---
phase: 17
slug: mtg-specific-ui
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-09
---

# Phase 17 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Vitest (jsdom environment) |
| **Config file** | client/vitest.config.ts |
| **Quick run command** | `cd client && pnpm test -- --run --reporter=verbose` |
| **Full suite command** | `cd client && pnpm test -- --run && pnpm run type-check` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run --reporter=verbose`
- **After every plan wave:** Run `cd client && pnpm test -- --run && pnpm run type-check`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 17-01-xx | 01 | 1 | STACK-01 | unit | `cd client && pnpm test -- --run src/components/stack/__tests__/StackDisplay.test.tsx` | ❌ W0 | ⬜ pending |
| 17-01-xx | 01 | 1 | STACK-02 | unit | `cd client && pnpm test -- --run src/components/board/__tests__/ActionButton.test.tsx` | ❌ W0 | ⬜ pending |
| 17-01-xx | 01 | 1 | STACK-03 | unit | `cd client && pnpm test -- --run src/game/__tests__/autoPass.test.ts` | ✅ | ⬜ pending |
| 17-01-xx | 01 | 1 | STACK-04 | unit | `cd client && pnpm test -- --run src/game/__tests__/autoPass.test.ts` | ✅ | ⬜ pending |
| 17-01-xx | 01 | 1 | MANA-01 | unit | `cd client && pnpm test -- --run src/components/mana/__tests__/ManaPaymentUI.test.tsx` | ❌ W0 | ⬜ pending |
| 17-01-xx | 01 | 1 | MANA-02 | unit | `cd client && pnpm test -- --run src/components/mana/__tests__/ManaPaymentUI.test.tsx` | ❌ W0 | ⬜ pending |
| 17-01-xx | 01 | 1 | MANA-03 | unit | `cd client && pnpm test -- --run src/components/mana/__tests__/ManaBadge.test.tsx` | ❌ W0 | ⬜ pending |
| 17-01-xx | 01 | 1 | COMBAT-01 | unit | `cd client && pnpm test -- --run src/components/board/__tests__/ActionButton.test.tsx` | ❌ W0 | ⬜ pending |
| 17-01-xx | 01 | 1 | COMBAT-02 | unit | `cd client && pnpm test -- --run src/components/board/__tests__/ActionButton.test.tsx` | ❌ W0 | ⬜ pending |
| 17-01-xx | 01 | 1 | COMBAT-03 | manual-only | N/A — DEFERRED | N/A | ⬜ deferred |
| 17-01-xx | 01 | 1 | COMBAT-04 | unit | `cd client && pnpm test -- --run src/components/combat/__tests__/DamageAssignmentModal.test.tsx` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `src/components/board/__tests__/ActionButton.test.tsx` — stubs for STACK-02, COMBAT-01, COMBAT-02
- [ ] `src/components/stack/__tests__/StackDisplay.test.tsx` — stubs for STACK-01
- [ ] `src/components/mana/__tests__/ManaPaymentUI.test.tsx` — stubs for MANA-01, MANA-02
- [ ] `src/components/mana/__tests__/ManaBadge.test.tsx` — stubs for MANA-03
- [ ] `src/components/combat/__tests__/DamageAssignmentModal.test.tsx` — stubs for COMBAT-04

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Combat math bubbles | COMBAT-03 | DEFERRED per user decision | N/A — not implementing |
| Visual MTGA feel (board sizing, hand fan, HUD) | N/A (expanded scope) | Visual/subjective | Launch dev server, play a game, compare feel to MTGA |
| Resolve All with interrupt | STACK-02 | Async timing hard to unit test | Play game, cast spell during Resolve All, verify interruption |
| BlockAssignmentLines animation | COMBAT-02 | SVG animation timing | Declare blockers, verify animated dashed lines track positions |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
