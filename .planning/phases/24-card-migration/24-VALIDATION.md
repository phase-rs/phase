---
phase: 24
slug: card-migration
status: draft
nyquist_compliant: true
wave_0_complete: false
created: 2026-03-10
---

# Phase 24 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test + cargo integration tests |
| **Config file** | `Cargo.toml` [[test]] entries (auto-detected by cargo) |
| **Quick run command** | `cargo test --test parity` |
| **Full suite command** | `cargo test --all` |
| **Estimated runtime** | ~120 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine && cargo test --test parity`
- **After every plan wave:** Run `cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 120 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | Status |
|---------|------|------|-------------|-----------|-------------------|--------|
| 24-01-T1 | 01 | 1 | MIGR-01 | unit | `cargo test -p engine -- parse_cost` | pending |
| 24-01-T2 | 01 | 1 | MIGR-03 | integration (binary run) | `cargo build --bin migrate && cargo run --bin migrate 2>&1 \| tail -10 && ls data/abilities/*.json \| wc -l` | pending |
| 24-02-T1 | 02 | 2 | TEST-04 | data validation | `wc -l data/standard-cards.txt && python3 -c "import json; d=json.load(open('data/mtgjson/test_fixture.json')); print(f'entries: {len(d[\"data\"])}')"` | pending |
| 24-02-T2 | 02 | 2 | TEST-04 | integration | `cargo test --test parity` | pending |
| 24-03-T1 | 03 | 3 | MIGR-05 | integration (binary run) | `cargo run --bin coverage-report -- --json data/ --ci` | pending |
| 24-03-T2 | 03 | 3 | MIGR-05 | config validation | `grep -A2 "coverage gate" .github/workflows/ci.yml` | pending |

*Status: pending / green / red / flaky*

---

## Wave 0 Requirements

Wave 0 stubs are NOT needed for this phase. Rationale:

- **Plan 01 (Wave 1)** creates the migration tool binary and enhanced cost parser. Its verification relies on binary compilation, execution output, and file count checks -- not on pre-existing test stubs. The `parse_cost` unit tests are created within the same task that implements the parser.
- **Plan 02 (Wave 2)** creates `parity.rs`, `standard-cards.txt`, and the expanded `test_fixture.json`. These are all new files created together -- no prior stub is needed since Plan 02's Task 1 creates the data files before Task 2 creates the test that consumes them.
- **Plan 03 (Wave 3)** depends on Plans 01 and 02, so all test artifacts exist by the time it executes.

*No Wave 0 stubs required.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Migration tool processes all 32,300 cards | MIGR-03 | Full run takes ~minutes, not suitable for CI quick check | Run `cargo run --bin migrate -- data/cardsfolder data/abilities` and verify summary stats |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify commands
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 requirements analyzed and documented (none needed)
- [x] No watch-mode flags
- [x] Feedback latency < 120s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
