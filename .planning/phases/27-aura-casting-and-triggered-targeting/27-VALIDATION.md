---
phase: 27
slug: aura-casting-and-triggered-targeting
status: draft
nyquist_compliant: false
wave_0_complete: false
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
| 27-01-01 | 01 | 1 | SC-1 (Aura targeting) | integration | `cargo test -p engine aura` | ❌ W0 | ⬜ pending |
| 27-01-02 | 01 | 1 | SC-1 (Aura attachment) | integration | `cargo test -p engine aura` | ❌ W0 | ⬜ pending |
| 27-01-03 | 01 | 1 | SC-4 (TargetFilter matching) | unit | `cargo test -p engine target_filter` | ❌ W0 | ⬜ pending |
| 27-02-01 | 02 | 2 | SC-2 (Trigger targeting) | integration | `cargo test -p engine trigger_target` | ❌ W0 | ⬜ pending |
| 27-02-02 | 02 | 2 | SC-3 (Exile return) | integration | `cargo test -p engine exile_return` | ❌ W0 | ⬜ pending |
| 27-03-01 | 03 | 3 | SC-5 (All tests pass) | full suite | `cargo test --all` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- Existing test infrastructure covers all phase requirements (GameScenario from Phase 22)
- Card JSON ability files for test cards (Sheltered by Ghosts, Pacifism, Banishing Light) need manual `execute` field authoring

*Existing infrastructure covers all phase requirements.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Aura visual attachment in UI | SC-1 visual | UI rendering not testable in Rust | Cast an Aura in the browser, verify it visually attaches to target |
| AI selects Aura targets | SC-1 AI | AI behavior varies | Play against AI with Aura cards in deck |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
