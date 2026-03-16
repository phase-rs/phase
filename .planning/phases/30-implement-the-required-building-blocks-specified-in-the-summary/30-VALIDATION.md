---
phase: 30
slug: implement-the-required-building-blocks-specified-in-the-summary
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-16
---

# Phase 30 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust `#[cfg(test)]` + cargo test (engine), Vitest (frontend) |
| **Config file** | `Cargo.toml` (workspace), `client/vitest.config.ts` |
| **Quick run command** | `cargo test -p engine -- --test-threads=4` |
| **Full suite command** | `cargo test --all && cd client && pnpm test -- --run` |
| **Estimated runtime** | ~30 seconds (Rust) + ~15 seconds (frontend) |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine -- --test-threads=4`
- **After every plan wave:** Run `cargo test --all && cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 45 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 30-01-01 | 01 | 1 | Event-context TargetFilter | unit | `cargo test -p engine target_filter` | ❌ W0 | ⬜ pending |
| 30-01-02 | 01 | 1 | Trigger event threading | unit | `cargo test -p engine trigger_event` | ❌ W0 | ⬜ pending |
| 30-02-01 | 02 | 1 | Parser possessive refs | unit | `cargo test -p engine oracle_target` | ✅ | ⬜ pending |
| 30-03-01 | 03 | 1 | GameRestriction enum | unit | `cargo test -p engine restriction` | ❌ W0 | ⬜ pending |
| 30-03-02 | 03 | 2 | Prevention pipeline gate | integration | `cargo test -p engine replacement` | ✅ | ⬜ pending |
| 30-04-01 | 04 | 3 | Adventure casting | scenario | `cargo test -p engine adventure` | ❌ W0 | ⬜ pending |
| 30-04-02 | 04 | 3 | Adventure frontend | manual + unit | `cd client && pnpm test -- --run` | ❌ W0 | ⬜ pending |
| 30-05-01 | 05 | 3 | Integration (Bonecrusher) | scenario | `cargo test -p engine bonecrusher` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] Scenario test stubs for event-context targeting, prevention disabling, Adventure casting
- [ ] Integration test stub for Bonecrusher Giant end-to-end

*Existing test infrastructure (cargo test, GameScenario harness, Vitest) covers all framework needs.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Adventure card rendering | Frontend display | Visual verification | Cast Adventure card, verify both faces render correctly |
| Casting choice modal | Frontend UX | User interaction | Verify modal appears with creature/Adventure options |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 45s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
