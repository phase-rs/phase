---
phase: 12
slug: multiplayer-wiring-final-cleanup
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-08
---

# Phase 12 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust: cargo test, Frontend: vitest 3.x |
| **Config file** | `client/vitest.config.ts`, Cargo workspace |
| **Quick run command** | `cargo test -p server-core -p forge-server` / `cd client && pnpm test -- --run` |
| **Full suite command** | `cargo test --all && cd client && pnpm test -- --run` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p server-core -p forge-server && cd client && pnpm test -- --run`
- **After every plan wave:** Run `cargo test --all && cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 12-01-01 | 01 | 1 | MP-01 | unit | `cargo test -p server-core -- resolve_deck` | ❌ W0 | ⬜ pending |
| 12-01-02 | 01 | 1 | MP-01 | unit | `cargo test -p server-core -- resolve_deck_missing` | ❌ W0 | ⬜ pending |
| 12-01-03 | 01 | 1 | MP-01 | unit | `cargo test -p engine -- load_deck` | ✅ | ⬜ pending |
| 12-01-04 | 01 | 1 | MP-03 | unit | `cd client && pnpm test -- --run ws-adapter` | ❌ W0 | ⬜ pending |
| 12-01-05 | 01 | 1 | MP-03 | unit | `cd client && pnpm test -- --run ws-adapter` | ❌ W0 | ⬜ pending |
| 12-01-06 | 01 | 1 | MP-03 | unit | `cd client && pnpm test -- --run ws-adapter` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/server-core/src/deck_resolve.rs` — unit tests for resolve_deck function (MP-01)
- [ ] `client/src/adapter/__tests__/ws-adapter.test.ts` — reconnect behavior tests with mock WebSocket (MP-03)

*Wave 0 creates test stubs before implementation begins.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Reconnect overlay banner visible during retry | MP-03 | Visual/UX behavior | 1. Start multiplayer game 2. Kill server 3. Verify banner shows "Reconnecting..." 4. Restart server 5. Verify auto-reconnect |
| Server rejects game with invalid card names | MP-01 | End-to-end integration | 1. Submit deck with fake card name 2. Verify error message returned |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
