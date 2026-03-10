---
phase: 23
slug: unified-card-loader
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-10
---

# Phase 23 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust std test + insta (snapshot) |
| **Config file** | Cargo workspace test configuration |
| **Quick run command** | `cargo test -p engine -- json_loader` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine -- json_loader`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 23-01-01 | 01 | 1 | DATA-03 | unit | `cargo test -p engine -- json_loader -x` | ❌ W0 | ⬜ pending |
| 23-01-02 | 01 | 1 | DATA-03 | unit | `cargo test -p engine -- json_loader::multi_face -x` | ❌ W0 | ⬜ pending |
| 23-01-03 | 01 | 1 | DATA-03 | unit | `cargo test -p engine -- json_loader::basic_land -x` | ❌ W0 | ⬜ pending |
| 23-01-04 | 01 | 1 | DATA-03 | unit | `cargo test -p engine -- json_loader::equipment -x` | ❌ W0 | ⬜ pending |
| 23-01-05 | 01 | 1 | DATA-03 | unit | `cargo test -p engine -- json_loader::planeswalker -x` | ❌ W0 | ⬜ pending |
| 23-01-06 | 01 | 1 | DATA-03 | unit | `cargo test -p engine -- json_loader::missing -x` | ❌ W0 | ⬜ pending |
| 23-01-07 | 01 | 1 | DATA-03 | unit | `cargo test -p engine -- cross_validation -x` | ❌ W0 | ⬜ pending |
| 23-01-08 | 01 | 1 | MIGR-04 | unit | `cargo test -p engine -- json_loader::oracle_id -x` | ❌ W0 | ⬜ pending |
| 23-02-01 | 02 | 2 | DATA-03 | integration | `cargo test -p engine -- smoke_test_json -x` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/database/json_loader.rs` — new module with merge logic + tests
- [ ] `data/abilities/` — 8 new ability JSON files for smoke test cards
- [ ] `data/mtgjson/test_fixture.json` — extended with smoke test card entries
- [ ] Smoke test in integration tests or `json_loader` module

*Existing test infrastructure (cargo test, Cargo workspace) covers framework needs.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Frontend displays card images via oracle ID | MIGR-04 | Requires browser + Scryfall API | Load a JSON card in dev server, verify image loads via oracle ID |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
