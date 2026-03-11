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
| **Framework** | Rust `#[cfg(test)]` + cargo test |
| **Config file** | `Cargo.toml` (workspace) |
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
| 28-01-01 | 01 | 1 | SC-1 | unit | `cargo test -p engine` | ✅ | ⬜ pending |
| 28-02-01 | 02 | 1 | SC-2 | unit | `cargo test -p engine` | ✅ | ⬜ pending |
| 28-03-01 | 03 | 2 | SC-3 | unit | `cargo test -p engine` | ✅ | ⬜ pending |
| 28-04-01 | 04 | 2 | SC-4 | integration | `cargo test --all` | ✅ | ⬜ pending |
| 28-05-01 | 05 | 3 | SC-5 | integration | `cargo test --all` | ✅ | ⬜ pending |
| 28-06-01 | 06 | 3 | SC-6 | integration | `cargo test --all` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

*Existing infrastructure covers all phase requirements.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| card-data.json format correct | SC-6 | Spot-check specific cards | `python3 -c "import json; d=json.load(open('client/public/card-data.json')); c=d['Lightning Bolt']; assert 'svars' not in c; assert 'remaining_params' not in str(c)"` |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
