---
phase: 26
slug: polish-and-fix-multi-player-with-lobby-and-embedded-server
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-10
---

# Phase 26 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Vitest (frontend), cargo test (Rust) |
| **Config file** | `client/vitest.config.ts`, inline `#[cfg(test)]` (Rust) |
| **Quick run command** | `cd client && pnpm test -- --run --reporter=verbose` |
| **Full suite command** | `cd client && pnpm test -- --run && cargo test --all` |
| **Estimated runtime** | ~45 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run && cargo test -p server-core`
- **After every plan wave:** Run `cd client && pnpm test -- --run && cargo test --all`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 45 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 26-01-01 | 01 | 1 | BUG-A | unit | `cd client && pnpm test -- --run -t "stale session"` | ❌ W0 | ⬜ pending |
| 26-01-02 | 01 | 1 | BUG-B | unit | `cd client && pnpm test -- --run -t "deck validation"` | ❌ W0 | ⬜ pending |
| 26-01-03 | 01 | 1 | BUG-C | unit | `cd client && pnpm test -- --run -t "unsolicited state"` | ❌ W0 | ⬜ pending |
| 26-01-04 | 01 | 1 | BUG-D | unit | `cd client && pnpm test -- --run -t "getAiAction"` | ❌ W0 | ⬜ pending |
| 26-01-05 | 01 | 1 | BUG-E | unit | `cd client && pnpm test -- --run -t "player id"` | ❌ W0 | ⬜ pending |
| 26-02-01 | 02 | 1 | LOBBY-SRV | unit | `cargo test -p server-core -- lobby` | ❌ W0 | ⬜ pending |
| 26-02-02 | 02 | 1 | LOBBY-UI | unit | `cd client && pnpm test -- --run -t "LobbyView"` | ❌ W0 | ⬜ pending |
| 26-03-01 | 03 | 2 | P2P | unit | `cd client && pnpm test -- --run -t "PeerSession"` | ❌ W0 | ⬜ pending |
| 26-03-02 | 03 | 2 | SIDECAR | manual-only | Manual: requires Tauri desktop build | N/A | ⬜ pending |
| 26-04-01 | 04 | 2 | EMOTE | unit | `cargo test -p server-core -- emote` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/adapter/__tests__/ws-adapter.test.ts` — extend with Bug C/D/E test cases
- [ ] `client/src/network/__tests__/peer.test.ts` — port from Alchemy + adapt
- [ ] `client/src/stores/__tests__/multiplayerStore.test.ts` — new store tests
- [ ] `crates/server-core/src/lobby.rs` — lobby manager with inline `#[cfg(test)]` module
- [ ] Protocol roundtrip tests for new `ClientMessage`/`ServerMessage` variants

*Existing infrastructure covers bug fix tests (A, B); new test files needed for lobby, P2P, and emotes.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Tauri sidecar spawn + health check | SIDECAR | Requires Tauri desktop build with bundled binary | 1. Build desktop app with `pnpm tauri build` 2. Click "Host Game" 3. Verify server process starts on port 8080 4. Verify server stops when game ends |
| P2P WebRTC connection (real network) | P2P | Requires two browser tabs/devices with WebRTC | 1. Host creates P2P game 2. Join from another tab using game code 3. Verify state sync between host and guest |
| Lobby real-time updates | LOBBY-UI | Requires running server + multiple clients | 1. Open lobby on two clients 2. Host a game on client A 3. Verify game appears on client B within 2s |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 45s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
