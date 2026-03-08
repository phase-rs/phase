---
phase: 11
slug: tech-debt-cleanup-constants-combat-ui-wasm-tests-ci-coverage
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-08
---

# Phase 11 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework (TS)** | Vitest 3.x + @testing-library/react 16 + jsdom |
| **Framework (Rust)** | cargo test (built-in) |
| **Config file (TS)** | `client/vitest.config.ts` |
| **Quick run command (TS)** | `cd client && pnpm test -- --run` |
| **Quick run command (Rust)** | `cargo test --all` |
| **Full suite command** | `cargo test --all && cd client && pnpm test -- --run` |
| **Estimated runtime** | ~60 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run`
- **After every plan wave:** Run `cargo test --all && cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 60 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 11-01-01 | 01 | 1 | TD-01 | unit | `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts` | ✅ | ⬜ pending |
| 11-02-01 | 02 | 1 | TD-02 | component | `cd client && pnpm test -- --run src/components/combat/__tests__/CombatOverlay.test.tsx` | ❌ W0 | ⬜ pending |
| 11-02-02 | 02 | 1 | TD-03 | component | `cd client && pnpm test -- --run src/components/combat/__tests__/CombatOverlay.test.tsx` | ❌ W0 | ⬜ pending |
| 11-03-01 | 03 | 1 | TD-04 | component | `cd client && pnpm test -- --run src/components/modal/__tests__/CardDataMissingModal.test.tsx` | ❌ W0 | ⬜ pending |
| 11-04-01 | 04 | 2 | TD-05 | unit | `cd client && pnpm test -- --run src/adapter/__tests__/wasm-adapter.test.ts` | ✅ | ⬜ pending |
| 11-05-01 | 05 | 2 | TD-06 | CI | Full CI pipeline | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/components/combat/__tests__/CombatOverlay.test.tsx` — stubs for TD-02, TD-03
- [ ] `client/src/components/modal/__tests__/CardDataMissingModal.test.tsx` — stubs for TD-04
- [ ] `@vitest/coverage-v8` — install dev dependency for coverage reporting
- [ ] `client/vitest.config.ts` — add coverage configuration block

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Combat tilt/glow visual | TD-02 | CSS visual — hard to assert in jsdom | Inspect attacker cards tilt 15° with red border glow |
| Blocker-to-attacker lines | TD-03 | SVG positioning in DOM | Verify arrows render between blocker and attacker |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 60s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
