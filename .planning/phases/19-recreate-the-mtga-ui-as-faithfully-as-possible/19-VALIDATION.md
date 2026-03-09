---
phase: 19
slug: recreate-the-mtga-ui-as-faithfully-as-possible
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-03-09
---

# Phase 19 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Vitest 3.x with jsdom |
| **Config file** | `client/vitest.config.ts` |
| **Quick run command** | `cd client && pnpm test -- --run` |
| **Full suite command** | `cd client && pnpm test -- --run --coverage` |
| **Estimated runtime** | ~15 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd client && pnpm test -- --run`
- **After every plan wave:** Run `cd client && pnpm test -- --run --coverage`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Area | Behavior | Test Type | Automated Command | File Exists | Status |
|------|----------|-----------|-------------------|-------------|--------|
| Image infrastructure | `art_crop` size accepted by scryfall service | unit | `cd client && pnpm test -- --run -t "art_crop"` | ❌ W0 | ⬜ pending |
| Image infrastructure | Dual caching (art_crop + normal) | unit | `cd client && pnpm test -- --run -t "cache"` | ✅ existing | ⬜ pending |
| Card presentation | ArtCropCard renders border color by color identity | unit | `cd client && pnpm test -- --run -t "ArtCropCard"` | ❌ W0 | ⬜ pending |
| Card presentation | Tap rotation uses 17deg not 90deg | unit | `cd client && pnpm test -- --run -t "tap rotation"` | ❌ W0 | ⬜ pending |
| Board layout | Zone order: creatures near center, lands far | unit | `cd client && pnpm test -- --run -t "zone order"` | ❌ W0 | ⬜ pending |
| Animation | Turn banner renders with correct theming | unit | `cd client && pnpm test -- --run -t "TurnBanner"` | ❌ W0 | ⬜ pending |
| Deck builder | Art-crop grid renders | unit | `cd client && pnpm test -- --run -t "CardGrid"` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `client/src/services/__tests__/scryfall.test.ts` — test art_crop size variant
- [ ] `client/src/components/card/__tests__/ArtCropCard.test.tsx` — test border color logic
- [ ] `client/src/components/board/__tests__/tapRotation.test.ts` — verify 17deg rotation
- [ ] `client/src/viewmodel/__tests__/battlefieldLayout.test.ts` — verify zone ordering
- [ ] `client/src/components/phase/__tests__/TurnBanner.test.tsx` — test turn banner theming
- [ ] `client/src/components/card/__tests__/CardGrid.test.tsx` — test deck builder art-crop grid

*Existing infrastructure covers caching (imageCache tests) and scryfall service basics.*

---

## Manual-Only Verifications

| Behavior | Why Manual | Test Instructions |
|----------|------------|-------------------|
| Menu mode-first flow navigation | UI flow / routing integration | Navigate main menu → mode selection → deck gallery → game start |
| MTGA-faithful board layout visual fidelity | Visual spacing and positioning | Compare board layout against MTGA screenshot reference |
| Death shatter animation quality | Canvas 2D visual effect | Kill a creature, verify fragment scatter + particle burst |
| Coin flip / play-draw animation | Visual-only animation | Start new game, verify animated reveal |
| Game over screen drama | Visual effect quality | Win/lose game, verify gold/red overlay + particles |
| Splash screen loading experience | First-load timing | Hard refresh, verify logo + loading bar during WASM init |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
