---
phase: 21
slug: schema-mtgjson-foundation
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-10
---

# Phase 21 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (built-in) + insta 1.46 |
| **Config file** | crates/engine/Cargo.toml (add insta, schemars dev-deps) |
| **Quick run command** | `cargo test -p engine -- --lib` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine -- --lib`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green + `cargo clippy --all-targets -- -D warnings`
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 21-01-01 | 01 | 0 | DATA-02 | setup | `cargo test -p engine -- --lib` | ❌ W0 | ⬜ pending |
| 21-01-02 | 01 | 1 | DATA-02 | unit | `cargo test -p engine test_ability_json_roundtrip -x` | ❌ W0 | ⬜ pending |
| 21-01-03 | 01 | 1 | DATA-04 | unit | `cargo test -p engine test_schema_generation -x` | ❌ W0 | ⬜ pending |
| 21-01-04 | 01 | 1 | DATA-02 | unit | `cargo test -p engine test_schema_snapshot -x` | ❌ W0 | ⬜ pending |
| 21-02-01 | 02 | 2 | DATA-01 | integration | `cargo test -p engine test_load_mtgjson_card -x` | ❌ W0 | ⬜ pending |
| 21-02-02 | 02 | 2 | DATA-01 | unit | `cargo test -p engine test_ability_roundtrip -x` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `insta` dev-dependency in `crates/engine/Cargo.toml`
- [ ] `schemars` dependency in `crates/engine/Cargo.toml`
- [ ] `data/mtgjson/` directory with test fixture (small MTGJSON excerpt, not full 50MB)
- [ ] `data/abilities/lightning_bolt.json` — first test ability file
- [ ] Test stubs for all DATA-01, DATA-02, DATA-04 requirements

*Existing infrastructure covers cargo test framework; Wave 0 adds dependencies and test data.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Schema file enables editor autocompletion | DATA-04 | IDE behavior | Open `data/abilities/lightning_bolt.json` in VS Code, verify `$schema` ref provides autocomplete |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
