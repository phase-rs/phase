---
phase: 10
slug: fix-undo-wasm-state-sync
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-08
---

# Phase 10 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | vitest (frontend) + cargo test (Rust) |
| **Config file** | `client/vitest.config.ts` + `Cargo.toml` |
| **Quick run command** | `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts` |
| **Full suite command** | `cd client && pnpm test -- --run && cargo test -p engine-wasm` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts`
- **After every plan wave:** Run `cd client && pnpm test -- --run && cargo test -p engine-wasm`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 10-01-01 | 01 | 1 | QOL-02 | unit | `cargo test -p engine-wasm restore` | ❌ W0 | ⬜ pending |
| 10-01-02 | 01 | 1 | QOL-02 | unit | `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts` | ✅ (needs update) | ⬜ pending |
| 10-01-03 | 01 | 1 | QOL-02 | integration | Manual WASM integration test | ❌ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/stores/__tests__/gameStore.test.ts` — add test asserting `adapter.restoreState` called during undo
- [ ] `cargo test -p engine-wasm` — add test for `restore_game_state` round-trip (Rust-side unit test)

*Existing test infrastructure covers framework installation; only test stubs needed.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| After undo + submitAction, engine uses restored state | QOL-02 | Full WASM integration requires browser context | 1. Start game 2. Take action 3. Press Z to undo 4. Take different action 5. Verify no desync |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
