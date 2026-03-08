---
phase: 6
slug: advanced-rules
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 6 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test framework (cargo test) |
| **Config file** | Cargo.toml (already configured) |
| **Quick run command** | `cargo test -p engine` |
| **Full suite command** | `cargo test` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine`
- **After every plan wave:** Run `cargo test`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 06-01-01 | 01 | 1 | REPL-01 | unit | `cargo test -p engine replacement` | ❌ W0 | ⬜ pending |
| 06-01-02 | 01 | 1 | REPL-02 | unit | `cargo test -p engine replacement::tests::once_per_event` | ❌ W0 | ⬜ pending |
| 06-01-03 | 01 | 1 | REPL-03 | unit | `cargo test -p engine replacement::tests::multiple_choice` | ❌ W0 | ⬜ pending |
| 06-01-04 | 01 | 1 | REPL-04 | unit | `cargo test -p engine replacement` | ❌ W0 | ⬜ pending |
| 06-02-01 | 02 | 2 | STAT-01 | unit | `cargo test -p engine layers` | ❌ W0 | ⬜ pending |
| 06-02-02 | 02 | 2 | STAT-02 | unit | `cargo test -p engine layers::tests::timestamp` | ❌ W0 | ⬜ pending |
| 06-02-03 | 02 | 2 | STAT-03 | unit | `cargo test -p engine layers::tests::dependency` | ❌ W0 | ⬜ pending |
| 06-02-04 | 02 | 2 | STAT-04 | unit | `cargo test -p engine static_abilities` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/types/proposed_event.rs` — ProposedEvent enum stubs
- [ ] `crates/engine/src/game/replacement.rs` — replacement pipeline + core test stubs
- [ ] `crates/engine/src/game/layers.rs` — layer evaluation engine + core test stubs
- [ ] petgraph + indexmap dependencies in `crates/engine/Cargo.toml`

*Test stubs created in Wave 0, implemented during plan execution.*

---

## Manual-Only Verifications

*All phase behaviors have automated verification.*

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
