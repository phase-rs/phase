---
phase: 16
slug: audio-system
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-09
---

# Phase 16 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Vitest 3.x + jsdom |
| **Config file** | `client/vitest.config.ts` |
| **Quick run command** | `cd client && pnpm test -- --run --reporter=verbose` |
| **Full suite command** | `cd client && pnpm test -- --run` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run --reporter=verbose`
- **After every plan wave:** Run `cd client && pnpm test -- --run`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|-----------|-------------------|-------------|--------|
| 16-01-01 | 01 | 1 | AUDIO-04 | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | ✅ (extend) | ⬜ pending |
| 16-01-02 | 01 | 1 | AUDIO-01 | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | ❌ W0 | ⬜ pending |
| 16-01-03 | 01 | 1 | AUDIO-05 | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | ❌ W0 | ⬜ pending |
| 16-02-01 | 02 | 1 | AUDIO-01 | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | ❌ W0 | ⬜ pending |
| 16-02-02 | 02 | 1 | AUDIO-01 | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | ❌ W0 | ⬜ pending |
| 16-03-01 | 03 | 2 | AUDIO-02 | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | ❌ W0 | ⬜ pending |
| 16-03-02 | 03 | 2 | AUDIO-03 | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | ❌ W0 | ⬜ pending |
| 16-04-01 | 04 | 2 | AUDIO-04 | unit | `cd client && pnpm test -- --run src/components/__tests__/PreferencesModal.test.ts` | ❌ W0 | ⬜ pending |
| 16-04-02 | 04 | 2 | AUDIO-04 | unit | `cd client && pnpm test -- --run src/components/__tests__/PlayerHud.test.ts` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/audio/__tests__/AudioManager.test.ts` — stubs for AUDIO-01, AUDIO-02, AUDIO-03, AUDIO-05
- [ ] AudioContext mock setup (jsdom doesn't provide AudioContext natively)
- [ ] Extend `client/src/stores/__tests__/preferencesStore.test.ts` — stubs for AUDIO-04 audio preference fields

*Existing Vitest infrastructure covers framework setup.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| SFX plays audibly on game events | AUDIO-01 | Requires actual audio output hardware | Play a game, verify sounds on damage, spell cast, combat |
| Music crossfade sounds smooth | AUDIO-02 | Subjective audio quality | Wait for track transition, listen for smooth crossfade |
| iOS AudioContext warm-up works | AUDIO-05 | Requires real iOS device | Open on iOS Safari, tap to interact, verify audio plays |
| Volume sliders feel responsive | AUDIO-04 | UX feel check | Adjust sliders in PreferencesModal, verify volume changes |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
