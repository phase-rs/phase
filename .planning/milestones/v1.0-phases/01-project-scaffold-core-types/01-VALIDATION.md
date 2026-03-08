---
phase: 1
slug: project-scaffold-core-types
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 1 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust), Vitest 4.x (TypeScript) |
| **Config file** | None yet — Wave 0 creates Cargo workspace test config and vitest.config.ts |
| **Quick run command** | `cargo test --lib` (Rust) / `cd client && pnpm test -- --run` (TS) |
| **Full suite command** | `cargo test --all && cd client && pnpm test -- --run` |
| **Estimated runtime** | ~5 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cargo test --lib && cargo clippy -- -D warnings`
- **After every plan wave:** Run `cargo test --all && cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 10 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 01-01-01 | 01 | 1 | SC-1 | smoke | `cargo build && cargo build --package engine-wasm --target wasm32-unknown-unknown` | ❌ W0 | ⬜ pending |
| 01-01-02 | 01 | 1 | PLAT-03 | unit | `cd client && pnpm test -- --run src/adapter/` | ❌ W0 | ⬜ pending |
| 01-02-01 | 02 | 1 | SC-3 | unit | `cargo test --package engine --lib types` | ❌ W0 | ⬜ pending |
| 01-02-02 | 02 | 2 | SC-2 | integration | `cd client && pnpm test -- --run src/wasm-integration.test.ts` | ❌ W0 | ⬜ pending |
| 01-02-03 | 02 | 2 | SC-4 | smoke | Verify `.github/workflows/ci.yml` exists and passes | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] Cargo workspace with test configuration (workspace root + crate Cargo.toml files)
- [ ] `client/vitest.config.ts` — Vitest configuration
- [ ] `client/src/adapter/__tests__/wasm-adapter.test.ts` — adapter unit tests
- [ ] `crates/engine/src/types/` test modules — type serialization tests
- [ ] wasm32-unknown-unknown target installed: `rustup target add wasm32-unknown-unknown`

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| React app renders placeholder with WASM call | SC-2 | Requires browser rendering | `cd client && pnpm dev`, verify placeholder screen loads and displays WASM output |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 10s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
