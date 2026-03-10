---
phase: 22
slug: test-infrastructure
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-10
---

# Phase 22 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (built-in) + insta 1.x (snapshot) |
| **Config file** | `crates/engine/Cargo.toml` (dev-dependencies) |
| **Quick run command** | `cargo test -p engine -- rules` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine -- rules`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 22-01-01 | 01 | 1 | TEST-01 | unit | `cargo test -p engine -- scenario -x` | ❌ W0 | ⬜ pending |
| 22-01-02 | 01 | 1 | TEST-01 | unit | `cargo test -p engine -- scenario::tests -x` | ❌ W0 | ⬜ pending |
| 22-02-01 | 02 | 2 | TEST-02 | integration | `cargo test -p engine --test rules -- etb -x` | ❌ W0 | ⬜ pending |
| 22-02-02 | 02 | 2 | TEST-02 | integration | `cargo test -p engine --test rules -- combat -x` | ❌ W0 | ⬜ pending |
| 22-02-03 | 02 | 2 | TEST-02 | integration | `cargo test -p engine --test rules -- stack -x` | ❌ W0 | ⬜ pending |
| 22-02-04 | 02 | 2 | TEST-02 | integration | `cargo test -p engine --test rules -- sba -x` | ❌ W0 | ⬜ pending |
| 22-02-05 | 02 | 2 | TEST-02 | integration | `cargo test -p engine --test rules -- layers -x` | ❌ W0 | ⬜ pending |
| 22-02-06 | 02 | 2 | TEST-02 | integration | `cargo test -p engine --test rules -- keywords -x` | ❌ W0 | ⬜ pending |
| 22-03-01 | 03 | 2 | TEST-03 | integration | `cargo test -p engine --test rules -- snapshot -x` | ❌ W0 | ⬜ pending |
| 22-03-02 | 03 | 2 | TEST-03 | integration | `cargo test -p engine -- --check` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/game/scenario.rs` — GameScenario builder + GameSnapshot
- [ ] `crates/engine/tests/rules.rs` — Integration test entry point
- [ ] `crates/engine/tests/rules/mod.rs` — Common imports
- [ ] `crates/engine/tests/rules/etb.rs` — ETB trigger tests
- [ ] `crates/engine/tests/rules/combat.rs` — Combat tests
- [ ] `crates/engine/tests/rules/stack.rs` — Stack resolution tests
- [ ] `crates/engine/tests/rules/sba.rs` — SBA tests
- [ ] `crates/engine/tests/rules/layers.rs` — Layer system tests
- [ ] `crates/engine/tests/rules/keywords.rs` — Keyword interaction tests
- [ ] `crates/engine/tests/rules/targeting.rs` — Targeting/fizzle tests

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Zero silent skips in CI | TEST-01 | Requires inspecting cargo output for skip patterns | Run `cargo test --all 2>&1 \| grep -i skip` and verify no tests silently pass |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
