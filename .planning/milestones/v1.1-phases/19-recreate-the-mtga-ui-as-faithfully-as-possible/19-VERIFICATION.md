---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
verified: 2026-03-09T23:30:00Z
status: passed
score: 8/8 must-haves verified
must_haves:
  truths:
    - "Battlefield permanents display Scryfall art_crop images with WUBRG color-coded borders, card name labels, P/T overlays for creatures, and loyalty shields for planeswalkers"
    - "Board layout matches MTGA: creatures near center, lands far, centered player/opponent avatars, phase indicators flanking player avatar, no visible borders or bars"
    - "Tapped permanents rotate 15-20 degrees (not 90) with slight opacity dim"
    - "Golden curved targeting arcs connect spells to targets, orange glow on selected attackers/blockers, attackers slide forward when declared"
    - "Cinematic layered turn banner (amber/slate), canvas death shatter fragments, card flight arcs for cast/resolve"
    - "Full-screen MTGA-style mulligan with large card images, dramatic VICTORY/DEFEAT game over screens"
    - "Mode-first menu flow with splash screen, deck gallery with art tiles, animated particle background"
    - "Deck builder displays art-crop grid with instant card preview and visual color/type filtering"
  artifacts:
    - path: "client/src/components/card/ArtCropCard.tsx"
      provides: "Art crop battlefield card renderer with WUBRG borders, name, P/T, loyalty, counters"
    - path: "client/src/components/board/PermanentCard.tsx"
      provides: "Conditional ArtCropCard/CardImage rendering, preference-driven tap rotation, orange combat glow, attacker slide"
    - path: "client/src/components/targeting/TargetArrow.tsx"
      provides: "Golden curved SVG bezier targeting arc with glow filter"
    - path: "client/src/components/animation/TurnBanner.tsx"
      provides: "Cinematic layered turn banner with amber/slate theming and 3 VFX quality tiers"
    - path: "client/src/components/animation/DeathShatter.tsx"
      provides: "Canvas-based card fragment shatter effect"
    - path: "client/src/components/animation/CastArcAnimation.tsx"
      provides: "Card flight arc animation for cast and resolve modes"
    - path: "client/src/components/animation/AnimationOverlay.tsx"
      provides: "Wires DeathShatter and CastArcAnimation into animation pipeline"
    - path: "client/src/components/board/GameBoard.tsx"
      provides: "MTGA-faithful zone ordering (creatures near center, lands far)"
    - path: "client/src/pages/GamePage.tsx"
      provides: "MTGA layout, full-screen mulligan, dramatic game over, zone indicators"
    - path: "client/src/components/controls/PhaseStopBar.tsx"
      provides: "Split phase indicators: PhaseIndicatorLeft, PhaseIndicatorRight, CombatPhaseIndicator"
    - path: "client/src/components/hud/PlayerHud.tsx"
      provides: "Centered avatar pill with flanking phase indicators"
    - path: "client/src/components/hud/OpponentHud.tsx"
      provides: "Centered opponent avatar pill"
    - path: "client/src/components/card/CardPreview.tsx"
      provides: "Instant card preview (no animation)"
    - path: "client/src/components/board/BattlefieldBackground.tsx"
      provides: "Full-bleed background without darkening overlay"
    - path: "client/src/components/splash/SplashScreen.tsx"
      provides: "Logo splash screen with loading progress bar"
    - path: "client/src/components/menu/DeckGallery.tsx"
      provides: "Deck selection gallery with Scryfall art tiles"
    - path: "client/src/components/menu/MenuParticles.tsx"
      provides: "Canvas particle background for menu"
    - path: "client/src/pages/MenuPage.tsx"
      provides: "Mode-first menu flow with deck gallery"
    - path: "client/src/App.tsx"
      provides: "SplashScreen integration on app launch"
    - path: "client/src/components/deck-builder/CardGrid.tsx"
      provides: "Art-crop image grid for search results with hover callback and count overlay"
    - path: "client/src/pages/DeckBuilderPage.tsx"
      provides: "Deck builder page with CardPreview overlay"
    - path: "client/public/logo.webp"
      provides: "Forge.rs logo in WebP format"
  key_links:
    - from: "PermanentCard.tsx"
      to: "ArtCropCard.tsx"
      via: "conditional render based on battlefieldCardDisplay preference"
    - from: "ArtCropCard.tsx"
      to: "useCardImage.ts"
      via: "useCardImage with size art_crop"
    - from: "AnimationOverlay.tsx"
      to: "DeathShatter.tsx"
      via: "renders DeathShatter for CreatureDestroyed events"
    - from: "AnimationOverlay.tsx"
      to: "CastArcAnimation.tsx"
      via: "renders CastArcAnimation for SpellCast and ZoneChanged events"
    - from: "App.tsx"
      to: "SplashScreen.tsx"
      via: "shows splash during simulated loading"
    - from: "MenuPage.tsx"
      to: "DeckGallery.tsx"
      via: "renders DeckGallery after mode selection"
    - from: "DeckBuilderPage.tsx"
      to: "CardPreview.tsx"
      via: "renders CardPreview on card hover"
    - from: "PlayerHud.tsx"
      to: "PhaseStopBar.tsx"
      via: "imports PhaseIndicatorLeft and PhaseIndicatorRight"
---

# Phase 19: Recreate the MTGA UI as faithfully as possible — Verification Report

**Phase Goal:** Close the visual and interaction gap between the current Arena-inspired UI and the actual MTGA experience -- art-crop card presentation on battlefield, MTGA-faithful board layout with centered avatars and flanking phase indicators, cinematic animations (turn banner, death shatter, cast arcs), mode-first menu with deck gallery, and deck builder visual polish
**Verified:** 2026-03-09T23:30:00Z
**Status:** passed
**Re-verification:** No -- initial verification

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Battlefield permanents display Scryfall art_crop images with WUBRG color-coded borders, card name labels, P/T overlays for creatures, and loyalty shields for planeswalkers | VERIFIED | ArtCropCard.tsx (128 lines): BORDER_COLORS map with W/U/B/R/G hex values, getBorderColor() handles 0/1/2+ colors, uses `useCardImage(cardName, { size: "art_crop" })`, renders PTBox overlay, loyalty shield, counter badges, name label. Wired into PermanentCard when `battlefieldCardDisplay === "art_crop"` |
| 2 | Board layout matches MTGA: creatures near center, lands far, centered player/opponent avatars, phase indicators flanking player avatar, no visible borders or bars | VERIFIED | GameBoard.tsx renders opponent: other/lands/creatures (top-to-bottom), player: creatures/lands/other. GamePage layout: OpponentHand -> OpponentHud -> GameBoard -> PlayerHud -> PlayerHand. PlayerHud has PhaseIndicatorLeft/Right flanking avatar pill. No border-b/border-t/bg-black/20 classes in GameBoard or BattlefieldRow |
| 3 | Tapped permanents rotate 15-20 degrees (not 90) with slight opacity dim | VERIFIED | PermanentCard.tsx: `tapAngle = tapRotation === "mtga" ? 17 : 90` and `tapOpacity = tapRotation === "mtga" && obj.tapped && !isAttacking ? 0.85 : 1`. preferencesStore default: `tapRotation: "mtga"` |
| 4 | Golden curved targeting arcs connect spells to targets, orange glow on selected attackers/blockers, attackers slide forward when declared | VERIFIED | TargetArrow.tsx: quadratic Bezier curve with `#C9B037` gold color, SVG glow filter with feGaussianBlur/feMerge, animated pathLength. PermanentCard.tsx: both isAttacking and isBlocking use `ring-2 ring-orange-500`, isValidTarget uses `ring-2 ring-amber-400/60`. attackSlide: `isAttacking ? (obj.controller === 0 ? -30 : 30) : 0` |
| 5 | Cinematic layered turn banner (amber/slate), canvas death shatter fragments, card flight arcs for cast/resolve | VERIFIED | TurnBanner.tsx (269 lines): 3 quality tiers, full quality has light burst, banner strip, diamond accents, triple-glow text punch with hold pulse. AMBER/SLATE theme objects. DeathShatter.tsx (192 lines): canvas-based 3x4 grid = 12 fragments with outward velocity, gravity, rotation, red flash, alpha fade over 0.6s. CastArcAnimation.tsx (158 lines): three modes (cast, resolve-permanent, resolve-spell) with parabolic arc. AnimationOverlay wires all three |
| 6 | Full-screen MTGA-style mulligan with large card images, dramatic VICTORY/DEFEAT game over screens | VERIFIED | GamePage.tsx: MulliganDecisionPrompt uses `fixed inset-0 z-50` full-screen with radial gradient, 160x224px card images with fan rotation, staggered entrance animation, prominent Keep Hand (emerald) / Mulligan (outline) buttons. GameOverScreen: full-screen with VICTORY/DEFEAT/DRAW text in 6xl, golden/red/silver glow, spring animation, VictoryParticles for wins, life totals, Return to Menu + Rematch buttons |
| 7 | Mode-first menu flow with splash screen, deck gallery with art tiles, animated particle background | VERIFIED | App.tsx: SplashScreen with simulated 1.5s loading progress. MenuPage: mode-select as default view with Play vs AI / Play Online / Deck Builder buttons, MenuParticles canvas background with 50 particles (indigo/cyan/amber). DeckGallery: art_crop tiles via DeckArtTile component, color dots, card count, difficulty segmented control for AI mode |
| 8 | Deck builder displays art-crop grid with instant card preview and visual color/type filtering | VERIFIED | CardGrid.tsx: uses `card.image_uris?.art_crop` directly, 4:3 aspect ratio, onCardHover callback, cardCounts badge overlay, always-visible card name. DeckBuilderPage: manages hoveredCardName state, renders CardPreview overlay. CardPreview: instant render (no framer-motion imports). DeckBuilder.tsx: passes onCardHover to CardGrid and DeckList. Note: color/type filtering reuses existing CardSearch filters (no duplicate UI) |

**Score:** 8/8 truths verified

### Required Artifacts

| Artifact | Status | Details |
|----------|--------|---------|
| `client/src/components/card/ArtCropCard.tsx` | VERIFIED | 128 lines, exports ArtCropCard, uses useCardImage with art_crop, WUBRG borders, PTBox, loyalty, counters |
| `client/src/services/scryfall.ts` | VERIFIED | `ImageSize` type includes "art_crop", exported |
| `client/src/hooks/useCardImage.ts` | VERIFIED | `UseCardImageOptions.size` includes "art_crop" |
| `client/src/index.css` | VERIFIED | `--art-crop-base`, `--art-crop-w`, `--art-crop-h` defined with responsive breakpoints |
| `client/src/stores/preferencesStore.ts` | VERIFIED | `BattlefieldCardDisplay`, `TapRotation` types, defaults "art_crop"/"mtga" |
| `client/public/logo.webp` | VERIFIED | File exists |
| `client/src/components/board/PermanentCard.tsx` | VERIFIED | 211 lines, imports ArtCropCard, conditional rendering, 17deg tap, orange glow, attack slide |
| `client/src/components/card/CardPreview.tsx` | VERIFIED | No framer-motion, instant render, size "normal" |
| `client/src/components/board/BattlefieldRow.tsx` | VERIFIED | Dynamic min-h based on battlefieldCardDisplay, no border classes |
| `client/src/components/board/BattlefieldBackground.tsx` | VERIFIED | No bg-black/40 overlay |
| `client/src/components/board/GameBoard.tsx` | VERIFIED | Zone order: creatures near center, lands far, no border classes |
| `client/src/pages/GamePage.tsx` | VERIFIED | MTGA layout, full-screen mulligan, dramatic game over, zone indicators at bottom-left |
| `client/src/components/controls/PhaseStopBar.tsx` | VERIFIED | Exports PhaseIndicatorLeft, PhaseIndicatorRight, CombatPhaseIndicator |
| `client/src/components/hud/PlayerHud.tsx` | VERIFIED | Centered avatar pill with PhaseIndicatorLeft/Right flanking |
| `client/src/components/hud/OpponentHud.tsx` | VERIFIED | Centered avatar pill, no floating option |
| `client/src/components/targeting/TargetArrow.tsx` | VERIFIED | Golden bezier curve with #C9B037, SVG glow filter, animated pathLength |
| `client/src/components/animation/TurnBanner.tsx` | VERIFIED | 269 lines, layered cinematic with 3 VFX tiers, amber/slate theming |
| `client/src/components/animation/DeathShatter.tsx` | VERIFIED | 192 lines, canvas-based, 3x4 grid = 12 fragments, gravity, rotation, red flash |
| `client/src/components/animation/CastArcAnimation.tsx` | VERIFIED | 158 lines, cast/resolve-permanent/resolve-spell modes with parabolic arc |
| `client/src/components/animation/AnimationOverlay.tsx` | VERIFIED | Imports and renders DeathShatter (CreatureDestroyed) and CastArcAnimation (SpellCast, ZoneChanged) |
| `client/src/components/splash/SplashScreen.tsx` | VERIFIED | 56 lines, logo + progress bar + fade-out |
| `client/src/components/menu/DeckGallery.tsx` | VERIFIED | 198 lines, art_crop tiles, color dots, card count, difficulty selector, last-used deck restore |
| `client/src/components/menu/MenuParticles.tsx` | VERIFIED | 106 lines, canvas-based, 50 particles, indigo/cyan/amber, requestAnimationFrame |
| `client/src/pages/MenuPage.tsx` | VERIFIED | Mode-first flow (mode-select, deck-gallery-ai, deck-gallery-online, online-host-join, join-code) |
| `client/src/App.tsx` | VERIFIED | SplashScreen with simulated 1.5s progress |
| `client/src/components/deck-builder/CardGrid.tsx` | VERIFIED | Art_crop URLs from ScryfallCard, 4:3 ratio, onCardHover, cardCounts badge |
| `client/src/pages/DeckBuilderPage.tsx` | VERIFIED | Manages hoveredCardName, renders CardPreview overlay |

### Key Link Verification

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| PermanentCard.tsx | ArtCropCard.tsx | Conditional render when `battlefieldCardDisplay === "art_crop"` | WIRED | Line 163: `<ArtCropCard objectId={objectId} />` |
| ArtCropCard.tsx | useCardImage.ts | `useCardImage(cardName, { size: "art_crop" })` | WIRED | Line 34 |
| useCardImage.ts | scryfall.ts | `fetchCardImageUrl(cardName, faceIndex, size)` | WIRED | Line 47 |
| AnimationOverlay.tsx | DeathShatter.tsx | Renders for CreatureDestroyed events | WIRED | Lines 190-217 (fetch art_crop, create shatter), lines 444-451 (render) |
| AnimationOverlay.tsx | CastArcAnimation.tsx | Renders for SpellCast and ZoneChanged events | WIRED | Lines 234-243 (SpellCast), 292-299 (resolve-permanent), 303-316 (resolve-spell), lines 453-463 (render) |
| App.tsx | SplashScreen.tsx | Shows splash during loading | WIRED | Line 43: `<SplashScreen progress={progress} onComplete={handleSplashComplete} />` |
| MenuPage.tsx | DeckGallery.tsx | Renders after mode selection | WIRED | Lines 168, 182: `<DeckGallery>` in deck-gallery-ai and deck-gallery-online views |
| DeckBuilderPage.tsx | CardPreview.tsx | Renders on card hover | WIRED | Line 11: `<CardPreview cardName={hoveredCardName} />` |
| PlayerHud.tsx | PhaseStopBar.tsx | Flanking phase indicators | WIRED | Lines 16-17: `<PhaseIndicatorLeft />` and `<PhaseIndicatorRight />` |
| GamePage.tsx | CombatPhaseIndicator | Fixed position near action buttons | WIRED | Line 286: `<CombatPhaseIndicator />` |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|------------|-------------|--------|----------|
| ARENA-01 | 19-01 | Battlefield permanents display Scryfall art_crop images with WUBRG color-coded borders, card name labels, P/T overlays, loyalty shields, and circular counter badges | SATISFIED | ArtCropCard.tsx implements all: BORDER_COLORS map, name label, PTBox, loyalty shield, counter badges |
| ARENA-02 | 19-01 | Dual Scryfall image fetching (art_crop for battlefield/deck builder, normal for hand/stack/preview) with IndexedDB caching | SATISFIED | ImageSize type extended with "art_crop" in scryfall.ts, imageCache.ts, useCardImage.ts |
| ARENA-03 | 19-02 | Tapped permanents rotate 15-20 degrees clockwise (not 90) with 0.85 opacity dim | SATISFIED | PermanentCard.tsx: tapAngle=17 in MTGA mode, tapOpacity=0.85 |
| ARENA-04 | 19-03 | MTGA-faithful board layout with creatures near center, lands far, centered avatars, and phase indicators flanking player avatar | SATISFIED | GameBoard zone order reversed, GamePage layout restructured, PlayerHud with flanking phase indicators |
| ARENA-05 | 19-02, 19-03 | No visible zone borders, bars, or background overlays | SATISFIED | No border/bar classes in GameBoard, BattlefieldRow; no bg-black/40 in BattlefieldBackground |
| ARENA-06 | 19-04 | Golden curved SVG targeting arcs, orange glow on selected attackers/blockers, attacker slide-forward on declare | SATISFIED | TargetArrow: golden bezier with glow; PermanentCard: orange for attack+block, amber for valid targets, y-slide for attackers |
| ARENA-07 | 19-05 | Cinematic layered turn banner with amber/slate theming | SATISFIED | TurnBanner.tsx: 6-phase animation (light burst, banner, diamonds, text, hold, exit) with amber/slate themes and 3 VFX tiers |
| ARENA-08 | 19-05 | Canvas death shatter and card flight arcs for cast/resolve | SATISFIED | DeathShatter.tsx: canvas 12-fragment shatter; CastArcAnimation.tsx: 3 flight modes; AnimationOverlay wires both |
| ARENA-09 | 19-06 | Full-screen MTGA-style mulligan with large card images and dramatic VICTORY/DEFEAT game over screens | SATISFIED | MulliganDecisionPrompt: full-screen, 160x224 cards, fan layout. GameOverScreen: VICTORY/DEFEAT/DRAW with glow, particles, life totals |
| ARENA-10 | 19-07 | Mode-first menu flow, deck gallery with art tiles, particle background, splash screen | SATISFIED | MenuPage: mode-select -> deck-gallery flow. DeckGallery with art_crop tiles. MenuParticles canvas. SplashScreen in App.tsx |
| ARENA-11 | 19-08 | Deck builder art-crop image grid with instant card preview and visual color/type filtering | SATISFIED | CardGrid uses art_crop URLs at 4:3 ratio. DeckBuilderPage renders CardPreview on hover. Filtering via existing CardSearch |
| ARENA-12 | 19-01, 19-07 | Forge.rs logo converted to WebP for splash screen and menu branding | SATISFIED | client/public/logo.webp exists. Used in SplashScreen and MenuPage mode-select |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| (none) | - | - | - | No anti-patterns detected in any of the 20+ key files |

Zero TODO/FIXME/placeholder/stub patterns found across all modified files.

### Human Verification Required

### 1. Art Crop Visual Quality

**Test:** Open a game and verify battlefield permanents display with art_crop images and correct WUBRG-colored borders
**Expected:** Creatures show P/T overlay, planeswalkers show loyalty shield, multicolor cards have gold border, colorless cards have gray border, tokens have thinner borders
**Why human:** Visual appearance and color accuracy cannot be verified programmatically

### 2. Tap Rotation Feel

**Test:** Tap a permanent and observe the rotation angle
**Expected:** Card rotates approximately 17 degrees clockwise with slight opacity dim (0.85), not the classic 90-degree rotation
**Why human:** Angular perception and opacity feel need visual confirmation

### 3. Golden Targeting Arc

**Test:** Cast a spell that targets a permanent and observe the targeting line
**Expected:** Golden curved bezier arc from source to target with glow effect, not a straight cyan line
**Why human:** Visual appearance of SVG curves and glow filters need human eyes

### 4. Combat Glow and Slide

**Test:** Declare attackers and observe selected creatures
**Expected:** Orange glow ring on selected attackers, creatures slide 30px toward center when declared as attackers
**Why human:** Animation timing, glow color, and slide distance are visual properties

### 5. Turn Banner Cinematic

**Test:** Start a game and observe the turn banner on each turn change
**Expected:** Layered animation: light burst expands, banner strip slides in, diamond accents appear, text punches in with triple glow. Amber for your turn, slate for opponent's turn
**Why human:** Multi-phase animation timing and layered effects need visual confirmation

### 6. Death Shatter Effect

**Test:** Destroy a creature and observe the death animation
**Expected:** Card image shatters into 8-12 fragments that scatter outward with gravity, rotation, and alpha fade over 0.6s, with brief red tint flash
**Why human:** Canvas-based particle effects need visual confirmation

### 7. Mulligan Screen

**Test:** Start a game and observe the mulligan decision screen
**Expected:** Full-screen dark gradient backdrop, large 160x224 card images in fan layout with staggered entrance, prominent Keep Hand / Mulligan buttons
**Why human:** Full-screen layout composition and card fan visual need human eyes

### 8. Game Over Screen

**Test:** Complete a game (win, lose, or draw)
**Expected:** Full-screen VICTORY (gold glow + particles) or DEFEAT (red glow) or DRAW (silver glow) with animated text, life totals, and Rematch + Return to Menu buttons
**Why human:** Dramatic visual presentation and particle effects need human confirmation

### 9. Menu Flow

**Test:** Open the app and observe the splash screen, then navigate through the menu
**Expected:** Brief splash with logo and loading bar. Mode selection: Play vs AI, Play Online, Deck Builder. After selecting mode, deck gallery with art tiles and difficulty selector
**Why human:** Flow transition and art tile loading experience need human testing

### 10. Deck Builder Preview

**Test:** Open deck builder, hover over cards in search results and deck list
**Expected:** Instant full-card preview appears on right side (no animation delay), art_crop images at 4:3 ratio in search grid
**Why human:** Hover responsiveness and preview positioning need visual confirmation

### Gaps Summary

No gaps found. All 8 success criteria are verified with evidence from the codebase. All 12 ARENA requirements (ARENA-01 through ARENA-12) are accounted for across the 8 plans and satisfied with implemented code. All 16 commits verified in git history. TypeScript type-check passes cleanly. No anti-patterns, stubs, or incomplete implementations detected.

The phase successfully delivers:
- Art-crop card rendering with WUBRG color system (ArtCropCard component)
- MTGA-faithful board layout with reversed zone ordering and centered HUDs
- Preference-driven tap rotation (17deg MTGA / 90deg classic)
- Golden bezier targeting arcs with SVG glow filter
- Orange combat selection glow with attacker slide-forward
- Cinematic turn banner with 3 VFX quality tiers
- Canvas-based death shatter with fragment physics
- Cast/resolve card flight arc animations
- Full-screen mulligan and dramatic game over screens
- Mode-first menu with splash screen, deck gallery, and particle background
- Deck builder art-crop grid with instant card preview

All items marked for human verification are visual/interactive behaviors that cannot be confirmed through static code analysis alone.

---

_Verified: 2026-03-09T23:30:00Z_
_Verifier: Claude (gsd-verifier)_
