---
phase: 5
slug: triggers-combat
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 5 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test + cargo test |
| **Config file** | Cargo.toml (existing) |
| **Quick run command** | `cargo test --lib -p engine` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test --lib -p engine`
- **After every plan wave:** Run `cargo test --workspace`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 05-01-01 | 01 | 1 | KWRD-01 | unit | `cargo test --lib -p engine keywords::tests` | Wave 0 | pending |
| 05-01-02 | 01 | 1 | KWRD-02 | unit | `cargo test --lib -p engine keywords::tests::parse` | Wave 0 | pending |
| 05-02-01 | 02 | 1 | TRIG-01 | unit | `cargo test --lib -p engine triggers::tests` | Wave 0 | pending |
| 05-02-02 | 02 | 1 | TRIG-02 | unit | `cargo test --lib -p engine triggers::tests::match_` | Wave 0 | pending |
| 05-02-03 | 02 | 1 | TRIG-03 | unit | `cargo test --lib -p engine triggers::tests::apnap` | Wave 0 | pending |
| 05-02-04 | 02 | 1 | TRIG-04 | unit | `cargo test --lib -p engine triggers::tests::mode_` | Wave 0 | pending |
| 05-03-01 | 03 | 2 | COMB-01 | unit | `cargo test --lib -p engine combat::tests::legality` | Wave 0 | pending |
| 05-03-02 | 03 | 2 | COMB-02 | unit | `cargo test --lib -p engine combat::tests::damage` | Wave 0 | pending |
| 05-03-03 | 03 | 2 | COMB-03 | unit | `cargo test --lib -p engine combat::tests::keyword` | Wave 0 | pending |
| 05-03-04 | 03 | 2 | COMB-04 | integration | `cargo test --lib -p engine combat::tests::death` | Wave 0 | pending |

*Status: pending · green · red · flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/types/keywords.rs` — Keyword + TriggerMode enum definitions
- [ ] `crates/engine/src/game/keywords.rs` — Keyword FromStr + has_keyword helpers + tests
- [ ] `crates/engine/src/game/triggers.rs` — trigger pipeline + matching + APNAP + tests
- [ ] `crates/engine/src/game/combat.rs` — CombatState + attack/block validation + tests
- [ ] `crates/engine/src/game/combat_damage.rs` — damage resolution + tests
- [ ] `GameObject.trigger_definitions: Vec<TriggerDefinition>` field — parsed at creation time
- [ ] `GameState.combat: Option<CombatState>` field

*All test files created as stubs in Wave 0; filled during execution.*

---

## Manual-Only Verifications

All phase behaviors have automated verification.

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
