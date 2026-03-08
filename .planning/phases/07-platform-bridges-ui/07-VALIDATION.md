---
phase: 7
slug: platform-bridges-ui
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-07
---

# Phase 7 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Vitest 3.x |
| **Config file** | `client/vite.config.ts` |
| **Quick run command** | `cd client && pnpm test -- --run` |
| **Full suite command** | `cd client && pnpm test -- --run` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run`
- **After every plan wave:** Run `cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 07-01-01 | 01 | 0 | UI-01 | unit | `cd client && pnpm test -- --run src/stores/` | ❌ W0 | ⬜ pending |
| 07-01-02 | 01 | 0 | UI-02 | unit | `cd client && pnpm test -- --run src/stores/` | ❌ W0 | ⬜ pending |
| 07-01-03 | 01 | 0 | PLAT-04 | unit | `cd client && pnpm test -- --run src/services/` | ❌ W0 | ⬜ pending |
| 07-01-04 | 01 | 0 | DECK-02 | unit | `cd client && pnpm test -- --run src/services/` | ❌ W0 | ⬜ pending |
| 07-02-01 | 02 | 1 | UI-01 | unit | `cd client && pnpm test -- --run src/components/board/` | ❌ W0 | ⬜ pending |
| 07-02-02 | 02 | 1 | UI-02 | unit | `cd client && pnpm test -- --run src/components/hand/` | ❌ W0 | ⬜ pending |
| 07-02-03 | 02 | 1 | UI-03 | unit | `cd client && pnpm test -- --run src/components/stack/` | ❌ W0 | ⬜ pending |
| 07-02-04 | 02 | 1 | UI-04 | unit | `cd client && pnpm test -- --run src/components/controls/` | ❌ W0 | ⬜ pending |
| 07-02-05 | 02 | 1 | UI-05 | unit | `cd client && pnpm test -- --run src/components/controls/` | ❌ W0 | ⬜ pending |
| 07-02-06 | 02 | 1 | UI-06 | unit | `cd client && pnpm test -- --run src/components/targeting/` | ❌ W0 | ⬜ pending |
| 07-02-07 | 02 | 1 | UI-07 | unit | `cd client && pnpm test -- --run src/components/mana/` | ❌ W0 | ⬜ pending |
| 07-02-08 | 02 | 1 | UI-08 | unit | `cd client && pnpm test -- --run src/components/card/` | ❌ W0 | ⬜ pending |
| 07-02-09 | 02 | 1 | UI-09 | unit | `cd client && pnpm test -- --run src/components/modal/` | ❌ W0 | ⬜ pending |
| 07-02-10 | 02 | 1 | UI-10 | unit | `cd client && pnpm test -- --run src/components/log/` | ❌ W0 | ⬜ pending |
| 07-02-11 | 02 | 1 | UI-11 | unit | `cd client && pnpm test -- --run src/components/board/` | ❌ W0 | ⬜ pending |
| 07-03-01 | 03 | 2 | DECK-01 | unit | `cd client && pnpm test -- --run src/components/deck-builder/` | ❌ W0 | ⬜ pending |
| 07-03-02 | 03 | 2 | DECK-03 | unit | `cd client && pnpm test -- --run src/components/deck-builder/` | ❌ W0 | ⬜ pending |
| 07-04-01 | 04 | 3 | PLAT-01 | unit | `cd client && pnpm test -- --run src/adapter/` | ❌ W0 | ⬜ pending |
| 07-04-02 | 04 | 3 | PLAT-02 | smoke | Manual — `pnpm build && pnpm preview` | N/A | ⬜ pending |
| 07-04-03 | 04 | 3 | QOL-01 | unit | `cd client && pnpm test -- --run src/stores/` | ❌ W0 | ⬜ pending |
| 07-04-04 | 04 | 3 | QOL-02 | unit | `cd client && pnpm test -- --run src/hooks/` | ❌ W0 | ⬜ pending |
| 07-04-05 | 04 | 3 | QOL-03 | unit | `cd client && pnpm test -- --run src/components/` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/stores/__tests__/gameStore.test.ts` — stubs for UI-01, UI-02 (state management)
- [ ] `client/src/stores/__tests__/uiStore.test.ts` — stubs for UI state (selection, targeting, hover)
- [ ] `client/src/services/__tests__/imageCache.test.ts` — stubs for PLAT-04 (IndexedDB caching)
- [ ] `client/src/services/__tests__/deckParser.test.ts` — stubs for DECK-02 (.dck/.dec parsing)
- [ ] Zustand + Testing Library React test fixtures (already installed)

*If none: "Existing infrastructure covers all phase requirements."*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| PWA installable in browser | PLAT-02 | Browser install prompt is runtime-only | Build app, open in Chrome, verify install prompt appears |
| Touch responsiveness on tablet | UI-11 | Requires physical touch device or emulator | Open in Chrome DevTools device mode, test long-press and tap targets |
| Tauri desktop launch | PLAT-01 | Requires native build | Run `cargo tauri dev`, verify window opens with game UI |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
