---
phase: 3
slug: game-state-engine
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 3 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test framework (cargo test) |
| **Config file** | None — `#[cfg(test)]` modules in each file |
| **Quick run command** | `cargo test -p engine` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~5 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine`
- **After every plan wave:** Run `cargo test --workspace`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 10 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 03-01-01 | 01 | 1 | ENG-04 | unit | `cargo test -p engine game_object` | Wave 0 | pending |
| 03-01-02 | 01 | 1 | ENG-04 | unit | `cargo test -p engine zone_change` | Wave 0 | pending |
| 03-01-03 | 01 | 1 | ENG-05 | unit | `cargo test -p engine mana_pool` | Wave 0 | pending |
| 03-02-01 | 02 | 1 | ENG-01 | unit | `cargo test -p engine turn_progression` | Wave 0 | pending |
| 03-02-02 | 02 | 1 | ENG-02 | unit | `cargo test -p engine priority` | Wave 0 | pending |
| 03-02-03 | 02 | 1 | ENG-02 | unit | `cargo test -p engine stack_lifo` | Wave 0 | pending |
| 03-02-04 | 02 | 1 | ENG-01 | unit | `cargo test -p engine auto_advance` | Wave 0 | pending |
| 03-03-01 | 03 | 2 | ENG-03 | unit | `cargo test -p engine sba` | Wave 0 | pending |
| 03-03-02 | 03 | 2 | ENG-06 | unit | `cargo test -p engine mulligan` | Wave 0 | pending |
| 03-03-03 | 03 | 2 | ENG-01..06 | integration | `cargo test -p engine full_turn_integration` | Wave 0 | pending |

*Status: pending · green · red · flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/game/` — entire module tree (engine.rs, turns.rs, priority.rs, stack.rs, zones.rs, mana_payment.rs, sba.rs, mulligan.rs, game_object.rs)
- [ ] `rand` dependency added to engine Cargo.toml
- [ ] Restructured ManaPool type with tracked mana units
- [ ] Expanded GameState with objects store, zone collections, RNG, WaitingFor
- [ ] Expanded Player with per-player zone collections
- [ ] GameObject struct

---

## Manual-Only Verifications

*All phase behaviors have automated verification.*

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 10s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
