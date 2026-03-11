---
phase: 28
slug: native-ability-data-model
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-10
---

# Phase 28 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in `#[test]` + insta 1.x for snapshots |
| **Config file** | Cargo.toml (workspace-level test config) |
| **Quick run command** | `cargo test -p engine` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 28-01-01 | 01 | 1 | NAT-01 | unit | `cargo test -p engine types::ability::tests` | Partial | ⬜ pending |
| 28-01-02 | 01 | 1 | NAT-04 | unit | `cargo test -p engine game::filter::tests` | Yes (rewrite) | ⬜ pending |
| 28-01-03 | 01 | 1 | NAT-01 | unit | `cargo test -p engine types::ability::tests` | ❌ W0 | ⬜ pending |
| 28-02-01 | 02 | 2 | NAT-02 | unit+integration | `cargo test -p engine game::triggers::tests` | Partial | ⬜ pending |
| 28-02-02 | 02 | 2 | NAT-03 | unit | `cargo test -p engine -- remaining_params` | Partial | ⬜ pending |
| 28-02-03 | 02 | 2 | NAT-01 | integration | `cargo test -p engine game::layers::tests` | Partial | ⬜ pending |
| 28-03-01 | 03 | 3 | NAT-05 | unit | `cargo test -p engine` (compile without forge-compat) | ❌ W0 | ⬜ pending |
| 28-03-02 | 03 | 3 | NAT-06 | integration | `cargo test -p engine database::json_loader::tests` | Partial | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] New test for typed TriggerDefinition construction and serialization roundtrip
- [ ] New test for typed StaticDefinition with ContinuousModification
- [ ] New test for TargetFilter matching (typed replacement for filter.rs tests)
- [ ] New test for SubAbility chain resolution without SVar lookup
- [ ] Migration binary tests (old format → new format → round-trip)
- [ ] Compile-without-forge-compat test to verify parse_ability() gating

*Existing infrastructure covers most requirements; Wave 0 fills gaps for new types and migration.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| 32K JSON files all valid after migration | NAT-06 | Bulk file I/O + visual spot-check | Run migration binary, load card-data.json, spot-check 10 random cards |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
