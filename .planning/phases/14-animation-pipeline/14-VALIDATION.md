---
phase: 14
slug: animation-pipeline
status: draft
nyquist_compliant: true
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
| 14-01-T1 | 01 | 1 | ANIM-02, VFX-01 | unit | `cd client && pnpm test -- --run src/animation/__tests__/eventNormalizer.test.ts src/animation/__tests__/wubrgColors.test.ts` | TDD (created in task) | ⬜ pending |
| 14-01-T2 | 01 | 1 | ANIM-05, ANIM-06 | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | ✅ extend | ⬜ pending |
| 14-02-T1 | 02 | 2 | ANIM-01 | unit | `cd client && pnpm test -- --run src/stores/__tests__/animationStore.test.ts` | ✅ extend | ⬜ pending |
| 14-02-T2 | 02 | 2 | ANIM-03, ANIM-04 | unit | `cd client && pnpm test -- --run src/hooks/__tests__/useGameDispatch.test.ts` | TDD (created in task) | ⬜ pending |
| 14-03-T1 | 03 | 3 | VFX-02 | unit+type | `cd client && pnpm test -- --run src/components/animation/__tests__/FloatingNumber.test.tsx && pnpm run type-check` | Created in task | ⬜ pending |
| 14-03-T2 | 03 | 3 | VFX-03 | unit | `cd client && pnpm test -- --run src/components/animation/__tests__/screenShake.test.ts` | TDD (created in task) | ⬜ pending |
| 14-04-T1 | 04 | 4 | VFX-04, VFX-05, VFX-08 | type+existing | `cd client && pnpm run type-check && pnpm test -- --run` | N/A (wiring) | ⬜ pending |
| 14-04-T2 | 04 | 4 | VFX-06, VFX-07 | type+existing | `cd client && pnpm run type-check && pnpm test -- --run` | N/A (VFX quality gating) | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

All test files are created inline by their respective plan tasks (TDD or task-level test creation). No separate Wave 0 scaffold needed:

- Plan 01 Task 1 (TDD): creates `eventNormalizer.test.ts` and `wubrgColors.test.ts`
- Plan 01 Task 2: creates/extends `preferencesStore.test.ts`
- Plan 02 Task 1 (TDD): extends `animationStore.test.ts`
- Plan 02 Task 2: creates `useGameDispatch.test.ts`
- Plan 03 Task 1: creates `FloatingNumber.test.tsx`
- Plan 03 Task 2 (TDD): creates `screenShake.test.ts`

---

## Sampling Continuity Check

| Sequence | Task | Has Automated Test | OK |
|----------|------|-------------------|-----|
| 1 | 14-01-T1 | Yes (eventNormalizer, wubrgColors) | ✅ |
| 2 | 14-01-T2 | Yes (preferencesStore) | ✅ |
| 3 | 14-02-T1 | Yes (animationStore) | ✅ |
| 4 | 14-02-T2 | Yes (useGameDispatch) | ✅ |
| 5 | 14-03-T1 | Yes (FloatingNumber) | ✅ |
| 6 | 14-03-T2 | Yes (screenShake) | ✅ |
| 7 | 14-04-T1 | type-check + full suite | ✅ |
| 8 | 14-04-T2 | type-check + full suite | ✅ |

No 3 consecutive tasks without targeted automated tests.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Particle glow and gravity visual appearance | VFX-01 (partial) | Canvas rendering visual | Play game, verify particles have glow halo and drift down |
| Damage vignette flashes on player damage | VFX-05 | CSS visual overlay effect | Deal damage to player, verify red vignette appears and fades |
| Targeting arcs connect source to target | VFX-07 | SVG visual rendering | Cast targeted spell, verify curved arc from source to target |
| Turn banner slide animation | VFX-08 (partial) | Framer Motion visual timing | Start new turn, verify banner slides in/pauses/slides out |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify or inline test creation
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 covered by inline TDD/test creation in plan tasks
- [x] No watch-mode flags
- [x] Feedback latency < 15s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
