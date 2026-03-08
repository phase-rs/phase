---
phase: 4
slug: ability-system-effects
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 4 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test framework (cargo test) |
| **Config file** | Cargo.toml (existing) |
| **Quick run command** | `cargo test --lib -p engine` |
| **Full suite command** | `cargo test` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test --lib -p engine`
- **After every plan wave:** Run `cargo test`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 04-01-01 | 01 | 1 | ABIL-06 | unit | `cargo test --lib -p engine effects` | No - W0 | pending |
| 04-01-02 | 01 | 1 | ABIL-02 | unit | `cargo test --lib -p engine svar` | No - W0 | pending |
| 04-02-01 | 02 | 1 | ABIL-03 | unit | `cargo test --lib -p engine cost_parser` | No - W0 | pending |
| 04-02-02 | 02 | 1 | ABIL-04 | unit | `cargo test --lib -p engine targeting` | No - W0 | pending |
| 04-03-01 | 03 | 2 | ABIL-05 | unit | `cargo test --lib -p engine condition` | No - W0 | pending |
| 04-03-02 | 03 | 2 | ABIL-07 | integration | `cargo test --lib -p engine sub_ability_chain` | No - W0 | pending |
| 04-03-03 | 03 | 2 | SC-1 | integration | `cargo test --lib -p engine lightning_bolt` | No - W0 | pending |
| 04-03-04 | 03 | 2 | SC-2 | integration | `cargo test --lib -p engine counterspell` | No - W0 | pending |
| 04-03-05 | 03 | 2 | SC-3 | integration | `cargo test --lib -p engine giant_growth` | No - W0 | pending |

*Status: pending · green · red · flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/game/effects/mod.rs` — effect registry and dispatch test stubs
- [ ] `crates/engine/src/game/casting.rs` — casting flow test stubs (timing, cost, targets)
- [ ] `crates/engine/src/game/targeting.rs` — target validation and fizzle test stubs
- [ ] Integration test stubs for Lightning Bolt, Counterspell, Giant Growth scenarios

*Wave 0 tests are stubs that compile but fail — they define the verification contract before implementation.*

---

## Manual-Only Verifications

All phase behaviors have automated verification.

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
