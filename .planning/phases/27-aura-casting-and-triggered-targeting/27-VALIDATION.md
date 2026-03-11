---
phase: 27
slug: aura-casting-and-triggered-targeting
status: approved
nyquist_compliant: true
wave_0_complete: true
created: 2026-03-11
---

# Phase 27 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust `#[cfg(test)]` modules + integration tests |
| **Config file** | `Cargo.toml` workspace |
| **Quick run command** | `cargo test -p engine -- --test-threads=1` |
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
| 27-01-01 | 01 | 1 | SC-1 (Aura targeting) | integration | `cargo test -p engine aura` | TDD inline | pending |
| 27-01-02 | 01 | 1 | SC-1 (Aura attachment) | integration | `cargo test -p engine aura` | TDD inline | pending |
| 27-01-03 | 01 | 1 | SC-4 (TargetFilter matching) | unit | `cargo test -p engine target_filter` | TDD inline | pending |
| 27-02-01 | 02 | 2 | SC-2 (Trigger targeting) | integration | `cargo test -p engine trigger_target` | TDD inline | pending |
| 27-02-02 | 02 | 2 | SC-3 (Exile return) | integration | `cargo test -p engine exile_return` | TDD inline | pending |
| 27-03-01 | 03 | 3 | SC-5 (All tests pass) | full suite | `cargo test --all` | existing | pending |

*Status: pending / green / red / flaky*

---

## Wave 0 Requirements

All tasks in Plans 01-03 use `tdd="true"` attribute, meaning tests are authored inline as part of the RED-GREEN-REFACTOR cycle during execution. This satisfies the Wave 0 / Nyquist requirement -- no separate test scaffold plan is needed because each task creates its own tests before writing production code.

Existing test infrastructure (GameScenario from Phase 22, `#[cfg(test)]` modules) provides the harness. Card JSON ability files for test cards (Sheltered by Ghosts, Pacifism, Banishing Light) have `execute` field authoring included in Plan 01 Task 2.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Aura visual attachment in UI | SC-1 visual | UI rendering not testable in Rust | Cast an Aura in the browser, verify it visually attaches to target |
| AI selects Aura targets | SC-1 AI | AI behavior varies | Play against AI with Aura cards in deck |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify or Wave 0 dependencies
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 covered by TDD inline authoring (`tdd="true"` on all code-producing tasks)
- [x] No watch-mode flags
- [x] Feedback latency < 30s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** approved
