---
phase: 29
slug: support-n-players-in-engine-and-on-board-in-react-for-various-formats
status: draft
nyquist_compliant: true
wave_0_complete: true
created: 2026-03-11
---

# Phase 29 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust) + vitest (TypeScript) |
| **Config file** | Cargo.toml / client/vitest.config.ts |
| **Quick run command** | `cargo test -p engine && cargo test -p phase-ai` |
| **Full suite command** | `cargo test --all && cd client && pnpm test -- --run` |
| **Estimated runtime** | ~60 seconds |

---

## Test Strategy: Inline TDD

This phase uses **inline TDD** (`tdd="true"` on tasks). Tests are created as part of each task's RED-GREEN-REFACTOR cycle. There is no separate Wave 0 plan — each task creates its own test files alongside production code.

**Rationale:** The N-player migration touches many existing files that already have tests. New modules (format.rs, players.rs, elimination.rs, commander.rs) create their tests inline during implementation. This avoids a separate test scaffolding phase that would need to predict implementation details.

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine && cargo test -p phase-ai`
- **After every plan wave:** Run `cargo test --all && cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 60 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | Strategy | Status |
|---------|------|------|-------------|-----------|-------------------|----------|--------|
| 29-01-01 | 01 | 1 | NP-FORMAT | unit | `cargo test -p engine -- format` | inline TDD | ⬜ pending |
| 29-01-02 | 01 | 1 | NP-ITER | unit | `cargo test -p engine -- players` | inline TDD | ⬜ pending |
| 29-02-01 | 02 | 2 | NP-PRIORITY | unit | `cargo test -p engine -- priority` | inline TDD | ⬜ pending |
| 29-02-02 | 02 | 2 | NP-ELIM | unit | `cargo test -p engine -- elimination` | inline TDD | ⬜ pending |
| 29-03-01 | 03 | 2 | NP-COMBAT | unit | `cargo test -p engine -- combat` | inline TDD | ⬜ pending |
| 29-03-02 | 03 | 2 | NP-ATTACK-TARGET | unit | `cargo test -p engine -- commander` | inline | ⬜ pending |
| 29-04-01 | 04 | 2 | NP-COMMANDER | unit | `cargo test -p engine -- commander` | inline TDD | ⬜ pending |
| 29-05-01 | 05 | 3 | NP-OPPONENT-MIGRATION | integration | `cargo test -p engine` | migration + existing tests | ⬜ pending |
| 29-06-01 | 06 | 3 | NP-WASM | unit | `cargo test -p engine-wasm -p phase-ai` | inline | ⬜ pending |
| 29-06-02 | 06 | 3 | NP-SERVER | unit | `cargo test -p server-core -p phase-server` | inline | ⬜ pending |
| 29-07-01 | 07 | 5 | NP-BOARD-UI | component | `cd client && pnpm test -- --run PlayerArea` | inline | ⬜ pending |
| 29-13-01 | 13 | 9 | NP-INTEGRATION | integration | `cargo test --all && cd client && pnpm build` | full suite | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Compact opponent strip readability | NP-COMPACT | Visual layout judgment | 4-player Commander game, verify all opponent strips are readable at 1080p |
| 1v1 visual parity | NP-1V1-PARITY | Visual regression | Compare 2-player game screenshot before/after refactor |
| Game setup format-first UX flow | NP-SETUP-FLOW | UX evaluation | Walk through format selection -> config -> lobby -> start for each format |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify commands
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Inline TDD strategy documented (no Wave 0 needed)
- [x] No watch-mode flags
- [x] Feedback latency < 60s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** approved (inline TDD)
