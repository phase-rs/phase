---
phase: 24
slug: card-migration
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-10
---

# Phase 24 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test + cargo integration tests |
| **Config file** | `Cargo.toml` [[test]] entries (auto-detected by cargo) |
| **Quick run command** | `cargo test --test parity` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~120 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine && cargo test --test parity`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 120 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 24-01-xx | 01 | 1 | MIGR-01 | integration | `cargo test --test parity -- parity_all_standard_cards` | ❌ W0 | ⬜ pending |
| 24-01-xx | 01 | 1 | MIGR-03 | integration (manual run) | `cargo run --bin migrate -- data/cardsfolder data/abilities` | ❌ W0 | ⬜ pending |
| 24-02-xx | 02 | 2 | TEST-04 | integration | `cargo test --test parity` | ❌ W0 | ⬜ pending |
| 24-03-xx | 03 | 2 | MIGR-05 | integration | `cargo run --bin coverage-report -- --json data/abilities --ci` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/tests/parity.rs` — stubs for MIGR-01, TEST-04
- [ ] `crates/engine/src/bin/migrate.rs` — covers MIGR-03
- [ ] `data/standard-cards.txt` — manifest for both coverage gates
- [ ] Extended `data/mtgjson/test_fixture.json` — 78 Standard cards for JSON load path in parity tests

*Existing infrastructure covers test framework and configuration.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Migration tool processes all 32,300 cards | MIGR-03 | Full run takes ~minutes, not suitable for CI quick check | Run `cargo run --bin migrate -- data/cardsfolder data/abilities` and verify summary stats |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 120s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
