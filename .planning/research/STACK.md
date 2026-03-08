# Technology Stack

**Project:** Forge.rs v1.1 -- Arena UI Port from Alchemy
**Researched:** 2026-03-08

## Stack Comparison: Alchemy vs Forge.rs

### Shared Stack (Already Aligned)

Both projects use the same core stack. These require NO changes:

| Technology | Alchemy Version | Forge.rs Version | Status |
|------------|----------------|-----------------|--------|
| React | ^19.2.0 | ^19.0.0 | Aligned (minor semver bump fine) |
| Zustand | ^5.0.11 | ^5.0.11 | Identical |
| `subscribeWithSelector` | Used in 6 stores | Used in gameStore | Aligned -- Forge.rs already uses this middleware |
| Framer Motion | ^12.34.3 | ^12.35.1 | Aligned (Forge.rs actually newer) |
| Tailwind CSS v4 | ^4.2.1 | ^4.2.1 | Identical |
| `@tailwindcss/vite` | ^4.2.1 | ^4.2.1 | Identical |
| Vite | ^7.3.1 | ^6.2.0 | **Update needed** (see below) |
| `vite-plugin-pwa` | ^1.2.0 | ^1.2.0 | Identical |
| React Router | ^7.13.1 (`react-router-dom`) | ^7.13.1 (`react-router`) | Aligned (same package, different entry point) |
| Vitest | ^4.0.18 | ^3.0.0 | **Update needed** (see below) |
| TypeScript | ~5.9.3 | ~5.7.0 | **Update needed** (see below) |
| `@vitejs/plugin-react` | ^5.1.1 | ^4.4.1 | **Update needed** (see below) |
| ESLint | ^9.39.1 | ^9.21.0 | Minor update, not blocking |

### Version Gaps to Close

These are version bumps on existing dependencies. Update during port setup.

| Package | Current | Target | Why |
|---------|---------|--------|-----|
| `vite` | ^6.2.0 | ^7.3.1 | Alchemy uses Vite 7; aligns build tooling, avoids plugin compat issues with `@vitejs/plugin-react` v5 |
| `@vitejs/plugin-react` | ^4.4.1 | ^5.1.1 | Required by Vite 7 |
| `typescript` | ~5.7.0 | ~5.9.3 | Alchemy targets 5.9; newer type inference helps with discriminated union patterns |
| `vitest` | ^3.0.0 | ^4.0.18 | Alchemy uses Vitest 4; aligns test runner |
| `jsdom` | ^26.0.0 | ^28.1.0 | Test environment alignment |
| `@testing-library/react` | ^16.3.0 | ^16.3.2 | Trivial patch |

**Confidence:** HIGH -- versions read directly from both package.json files.

### New Dependencies to Add

#### Required: Audio System

Alchemy's audio system uses the **Web Audio API directly** -- no third-party audio library needed. The entire system is pure TypeScript:

| Component | Implementation | New Dependency? |
|-----------|---------------|-----------------|
| AudioContext singleton | `audioContext.ts` -- lazy singleton with SFX/music gain buses | No -- browser API |
| Sound synthesis | `sounds.ts` -- procedural SFX via OscillatorNode, BiquadFilter, noise buffers | No -- browser API |
| Sample playback | `sounds.ts` -- decodeAudioData + BufferSource for .m4a samples | No -- browser API |
| Ambient music | `ambientMusic.ts` -- OscillatorNode layering with gain envelopes | No -- browser API |
| Audio store | `audioStore.ts` -- Zustand store for volume/mute preferences | No -- uses existing Zustand |

**Key finding:** Alchemy has `howler` (^2.2.4) in package.json but NEVER imports it anywhere in src/. It is a dead dependency. DO NOT add Howler to Forge.rs. The Web Audio API approach is superior because:
- Zero bundle size for audio (browser-native API)
- Fine-grained control over synthesis parameters
- Sample playback with pre-warming and lazy loading
- Dual gain buses (SFX + music) with independent volume control
- iOS/iPadOS warm-up pattern already implemented

**Audio assets needed:** .m4a sample files in `public/audio/sfx/` organized by type (damage, death, heal, summon, spell, keyword, ui). These are static assets, not npm dependencies.

#### Required: Canvas Particle VFX

Alchemy's particle system is a custom `ParticleSystem` class using the **Canvas 2D API directly**. No dependency needed.

Forge.rs already has a basic `ParticleCanvas.tsx` but Alchemy's implementation is significantly more capable:

| Feature | Forge.rs Current | Alchemy |
|---------|-----------------|---------|
| Particle properties | x, y, vx, vy, alpha, color, decay | + size, gravity, drag, glow, style (circle/ring), start/end size |
| Rendering | Single pass, basic circles | 3-pass batched: plain circles, glowing (pre-rendered sprites), rings |
| Blending | Default compositing | Additive blending (`lighter`) for glow effects |
| Effects system | None | `ActiveEffect` with update/draw/onComplete callbacks |
| DPR handling | Uses raw window dimensions | Capped at 2x DPR with proper canvas scaling |
| Texture sprites | None | LRU-cached HTMLImageElement sprites with async loading |
| Performance | Always runs rAF loop | Auto-starts/stops loop when particles exist |

**Action:** Replace Forge.rs's `ParticleCanvas.tsx` with Alchemy's `ParticleSystem` class. This is a pure code port, no new dependencies.

#### Required: PWA Service Worker Registration

Forge.rs uses `vite-plugin-pwa` (already installed) but has no service worker registration code in the client. Alchemy has a complete SW registration system:

| File | Purpose | New Dependency? |
|------|---------|-----------------|
| `registerServiceWorker.ts` | Registers SW via `virtual:pwa-register`, handles updates | No -- uses existing `vite-plugin-pwa` |
| `updateStatus.ts` | Reactive update status for UI (downloading/activating/idle) | No |

**Note:** Alchemy uses `workbox-window` (^7.4.0) in package.json, but the actual registration code uses `virtual:pwa-register` from `vite-plugin-pwa` which wraps Workbox internally. The `workbox-window` direct dependency is NOT imported in src/. DO NOT add `workbox-window` as a direct dependency.

**PWA config change:** Forge.rs currently uses `registerType: "autoUpdate"` and `manifest: false`. Port Alchemy's `registerType: "prompt"` pattern with inline manifest config for better update control. Keep Forge.rs's Scryfall runtime caching rules.

#### Required: CSS Custom Properties for Card Sizing

Alchemy uses CSS custom properties for responsive card dimensions. This is pure CSS, no dependencies:

```css
:root {
  --_card-w: 90px;     /* mobile */
  --_card-h: 130px;
  --_board-w: 82px;    /* board creatures */
  --_board-h: 115px;
}
/* Scales up via media queries at 768px and 1024px breakpoints */
/* Uses dvh units with clamp() for viewport-adaptive sizing */
```

**Action:** Port these CSS custom properties into Forge.rs's `index.css`. Components reference `var(--card-width)` etc. No JS dependency.

### Dependencies to NOT Add

| Alchemy Dependency | Why NOT to Add |
|--------------------|----------------|
| `howler` ^2.2.4 | Dead dependency in Alchemy -- never imported. Web Audio API used directly instead |
| `workbox-window` ^7.4.0 | Not directly imported. `vite-plugin-pwa` handles Workbox internally via `virtual:pwa-register` |
| `peerjs` ^1.5.5 | P2P networking for Alchemy's multiplayer. Forge.rs uses WebSocket server (`forge-server`) instead -- architecturally different |
| `uuid` ^13.0.0 | Only used in Alchemy's PeerJS networking layer. Forge.rs generates IDs in the Rust engine |
| `cypress` ^15.11.0 | E2E testing framework. Forge.rs uses Vitest only. Can add later if needed |
| `puppeteer` ^24.37.5 | Used for Alchemy's visual regression testing. Not needed for port |
| `fast-check` ^4.5.3 | Property-based testing. Nice to have but not required for UI port |
| `husky` ^9.1.7 | Git hooks. Forge.rs has its own CI setup |

### Dependencies to Keep (Forge.rs Only)

| Forge.rs Dependency | Purpose | Keep? |
|---------------------|---------|-------|
| `vite-plugin-wasm` ^3.4.1 | WASM module loading | YES -- required for Rust engine |
| `vite-plugin-top-level-await` ^1.5.0 | Top-level await for WASM init | YES -- required for WASM |
| `idb-keyval` ^6.2.2 | IndexedDB wrapper for Scryfall image caching | YES -- efficient, tiny (600B), works well |
| `@vitest/coverage-v8` ^3.2.4 | Code coverage | YES -- update to match Vitest 4 |

## Recommended Stack

### Core Framework (No Changes)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| React | ^19.2.0 | UI framework | Both projects aligned |
| Zustand | ^5.0.11 | State management | Both use subscribeWithSelector middleware |
| Framer Motion | ^12.35.1 | DOM animations | Both aligned, Forge.rs already newer |
| Tailwind CSS v4 | ^4.2.1 | Styling | Both aligned |
| React Router | ^7.13.1 | Routing | Both aligned |

### Build Tooling (Version Bumps)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| Vite | ^7.3.1 | Build tool | Align with Alchemy, required for plugin-react v5 |
| `@vitejs/plugin-react` | ^5.1.1 | React Fast Refresh | Required by Vite 7 |
| TypeScript | ~5.9.3 | Type checking | Align with Alchemy |
| `vite-plugin-pwa` | ^1.2.0 | PWA/Workbox integration | Already installed, needs config update |

### Audio (Browser APIs, Zero Dependencies)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| Web Audio API | Browser native | Sound synthesis + sample playback | Zero bundle cost, full control, iOS warm-up pattern included |

### VFX (Browser APIs, Zero Dependencies)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| Canvas 2D API | Browser native | Particle effects, projectiles, shockwaves | Additive blending, glow sprites, auto-start/stop loop |
| CSS Custom Properties | Browser native | Responsive card sizing | Media query breakpoints with dvh/clamp for viewport adaptation |

### Forge.rs-Specific (Keep)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| `vite-plugin-wasm` | ^3.4.1 | WASM loading | Required for Rust engine bridge |
| `vite-plugin-top-level-await` | ^1.5.0 | WASM init | Required for async WASM bootstrap |
| `idb-keyval` | ^6.2.2 | IndexedDB caching | Scryfall image cache, tiny footprint |

### Testing (Version Bumps)

| Technology | Version | Purpose | Why |
|------------|---------|---------|-----|
| Vitest | ^4.0.18 | Test runner | Align with Alchemy |
| `@vitest/coverage-v8` | ^3.2.4 -> match v4 | Coverage | Must match Vitest major |
| jsdom | ^28.1.0 | Test DOM | Align with Alchemy |
| `@testing-library/react` | ^16.3.2 | Component testing | Minor patch bump |

## Vite Config Changes

Forge.rs's `vite.config.ts` needs these changes for the port:

1. **Add path aliases** matching Alchemy's convention (`@components`, `@hooks`, `@audio`, etc.) -- or adapt to Forge.rs's existing structure
2. **Update PWA config** from `registerType: "autoUpdate"` to `registerType: "prompt"` with inline manifest
3. **Keep WASM plugins** (`vite-plugin-wasm`, `vite-plugin-top-level-await`) -- Alchemy doesn't need these but Forge.rs does
4. **Keep Scryfall caching rules** in workbox config -- Alchemy doesn't have these
5. **Add `includeAssets`** for audio files: `'**/*.{m4a,mp3,json}'`
6. **Add build hash/version defines** (`__BUILD_HASH__`, `__APP_VERSION__`) for PWA update UI

## Installation

```bash
cd client

# Update existing dependencies
pnpm update vite@^7.3.1 @vitejs/plugin-react@^5.1.1 typescript@~5.9.3 \
  vitest@^4.0.18 jsdom@^28.1.0 @testing-library/react@^16.3.2

# Update coverage plugin to match Vitest 4
pnpm update @vitest/coverage-v8

# No new runtime dependencies needed!
# Audio = Web Audio API (browser native)
# Particles = Canvas 2D API (browser native)
# Card sizing = CSS custom properties (browser native)
# PWA registration = vite-plugin-pwa virtual module (already installed)
```

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| Audio | Web Audio API (native) | Howler.js | Alchemy already proved native API works; Howler adds 10KB for features not needed (spatial audio, audio sprites) |
| Audio | Web Audio API (native) | Tone.js | Overkill for game SFX; designed for music production, 150KB+ |
| Particles | Canvas 2D (native) | PixiJS | 300KB+ bundle for 2D rendering engine; overkill for particle effects only |
| Particles | Canvas 2D (native) | Three.js | 600KB+; 3D renderer unnecessary for 2D particle effects |
| Particles | Canvas 2D (native) | tsparticles | 50KB+; less control than custom system, Alchemy's implementation already tuned for card game VFX |
| Card sizing | CSS custom properties | JS resize observer | CSS-only solution has zero JS overhead, works with Tailwind, no layout thrashing |
| PWA updates | vite-plugin-pwa | Custom SW | Plugin already installed, battle-tested Workbox integration |

## Key Insight

The port requires **zero new npm dependencies**. All new capabilities (audio, particle VFX, responsive card sizing, PWA registration) use browser-native APIs. The only changes are version bumps on existing dependencies to align the two projects. This is by design -- Alchemy was built to minimize external dependencies, and that philosophy carries over cleanly.

## Sources

- Alchemy `package.json` -- direct file read (HIGH confidence)
- Forge.rs `client/package.json` -- direct file read (HIGH confidence)
- Alchemy `src/audio/audioContext.ts`, `src/audio/sounds.ts` -- direct file read confirming Web Audio API usage (HIGH confidence)
- Alchemy `src/components/animation/particleSystem.ts` -- direct file read confirming Canvas 2D usage (HIGH confidence)
- Alchemy `src/pwa/registerServiceWorker.ts` -- direct file read confirming `virtual:pwa-register` usage (HIGH confidence)
- Alchemy `src/index.css` -- direct file read confirming CSS custom property card sizing (HIGH confidence)
- Forge.rs `client/src/stores/gameStore.ts` -- confirmed `subscribeWithSelector` already in use (HIGH confidence)
- Forge.rs `client/src/components/animation/ParticleCanvas.tsx` -- confirmed basic particle system exists (HIGH confidence)
- Alchemy `src/` grep for `howler` -- zero imports, confirmed dead dependency (HIGH confidence)
- Alchemy `src/` grep for `workbox-window` -- zero direct imports (HIGH confidence)
