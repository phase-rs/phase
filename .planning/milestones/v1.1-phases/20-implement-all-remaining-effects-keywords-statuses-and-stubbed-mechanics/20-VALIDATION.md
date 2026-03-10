---
phase: 20
slug: implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
status: draft
nyquist_compliant: true
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
| 20-01-T1 | 01 | 1 | Mana ability detection + resolution | unit | `cargo test -p engine -- mana_abilit -x` | W0 | pending |
| 20-01-T2 | 01 | 1 | Mana ability engine wiring | unit | `cargo test -p engine -- test_mana -x && cargo test --all` | W0 | pending |
| 20-02-T1 | 02 | 1 | Equipment/Aura attach + equip action | unit | `cargo test -p engine -- test_equip -x && cargo test -p engine -- test_attach -x` | W0 | pending |
| 20-02-T2 | 02 | 1 | SBA for equipment + TS types | unit | `cargo test -p engine -- sba -x && cargo test --all` | W0 | pending |
| 20-03-T1 | 03 | 1 | WaitingFor ScryChoice/DigChoice/SurveilChoice | unit | `cargo test -p engine -- test_scry -x && cargo test -p engine -- test_dig -x && cargo test -p engine -- test_surveil -x` | W0 | pending |
| 20-03-T2 | 03 | 1 | WASM bridge + CardChoiceModal + AI eval | type-check | `cd client && pnpm run type-check && cd .. && cargo test --all` | W0 | pending |
| 20-04-T1 | 04 | 2 | Planeswalker loyalty activation | unit | `cargo test -p engine -- planeswalker -x` | W0 | pending |
| 20-04-T2 | 04 | 2 | SBA 0 loyalty + damage redirect | unit | `cargo test -p engine -- planeswalker -x && cargo test -p engine -- sba -x && cargo test --all` | W0 | pending |
| 20-05-T1 | 05 | 2 | Transform face switching + zone reset | unit | `cargo test -p engine -- transform -x` | W0 | pending |
| 20-05-T2 | 05 | 2 | Transform engine wiring + TS types + UI | type-check | `cd client && pnpm run type-check && cd .. && cargo test --all` | W0 | pending |
| 20-06-T1 | 06 | 3 | Static ability stub promotions | unit | `cargo test -p engine -- test_static -x && cargo test -p engine -- static_abilit -x` | Partial | pending |
| 20-06-T2 | 06 | 3 | Trigger matcher stub promotions | unit | `cargo test -p engine -- trigger -x && cargo test --all` | Partial | pending |
| 20-07-T1 | 07 | 3 | Effect handlers (Fight, Bounce, Explore, etc.) | unit | `cargo test -p engine -- test_fight -x && cargo test -p engine -- test_bounce -x && cargo test -p engine -- test_explore -x && cargo test -p engine -- test_proliferate -x` | W0 | pending |
| 20-07-T2 | 07 | 3 | Replacement effect promotions | unit | `cargo test -p engine -- replacement -x && cargo test --all` | Partial | pending |
| 20-08-T1 | 08 | 4 | Day/Night state + spell tracking | unit | `cargo test -p engine -- day_night -x && cargo test -p engine -- spells_cast -x` | W0 | pending |
| 20-08-T2 | 08 | 4 | Day/Night transition + auto-transform | unit | `cargo test -p engine -- day_night -x && cargo test -p engine -- test_daynight -x && cargo test --all` | W0 | pending |
| 20-09-T1 | 09 | 4 | Morph face-down play + turn face up | unit | `cargo test -p engine -- morph -x && cargo test -p engine -- face_down -x` | W0 | pending |
| 20-09-T2 | 09 | 4 | Manifest + engine wiring + trigger + TS types | unit | `cargo test -p engine -- morph -x && cargo test -p engine -- manifest -x && cd client && pnpm run type-check && cd .. && cargo test --all` | W0 | pending |
| 20-10-T1 | 10 | 5 | Standard card data + coverage report CI flag | integration | `cargo run --bin coverage_report -- data/standard-cards/ --ci` | Partial | pending |
| 20-10-T2 | 10 | 5 | CI workflow coverage gate | integration | `cargo test --all && cargo run --bin coverage_report -- data/standard-cards/ --ci` | Exists | pending |

*Status: pending | green | red | flaky*

---

## Wave 0 Requirements

- [ ] Standard-legal card subset data file -- curate and check into repo for CI gating
- [ ] Integration tests for mana abilities with real Forge cards (extend test_helpers.rs)
- [ ] Integration tests for equipment with real Forge cards
- [ ] Frontend test stubs for ScryChoice component (Vitest)

*Wave 0 is embedded in Plan 1 -- baseline coverage + test infrastructure setup.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Scry UI top/bottom buttons | WaitingFor choices | Visual layout verification | Play a Scry card, verify per-card top/bottom buttons appear MTGA-style |
| DFC hover-to-peek back face | Transform/DFC | Visual interaction | Hover over a DFC on battlefield, verify back face preview appears |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify or Wave 0 dependencies
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 covers all MISSING references
- [x] No watch-mode flags
- [x] Feedback latency < 30s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
