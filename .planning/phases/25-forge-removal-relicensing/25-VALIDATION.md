---
phase: 25
slug: forge-removal-relicensing
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-10
---

# Phase 25 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test + cargo test |
| **Config file** | Cargo.toml (workspace) |
| **Quick run command** | `cargo test -p engine` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~60 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine && cargo clippy --all-targets -- -D warnings`
- **After every plan wave:** Run `cargo test --all && cargo build --package engine-wasm --target wasm32-unknown-unknown`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 60 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 25-01-01 | 01 | 1 | MIGR-02 | build | `cargo build -p engine` (no features) | ✅ | ⬜ pending |
| 25-01-02 | 01 | 1 | MIGR-02 | smoke | `test ! -d data/standard-cards/` | ❌ W0 | ⬜ pending |
| 25-01-03 | 01 | 1 | MIGR-02 | build | `cargo build -p engine --features forge-compat` | ❌ W0 | ⬜ pending |
| 25-02-01 | 02 | 1 | LICN-01 | smoke | `test -f LICENSE-MIT` | ❌ W0 | ⬜ pending |
| 25-02-02 | 02 | 1 | LICN-01 | smoke | `test -f LICENSE-APACHE` | ❌ W0 | ⬜ pending |
| 25-02-03 | 02 | 1 | LICN-01 | grep | `rg 'license' crates/*/Cargo.toml Cargo.toml` | ❌ W0 | ⬜ pending |
| 25-03-01 | 03 | 2 | LICN-03 | integration | `cargo run --bin coverage-report -- data/ --ci` | ✅ (adapted) | ⬜ pending |
| 25-03-02 | 03 | 2 | LICN-03 | test | `cargo test --all` | ✅ | ⬜ pending |
| 25-03-03 | 03 | 2 | LICN-03 | build | `cargo build --package engine-wasm --target wasm32-unknown-unknown` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

None — existing test infrastructure covers all phase requirements. The refactoring itself is validated by the existing test suite continuing to pass. Smoke tests for data deletion and license files are trivial shell checks, not framework tests.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| PROJECT.md reflects MTGJSON | LICN-02 | Documentation content review | Read PROJECT.md, verify no Forge runtime dependency mentions |
| CLAUDE.md scrubbed | LICN-02 | Documentation content review | Read CLAUDE.md, verify Forge references removed from ungated descriptions |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 60s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
