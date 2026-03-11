---
phase: 29
slug: support-n-players-in-engine-and-on-board-in-react-for-various-formats
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-11
---

# Phase 29 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust) + vitest (TypeScript) |
| **Config file** | Cargo.toml / client/vitest.config.ts |
| **Quick run command** | `cargo test -p engine && cargo test -p phase-ai` |
| **Full suite command** | `cargo test --all && cd client && pnpm test -- --run` |
| **Estimated runtime** | ~60 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p engine && cargo test -p phase-ai`
- **After every plan wave:** Run `cargo test --all && cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 60 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 29-01-01 | 01 | 1 | N-PLAYER-01 | unit | `cargo test -p engine -- new_creates_n_players` | ❌ W0 | ⬜ pending |
| 29-01-02 | 01 | 1 | N-PLAYER-02 | unit | `cargo test -p engine -- priority_clockwise_n_player` | ❌ W0 | ⬜ pending |
| 29-01-03 | 01 | 1 | N-PLAYER-03 | unit | `cargo test -p engine -- turn_rotation_seat_order` | ❌ W0 | ⬜ pending |
| 29-01-04 | 01 | 1 | N-PLAYER-04 | unit | `cargo test -p engine -- elimination_cleanup` | ❌ W0 | ⬜ pending |
| 29-02-01 | 02 | 1 | N-PLAYER-05 | unit | `cargo test -p engine -- per_creature_attack_targets` | ❌ W0 | ⬜ pending |
| 29-02-02 | 02 | 1 | N-PLAYER-06 | unit | `cargo test -p engine -- commander_damage_threshold` | ❌ W0 | ⬜ pending |
| 29-02-03 | 02 | 1 | N-PLAYER-07 | unit | `cargo test -p engine -- commander_tax` | ❌ W0 | ⬜ pending |
| 29-03-01 | 03 | 1 | N-PLAYER-08 | unit | `cargo test -p engine -- deck_validation_format` | ❌ W0 | ⬜ pending |
| 29-04-01 | 04 | 2 | N-PLAYER-09 | unit | `cargo test -p server-core -- filter_n_player_state` | ❌ W0 | ⬜ pending |
| 29-05-01 | 05 | 2 | N-PLAYER-10 | unit | `cargo test -p phase-ai -- threat_eval_n_player` | ❌ W0 | ⬜ pending |
| 29-06-01 | 06 | 2 | N-PLAYER-11 | integration | `cargo test -p engine -- two_player_compat` | ❌ W0 | ⬜ pending |
| 29-07-01 | 07 | 3 | N-PLAYER-12 | unit | `cd client && pnpm test -- --run PlayerArea` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/engine/src/types/format.rs` — FormatConfig type and tests
- [ ] `crates/engine/src/game/players.rs` — next_player, opponents, apnap_order functions with tests
- [ ] `crates/engine/src/game/elimination.rs` — elimination cleanup with tests
- [ ] `crates/engine/src/game/commander.rs` — commander zone, tax, damage tracking with tests
- [ ] `crates/engine/tests/two_player_compat.rs` — 2-player backward compatibility regression tests
- [ ] `client/src/components/board/__tests__/PlayerArea.test.tsx` — PlayerArea mode rendering tests

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Compact opponent strip readability | N-PLAYER-12 | Visual layout judgment | 4-player Commander game, verify all opponent strips are readable at 1080p |
| 1v1 visual parity | N-PLAYER-11 | Visual regression | Compare 2-player game screenshot before/after refactor |
| Game setup format-first UX flow | — | UX evaluation | Walk through format selection → config → lobby → start for each format |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 60s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
