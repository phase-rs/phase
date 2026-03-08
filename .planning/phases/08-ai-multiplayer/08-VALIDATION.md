---
phase: 8
slug: ai-multiplayer
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-08
---

# Phase 8 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust unit tests (cargo test) + Vitest (client) |
| **Config file** | Cargo.toml (Rust), vitest.config.ts (client) |
| **Quick run command** | `cargo test -p forge-ai --lib` |
| **Full suite command** | `cargo test --workspace && cd client && pnpm test` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p forge-ai --lib` or `cargo test -p server-core --lib` (whichever is relevant)
- **After every plan wave:** Run `cargo test --workspace && cd client && pnpm test`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 08-01-01 | 01 | 1 | AI-01 | unit | `cargo test -p forge-ai legal_actions -x` | ❌ W0 | ⬜ pending |
| 08-01-02 | 01 | 1 | AI-02 | unit | `cargo test -p forge-ai eval -x` | ❌ W0 | ⬜ pending |
| 08-01-03 | 01 | 1 | AI-03 | unit | `cargo test -p forge-ai card_hints -x` | ❌ W0 | ⬜ pending |
| 08-01-04 | 01 | 1 | AI-04 | unit | `cargo test -p forge-ai search -x` | ❌ W0 | ⬜ pending |
| 08-01-05 | 01 | 1 | AI-05 | unit | `cargo test -p forge-ai config -x` | ❌ W0 | ⬜ pending |
| 08-02-01 | 02 | 2 | MP-01 | integration | `cargo test -p forge-server -- --ignored` | ❌ W0 | ⬜ pending |
| 08-02-02 | 02 | 2 | MP-02 | unit | `cargo test -p server-core filter -x` | ❌ W0 | ⬜ pending |
| 08-02-03 | 02 | 2 | MP-03 | unit | `cargo test -p server-core session -x` | ❌ W0 | ⬜ pending |
| 08-02-04 | 02 | 2 | MP-04 | integration | `cargo test -p server-core reconnect -x` | ❌ W0 | ⬜ pending |
| 08-03-01 | 03 | 2 | PLAT-05 | unit | `cargo test -p engine coverage -x` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/forge-ai/` — entire new crate (lib.rs, legal_actions.rs, eval.rs, search.rs, config.rs, combat_ai.rs) with test stubs
- [ ] `crates/server-core/` — entire new crate (lib.rs, session.rs, protocol.rs, filter.rs, reconnect.rs) with test stubs
- [ ] `crates/forge-server/` — entire new crate (main.rs) with integration test stubs
- [ ] Cargo workspace members updated to include new crates
- [ ] `client/src/game/controllers/aiController.ts` — AI controller with test stub
- [ ] `client/src/adapter/ws-adapter.ts` — WebSocketAdapter with test stub

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| AI provides challenging gameplay at multiple difficulty levels | AI-05 | Subjective quality assessment | Play 3 games at Easy, Medium, Hard; verify perceived difficulty difference |
| AI hand hidden by default with debug toggle | AI-05 | Visual verification | Start AI game, confirm hand is hidden, enable debug toggle, confirm hand visible |
| Simulated thinking delay feels natural | AI-05 | UX feel assessment | Observe AI turns; verify 0.5-2s delay feels natural, not jarring |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
