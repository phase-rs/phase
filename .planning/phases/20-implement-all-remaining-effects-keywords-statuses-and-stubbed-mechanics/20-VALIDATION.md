---
phase: 20
slug: implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-09
---

# Phase 20 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust inline `#[cfg(test)]` + cargo test; Vitest for frontend |
| **Config file** | Cargo.toml workspace (existing), client/vitest.config.ts |
| **Quick run command** | `cargo test -p engine -- {test_prefix} -x` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~30 seconds (Rust), ~10 seconds (frontend) |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine -- {relevant_test_prefix} -x`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green + coverage report shows 100% Standard
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 20-01-01 | 01 | 1 | Mana abilities | unit | `cargo test -p engine -- test_mana_ability -x` | ❌ W0 | ⬜ pending |
| 20-01-02 | 01 | 1 | Equipment/Aura | unit | `cargo test -p engine -- test_equip -x` | ❌ W0 | ⬜ pending |
| 20-01-03 | 01 | 1 | WaitingFor choices | unit | `cargo test -p engine -- test_scry_choice -x` | ❌ W0 | ⬜ pending |
| 20-02-01 | 02 | 2 | Planeswalker loyalty | unit | `cargo test -p engine -- test_planeswalker -x` | ❌ W0 | ⬜ pending |
| 20-02-02 | 02 | 2 | Transform/DFC | unit | `cargo test -p engine -- test_transform -x` | ❌ W0 | ⬜ pending |
| 20-03-01 | 03 | 3 | Static ability promotions | unit | `cargo test -p engine -- test_static -x` | Partial | ⬜ pending |
| 20-04-01 | 04 | 3 | Trigger promotions | unit | `cargo test -p engine -- test_trigger -x` | Partial | ⬜ pending |
| 20-05-01 | 05 | 4 | Effect handler promotions | unit | `cargo test -p engine -- test_effect -x` | Partial | ⬜ pending |
| 20-06-01 | 06 | 4 | Replacement promotions | unit | `cargo test -p engine -- test_replacement -x` | Partial | ⬜ pending |
| 20-07-01 | 07 | 5 | Day/Night + Morph | unit | `cargo test -p engine -- test_daynight -x` | ❌ W0 | ⬜ pending |
| 20-08-01 | 08 | 6 | Coverage CI gate | integration | `cargo run --bin coverage_report` | Exists | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] Standard-legal card subset data file — curate and check into repo for CI gating
- [ ] Integration tests for mana abilities with real Forge cards (extend test_helpers.rs)
- [ ] Integration tests for equipment with real Forge cards
- [ ] Frontend test stubs for ScryChoice component (Vitest)

*Wave 0 is embedded in Plan 1 — baseline coverage + test infrastructure setup.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Scry UI top/bottom buttons | WaitingFor choices | Visual layout verification | Play a Scry card, verify per-card top/bottom buttons appear MTGA-style |
| DFC hover-to-peek back face | Transform/DFC | Visual interaction | Hover over a DFC on battlefield, verify back face preview appears |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
