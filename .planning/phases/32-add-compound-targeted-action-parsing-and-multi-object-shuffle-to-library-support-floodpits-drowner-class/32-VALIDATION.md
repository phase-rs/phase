---
phase: 32
slug: add-compound-targeted-action-parsing-and-multi-object-shuffle-to-library-support-floodpits-drowner-class
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-17
---

# Phase 32 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust `#[cfg(test)]` inline modules + `cargo test` |
| **Config file** | `Cargo.toml` (workspace) |
| **Quick run command** | `cargo test -p engine -- --test-threads=4` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine -- --test-threads=4`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 32-01-01 | 01 | 1 | HasCounter FilterProp | unit | `cargo test -p engine filter` | ✅ | ⬜ pending |
| 32-01-02 | 01 | 1 | parse_counter_filter | unit | `cargo test -p engine parser` | ✅ | ⬜ pending |
| 32-02-01 | 02 | 1 | try_split_compound | unit | `cargo test -p engine parser` | ✅ | ⬜ pending |
| 32-02-02 | 02 | 1 | ParentTarget resolution | unit | `cargo test -p engine target` | ✅ | ⬜ pending |
| 32-03-01 | 03 | 2 | compound subject splitter | unit | `cargo test -p engine parser` | ✅ | ⬜ pending |
| 32-03-02 | 03 | 2 | ChangeZone auto-shuffle | unit | `cargo test -p engine effects` | ✅ | ⬜ pending |
| 32-04-01 | 04 | 3 | Floodpits Drowner integration | integration | `cargo test -p engine floodpits` | ❌ W0 | ⬜ pending |
| 32-04-02 | 04 | 3 | Coverage delta | script | `cargo coverage` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] Integration test scaffolding for Floodpits Drowner GameScenario — stubs created during wave 2/3

*Existing infrastructure covers all unit test requirements.*

---

## Manual-Only Verifications

*All phase behaviors have automated verification.*

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
