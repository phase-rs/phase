---
phase: 2
slug: card-parser-database
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 2 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in test framework (`#[cfg(test)]` / `#[test]`) |
| **Config file** | None needed — Cargo handles test discovery |
| **Quick run command** | `cargo test -p engine --lib` |
| **Full suite command** | `cargo test -p engine` |
| **Estimated runtime** | ~5 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine --lib`
- **After every plan wave:** Run `cargo test -p engine`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 5 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 02-01-01 | 01 | 1 | PARSE-01 | unit | `cargo test -p engine parser::card_parser::tests::parse_lightning_bolt` | ❌ W0 | ⬜ pending |
| 02-01-02 | 01 | 1 | PARSE-01 | unit | `cargo test -p engine parser::card_parser::tests::parse_all_keys` | ❌ W0 | ⬜ pending |
| 02-01-03 | 01 | 1 | PARSE-01 | unit | `cargo test -p engine parser::card_parser::tests::skip_comments` | ❌ W0 | ⬜ pending |
| 02-01-04 | 01 | 1 | PARSE-01 | unit | `cargo test -p engine parser::card_parser::tests::skip_unknown_keys` | ❌ W0 | ⬜ pending |
| 02-01-05 | 01 | 1 | PARSE-04 | unit | `cargo test -p engine parser::mana_cost::tests` | ❌ W0 | ⬜ pending |
| 02-01-06 | 01 | 1 | PARSE-04 | unit | `cargo test -p engine parser::card_type::tests` | ❌ W0 | ⬜ pending |
| 02-01-07 | 01 | 1 | ABIL-01 | unit | `cargo test -p engine parser::ability::tests::parse_spell` | ❌ W0 | ⬜ pending |
| 02-01-08 | 01 | 1 | ABIL-01 | unit | `cargo test -p engine parser::ability::tests::parse_activated` | ❌ W0 | ⬜ pending |
| 02-01-09 | 01 | 1 | ABIL-01 | unit | `cargo test -p engine parser::ability::tests::parse_trigger` | ❌ W0 | ⬜ pending |
| 02-01-10 | 01 | 1 | ABIL-01 | unit | `cargo test -p engine parser::ability::tests::parse_replacement` | ❌ W0 | ⬜ pending |
| 02-02-01 | 02 | 1 | PARSE-02 | unit | `cargo test -p engine parser::card_parser::tests::parse_split` | ❌ W0 | ⬜ pending |
| 02-02-02 | 02 | 1 | PARSE-02 | unit | `cargo test -p engine parser::card_parser::tests::parse_transform` | ❌ W0 | ⬜ pending |
| 02-02-03 | 02 | 1 | PARSE-02 | unit | `cargo test -p engine parser::card_parser::tests::parse_mdfc` | ❌ W0 | ⬜ pending |
| 02-02-04 | 02 | 1 | PARSE-02 | unit | `cargo test -p engine parser::card_parser::tests::parse_adventure` | ❌ W0 | ⬜ pending |
| 02-02-05 | 02 | 1 | PARSE-02 | unit | `cargo test -p engine parser::card_parser::tests::parse_flip` | ❌ W0 | ⬜ pending |
| 02-02-06 | 02 | 1 | PARSE-02 | unit | `cargo test -p engine parser::card_parser::tests::parse_meld` | ❌ W0 | ⬜ pending |
| 02-02-07 | 02 | 2 | PARSE-03 | integration | `cargo test -p engine database::tests::load_and_lookup` | ❌ W0 | ⬜ pending |
| 02-02-08 | 02 | 2 | PARSE-03 | integration | `cargo test -p engine database::tests::face_lookup` | ❌ W0 | ⬜ pending |
| 02-02-09 | 02 | 2 | PARSE-03 | unit | `cargo test -p engine database::tests::case_insensitive` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/parser/` — new parser module (card_parser, mana_cost, card_type, ability submodules)
- [ ] `crates/engine/src/database/` — new database module
- [ ] `crates/engine/src/types/card_type.rs` — CardType with supertypes/types/subtypes
- [ ] `crates/engine/src/types/ability.rs` — AbilityDefinition, AbilityKind
- [ ] Test fixtures: embedded sample card file contents as `const` strings
- [ ] `walkdir` and `thiserror` dependencies in engine Cargo.toml

*Wave 0 tasks create these files with test stubs before implementation begins.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| 10ms lookup benchmark | PARSE-03 | Perf varies by machine | Run `cargo bench` or timed integration test; verify < 10ms |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 5s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
