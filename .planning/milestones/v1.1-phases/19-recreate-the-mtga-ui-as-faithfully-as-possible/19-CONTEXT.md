# Phase 19: Recreate the MTGA UI as faithfully as possible - Context

**Gathered:** 2026-03-09
**Status:** Ready for planning

<domain>
## Phase Boundary

Close the visual and interaction gap between the current Arena-inspired UI and the actual MTGA experience. This phase overhauls card presentation (art crops on battlefield, full cards in hand), board layout (MTGA-faithful zone arrangement, avatar placement, phase indicators), micro-interactions (combat animations, targeting arcs, death effects, turn banners), menu/lobby (mode-first flow, deck gallery, splash screen), and deck builder (art-crop grid). Engine and mechanics are unchanged — this is a visual/interaction fidelity phase.

</domain>

<decisions>
## Implementation Decisions

### Card presentation — battlefield
- Art crop images on battlefield (Scryfall `art_crop` variant, ~40KB each)
- No mana cost pips on battlefield cards — MTGA doesn't show them
- Color-coded border by card color identity: White=cream, Blue=blue, Black=dark, Red=red, Green=green, Multicolor=gold, Colorless=gray
- Small card name label always visible below the art crop
- Same art-crop treatment for all permanent types (creatures, lands, enchantments, artifacts)
- Creatures: P/T box overlay (existing color-coding: green=buffed, red=damaged)
- Planeswalkers: loyalty shield badge instead of P/T box
- Enchantments/artifacts: art crop only, no stat overlay
- Tokens: same art crop style, thinner frame to distinguish from regular cards
- Counter badges: circular overlay at top-right with count number

### Card presentation — hand and stack
- Full Scryfall card images (`normal` variant) in player's hand — readable card text
- Full card images on the stack (right-center, staggered pile — no change from Phase 17)
- Opponent hand: classic MTG card back images fanned at top

### Image sourcing
- Dual Scryfall image fetching: `art_crop` for battlefield/zone viewers, `normal` for hand/stack/preview
- Both variants cached in IndexedDB via existing imageCache.ts
- Two requests per card on first encounter (art_crop + normal)

### Board layout — MTGA-faithful arrangement
- Player avatar: CENTER, directly above the player hand
- Opponent avatar: top-center, BELOW their hand
- Phase indicators (non-combat): flanking the player avatar — left side: Upkeep → Draw → Main 1, right side: Main 2 → End
- Combat sub-phases: near the action button group on the right side
- Stack display: right-center overlay (no change from Phase 17)
- Zone order (from center outward): creatures NEAR center, lands FAR from center — both sides
  - Opponent (top to bottom): hand → avatar → lands → creatures → [center]
  - Player (bottom to top): hand → avatar → lands → creatures → [center]
- Attackers slide forward toward center during declare attackers
- Bottom-left: player graveyard (face-up cards) with library directly to its right

### Board layout — atmosphere
- Battlefield background images from existing 9 webp files + Forge assets (NO gradients, NO vignette, NO post-processing)
- No visible zone lane borders — invisible spatial grouping only (matches MTGA)
- No bars anywhere — everything floats over the battlefield
- Minimal center divider line at most (if any visual separator at all)

### Tapped permanents
- 15-20° clockwise rotation (NOT 90° — MTGA never does 90° rotation)
- Slight opacity dim (~0.85) on tapped cards
- OVERRIDES Phase 13 decision of 90° rotation
- User-toggleable preference: "mtga" (17°) vs "classic" (90°) tap rotation

### Display preferences (toggleable)
- Battlefield card display: toggle between art-crop (MTGA-style) and full card images
- Tap rotation angle: toggle between 17° MTGA-style and 90° classic
- Both settings live in preferencesStore (persisted to localStorage)
- Default to MTGA-style for both

### Micro-interactions — combat
- Legal attackers/blockers: cyan glow (existing, don't change)
- Selected attackers/blockers: orange glow (new — replaces any existing selection indicator)
- Attackers slide forward toward center when declared
- Golden targeting arcs connecting spells/attackers to targets (curved lines)
- Valid targets get golden glow during target selection

### Micro-interactions — animations
- Card play: card flies from hand → stack with arc trajectory, glow on arrival
- Stack resolve: card flies from stack → battlefield (permanents) or fades (instants/sorceries)
- Creature death: card flashes red → shatters into 8-12 art fragments → fragments scatter + fade (0.6s) → particle burst at death position
- Damage impact: red particle burst on target + floating damage number + screen shake on lethal
- Turn banner: port Alchemy's TurnBanner (layered cinematic: light burst → banner strip → diamond accents → punchy text with triple glow, amber for "Your Turn", slate for "Their Turn", 1.5s duration)
- Coin flip / play/draw reveal animation at game start
- Card hover on battlefield: instant full-card preview panel on right side (NO slide animation — instant appear to avoid jarring when scanning multiple cards) + subtle glow on hovered card

### Micro-interactions — screens
- Mulligan: full-screen MTGA-style with large full-card images centered, "Keep" / "Mulligan" buttons
- Game over: dramatic full-screen "VICTORY" (gold + particles) / "DEFEAT" (red/dark) overlays with final life totals, menu + rematch buttons

### Menu & lobby flow
- Mode selection first (Play vs AI, Play Online, Deck Builder) — no deck selection on main menu
- After choosing mode → deck gallery screen with card art tiles
- Deck gallery: tiles showing representative card art (Scryfall art_crop), deck name, color dots, card count
- AI difficulty: inline segmented control in deck gallery (Easy/Medium/Hard), not a separate screen
- Last-used deck remembered and pre-selected
- Animated particle background on menu (port from Alchemy's menu particles)
- Brief splash screen with Forge.rs logo + loading bar during WASM init

### Logo
- Convert existing logo PNG (`~/Downloads/ChatGPT Image Mar 9, 2026, 03_18_06 PM.png`) to WebP at quality 85
- Use for splash screen, menu header, and app branding

### Deck builder overhaul
- In scope for this phase — visual polish pass
- Card display: art-crop image grid (same art_crop Scryfall images as battlefield)
- Card count overlays on each tile
- Visual color/type filtering
- Card hover: same instant preview panel as game board

### Claude's Discretion
- Exact art-crop card dimensions and aspect ratio on battlefield
- Color-coded border exact hex values and width
- Counter badge exact positioning and sizing
- Attacker slide distance and animation timing
- Targeting arc curve parameters and animation
- Death shatter fragment count, scatter pattern, and timing
- Particle background implementation details for menu
- Splash screen animation timing and loading bar style
- Deck gallery tile sizing and grid layout
- Deck builder filter UI design
- How to pick "representative card" art for deck gallery tiles
- Graveyard and library visual design at bottom-left
- Whether center divider line exists at all or is purely invisible

</decisions>

<specifics>
## Specific Ideas

- "MTGA doesn't do 90 deg rotation... ever. not even lands" — tapped is a subtle 15-20° tilt, not sideways
- "I don't think we need ANY lanes. MTGA has none" — zone lanes are invisible spatial grouping
- "there is no bottom bar. i don't know where you're getting this from. there is no bar anywhere, ever" — no chrome bars, everything floats
- "I've included MULTIPLE battlefield maps we can use. Forge also has battlefield backgrounds" — use real images, never gradients
- MTGA screenshot reference (`~/Pictures/Screenshot 2026-03-09 at 2.46.03 PM.png`): player avatar center-bottom, phase dots flanking, action group right, stack right-center
- Port Alchemy's TurnBanner — "Alchemy currently does one that looks great!"
- Port Alchemy's menu particle background
- Forge.rs logo created and ready for conversion (`~/Downloads/ChatGPT Image Mar 9, 2026, 03_18_06 PM.png`)
- "leaning towards instant [preview] because going through multiple cards would be jarring with the constant animations"
- Playable card cyan highlight: "this already exists as a cyan highlight. don't touch it"
- Hand fan behavior: "how we've implemented this already should be fine. don't change it"

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `BattlefieldBackground.tsx` + `battlefields.ts`: 9 battlefield webp images with WUBRG color mapping — foundation for board backgrounds
- `CardImage.tsx`: Scryfall image loader — needs dual-variant support (art_crop + normal)
- `imageCache.ts` + `scryfall.ts`: IndexedDB caching pipeline — extend for art_crop variant
- `PTBox.tsx`: Power/toughness overlay — reuse on art-crop cards
- `AnimationOverlay.tsx` + `ParticleCanvas.tsx`: Animation and particle systems — extend for death shatter, cast arcs
- `TurnBanner.tsx`: Basic turn banner — replace with Alchemy's layered version
- `ActionButton.tsx`: Unified combat/priority controls — add combat sub-phase indicators
- `CardPreview.tsx`: Side preview panel — wire to battlefield art-crop hover
- `boardSizing.ts`: Dynamic card sizing — adapt for art-crop dimensions
- `buttonStyles.ts`: Game button styling — reuse across new UI
- `BlockAssignmentLines.tsx`: SVG combat lines — extend for targeting arcs

### Alchemy Components to Port
- `TurnBanner` (alchemy/src/components/phase/TurnBanner.tsx): Layered cinematic turn transition with Framer Motion
- Menu particle background (alchemy menu implementation)

### Forge Assets
- Battlefield backgrounds in `forge-gui/res/skins/default/` (bg_match.jpg, adv_bg_*.jpg)
- Card back image for opponent hand

### Established Patterns
- Scryfall art_crop API: `https://api.scryfall.com/cards/named?exact={name}&format=image&version=art_crop`
- WaitingFor-driven component visibility (unchanged)
- legalActions-driven highlighting (cyan glow — unchanged)
- Framer Motion AnimatePresence for enter/exit
- Module-level constants for Zustand selector stability

### Integration Points
- `PermanentCard.tsx`: Currently renders full card — needs art-crop mode
- `BattlefieldRow.tsx`: Row layout — needs invisible lane treatment
- `GameBoard.tsx`: Zone order — creatures near center, lands far
- `PlayerHud.tsx` / `OpponentHud.tsx`: Repositioning to center above/below hand
- `PhaseStopBar.tsx` / `PhaseTracker.tsx`: Relocate to flank player avatar
- `MenuPage.tsx`: Complete overhaul — mode-first flow + deck gallery
- `DeckBuilderPage.tsx`: Art-crop grid overhaul
- `GamePage.tsx`: Layout restructure for MTGA-faithful positioning
- `MulliganDecisionPrompt` / `GameOverScreen`: In-file components in GamePage.tsx — upgrade to full-screen

</code_context>

<deferred>
## Deferred Ideas

- Combat math bubbles — deferred from Phase 17 due to MTG keyword complexity
- Manual mana tapping mode — future preferences toggle
- Stack item hover preview (show enlarged on hover) — nice-to-have
- Deck builder advanced features (import/export improvements, sideboard management) — separate phase
- Animated card art on battlefield (like MTGA's premium card animations) — future enhancement
- Deckbuilder MTGA-style collection view with 3D card flipping — too complex for this phase

</deferred>

---

*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Context gathered: 2026-03-09*
