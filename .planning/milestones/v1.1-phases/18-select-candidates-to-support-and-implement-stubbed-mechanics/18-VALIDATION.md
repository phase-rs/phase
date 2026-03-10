---
phase: 18
slug: select-candidates-to-support-and-implement-stubbed-mechanics
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-09
---

# Phase 18 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in `#[test]` + `#[cfg(test)]` modules |
| **Config file** | Cargo.toml (workspace test settings) |
| **Quick run command** | `cargo test -p engine -- <module>::tests` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine -- <module>::tests`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 18-01-01 | 01 | 1 | Fear evasion | unit | `cargo test -p engine -- combat::tests::fear` | ❌ W0 | ⬜ pending |
| 18-01-02 | 01 | 1 | Intimidate evasion | unit | `cargo test -p engine -- combat::tests::intimidate` | ❌ W0 | ⬜ pending |
| 18-01-03 | 01 | 1 | Skulk evasion | unit | `cargo test -p engine -- combat::tests::skulk` | ❌ W0 | ⬜ pending |
| 18-01-04 | 01 | 1 | Horsemanship evasion | unit | `cargo test -p engine -- combat::tests::horsemanship` | ❌ W0 | ⬜ pending |
| 18-02-01 | 02 | 2 | Mill effect | unit | `cargo test -p engine -- effects::mill::tests` | ❌ W0 | ⬜ pending |
| 18-02-02 | 02 | 2 | Scry effect | unit | `cargo test -p engine -- effects::scry::tests` | ❌ W0 | ⬜ pending |
| 18-02-03 | 02 | 2 | Surveil effect | unit | `cargo test -p engine -- effects::surveil::tests` | ❌ W0 | ⬜ pending |
| 18-03-01 | 03 | 2 | Ward targeting cost | unit | `cargo test -p engine -- targeting::tests::ward` | ❌ W0 | ⬜ pending |
| 18-03-02 | 03 | 2 | Protection (DEBT) | unit | `cargo test -p engine -- targeting::tests::protection` | ❌ W0 | ⬜ pending |
| 18-04-01 | 04 | 3 | Integration tests | integration | `cargo test -p engine -- --ignored` | ❌ W0 | ⬜ pending |
| 18-04-02 | 04 | 3 | Coverage report | integration | `cargo test -p engine -- coverage_report --ignored` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/game/test_helpers.rs` — reusable card-loading helper for integration tests
- [ ] Effect module stubs: `effects/mill.rs`, `effects/scry.rs`, `effects/surveil.rs`
- [ ] Integration test structure: inline `#[ignore]` tests or `crates/engine/tests/` directory

*Existing infrastructure covers unit test framework — only new test files and effect modules needed.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Card-level UI warning | Unimplemented mechanic indicator | Visual/UX verification | Load a card with unimplemented mechanics, verify warning badge visible |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
