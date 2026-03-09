---
phase: 14
slug: animation-pipeline
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-08
---

# Phase 14 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Vitest 3.x + jsdom |
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
| 14-01-01 | 01 | 0 | ANIM-02 | unit | `cd client && pnpm test -- --run src/animation/__tests__/eventNormalizer.test.ts` | ❌ W0 | ⬜ pending |
| 14-01-02 | 01 | 0 | ANIM-03 | unit | `cd client && pnpm test -- --run src/animation/__tests__/eventNormalizer.test.ts` | ❌ W0 | ⬜ pending |
| 14-01-03 | 01 | 0 | VFX-01 | unit | `cd client && pnpm test -- --run src/animation/__tests__/wubrgColors.test.ts` | ❌ W0 | ⬜ pending |
| 14-01-04 | 01 | 0 | VFX-03 | unit | `cd client && pnpm test -- --run src/animation/__tests__/screenShake.test.ts` | ❌ W0 | ⬜ pending |
| 14-01-05 | 01 | 0 | ANIM-04 | unit | `cd client && pnpm test -- --run src/hooks/__tests__/useGameDispatch.test.ts` | ❌ W0 | ⬜ pending |
| 14-01-06 | 01 | 0 | ANIM-05, ANIM-06 | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | ✅ extend | ⬜ pending |
| 14-01-07 | 01 | 0 | ANIM-01 | unit | `cd client && pnpm test -- --run src/stores/__tests__/animationStore.test.ts` | ✅ extend | ⬜ pending |
| 14-xx-xx | TBD | TBD | VFX-04 | unit | covered by eventNormalizer | ❌ W0 | ⬜ pending |
| 14-xx-xx | TBD | TBD | VFX-08 | unit | covered by eventNormalizer | ❌ W0 | ⬜ pending |
| 14-xx-xx | TBD | TBD | VFX-06 | unit | `cd client && pnpm test -- --run src/components/combat/__tests__/CombatOverlay.test.tsx` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/animation/__tests__/eventNormalizer.test.ts` — stubs for ANIM-02, ANIM-03, VFX-04, VFX-08
- [ ] `client/src/animation/__tests__/wubrgColors.test.ts` — stubs for VFX-01
- [ ] `client/src/animation/__tests__/screenShake.test.ts` — stubs for VFX-03
- [ ] `client/src/hooks/__tests__/useGameDispatch.test.ts` — stubs for ANIM-04
- [ ] Extend `client/src/stores/__tests__/preferencesStore.test.ts` — stubs for ANIM-05, ANIM-06
- [ ] Update `client/src/stores/__tests__/animationStore.test.ts` — stubs for ANIM-01

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Floating damage/heal numbers animate visually | VFX-02 | Framer Motion visual animation timing | Trigger damage event, verify number floats upward and fades |
| Damage vignette flashes on player damage | VFX-05 | CSS visual overlay effect | Deal damage to player, verify red vignette appears and fades |
| Targeting arcs connect source to target | VFX-07 | SVG visual rendering | Cast targeted spell, verify curved arc from source to target |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
