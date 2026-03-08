# Feature Landscape: Arena UI Port (Alchemy to Forge.rs)

**Domain:** MTG game client UI — porting Alchemy's polished card game frontend to replace Forge.rs's functional but basic UI
**Researched:** 2026-03-08
**Confidence:** HIGH (direct source code comparison of both projects)

## Context

Alchemy is a card game (simplified MTG-like, educational focus) with a polished, MTGA-inspired UI built on Zustand + Framer Motion + Canvas. Forge.rs is a full MTG rules engine with a functional but minimal React frontend. This analysis identifies what Alchemy's UI adds that Forge.rs lacks, what Forge.rs needs that Alchemy does not have, and how to prioritize the port.

Key architectural alignment: both projects use Zustand stores (game, UI, animation), discriminated union action/event types, and Framer Motion. The EngineAdapter abstraction in Forge.rs is the integration boundary -- Alchemy's UI components will wire through it to the Rust/WASM engine.

---

## Table Stakes

Features users expect in a polished MTG client. Missing = product feels incomplete.

| Feature | Why Expected | Complexity | Alchemy Has | Forge.rs Has | Notes |
|---------|--------------|------------|-------------|--------------|-------|
| Canvas particle VFX system | Visual feedback for combat/spells is baseline for modern card games | Med | YES -- full ParticleSystem with 9 effect types: explosion, projectile, spellImpact, damageFlash, playerDamage, healEffect, keywordFlash, blockClash, summonBurst | Rudimentary -- emitBurst only (fixed color, fixed count) | Alchemy's system is element-aware with projectile trajectories. Must remap "element" concept to WUBRG colors. |
| Floating damage/heal numbers | Players need to see numeric game state changes | Low | YES -- FloatingNumber with red/green/amber + position targeting, per-step health deltas | YES -- basic FloatingNumber (value + color + position) | Forge.rs version works but Alchemy adds intermediate displayHealth tracking so damage appears per-step, not all at once |
| Screen shake on big hits | Visceral combat feel expected in MTGA-style clients | Low | YES -- 3 CSS intensity levels (light/medium/heavy) keyed to total player damage thresholds | NO | Simple CSS animation via useScreenShake hook. Threshold: 3+ damage = medium, 5+ = heavy. |
| Responsive card sizing | Cards must scale from mobile to desktop without breaking layout | Med | YES -- CSS custom properties (--card-width, --card-height, --card-font-scale), boardSizing.ts calculates per-slot sizes dynamically | NO -- fixed Scryfall image sizes | Alchemy uses --ui-scale and --board-scale CSS custom properties set from preferencesStore. User-adjustable. |
| MTGA-style hand peek/expand | Hand must be accessible without consuming screen space | Med | YES -- hand fixed at bottom, collapsed by default (translateY 60%), slides up on hover/touch, auto-collapses on phase change | NO -- hand always visible in static row | Alchemy's PlayerHand is a fixed overlay with pointer-events:none wrapper so clicks pass through to battlefield. Fan layout with rotation based on card count. |
| Card drag-to-play | Touch-friendly card playing beyond click | Med | YES -- pointer-based drag with 10px threshold, phantom card portal at cursor, drop-on-battlefield detection | NO -- click-only playing | Uses document-level pointermove/pointerup listeners. Phantom card rendered via createPortal. |
| Hero/Player HUD with health bar | Player identity and health visibility at a glance | Med | YES -- HeroHUD with HeroPortrait, HealthBadge, HealthBar, EnergyCrystals, PhaseDiamonds | Basic -- LifeTotal text, PhaseTracker text | Alchemy's HUD is visually rich. For MTG: replace EnergyCrystals with mana pool summary, keep HealthBar (becomes life total bar). |
| Turn/phase banner overlay | Players need to know when turns change | Low | YES -- TurnBanner with animated entrance/exit | Partial -- phase shown as inline text in middle spacer | Alchemy's banner is a full-width overlay with framer-motion animation. |
| Card preview on long-press | Players need to read card text at scale | Low | YES -- CardPreview modal, right-click or long-press trigger | YES -- inspectObject on hover | Both have this. Alchemy's is more polished with full card face rendering. |
| Block assignment visualization | Players must see which blocker maps to which attacker | Med | YES -- BlockAssignmentLines (SVG lines between cards) + animated BlockLink effect | Partial -- blockerAssignments Map stored in uiStore but no visual rendering | Forge.rs has the data; needs the visual component. |
| Animated card entry/exit | Cards appearing/disappearing should feel physical | Low | YES -- AnimatePresence on hand cards, layout animations, play burst effect (cyan flash when card leaves hand) | YES -- basic AnimatePresence with whileHover on hand cards | Forge.rs has basic animations; Alchemy adds burst effects, stagger delays, and smoother transitions. |
| Graveyard/discard viewer | Players need to see what's in graveyards | Low | YES -- modal with card list, counts per unique card, click-to-inspect | NO | Simple overlay: lists cards most-recent-first with dedup counts. Essential for MTG gameplay (many cards reference graveyard). |

---

## Differentiators

Features that set the product apart. Not expected but valued.

| Feature | Value Proposition | Complexity | Alchemy Has | Notes |
|---------|-------------------|------------|-------------|-------|
| Combat math bubbles | Shows attacker/blocker trade math BEFORE damage -- helps players learn | Med | YES -- keyword-aware (fury/armor/deathtouch), tappable expand popover | For MTG: adapt to show P/T trades with first strike, trample, deathtouch, lifelink. Positioned at midpoint of block assignment lines. |
| Math breakdown overlays | Shows "20 - 3 = 17" style equations per damage/heal step | Med | YES -- PersistedMathBreakdown with independent timing from animation steps | Persists long enough to read (2.2s). Tracks previousDisplayHealth for before/after values. |
| Audio: SFX system | Sound effects on game events make the experience immersive | High | YES -- sample-based with procedural synthesis fallback, catalog.json manifest, per-element spell variants, prewarm pipeline | ~30+ .m4a samples across 11 effect types. Alchemy has element-keyed spell sounds; adapt for WUBRG. Volume via sfxGain bus. |
| Audio: ambient music | Background music sets mood during gameplay | Med | YES -- 14 background tracks, battlefield-specific selection, decoded track cache, looping with cross-fade | AudioBuffer-based playback. Tracks rotate without repeat. Cross-fade on battlefield/track change. |
| Audio: context music | Different music for title screen, deck select, multiplayer lobby | Low | YES -- titleMusic, deckSelectMusic, multiplayerLobbyMusic modules | Each screen has independent start/stop/fade. Low effort to port. |
| Card reveal animation | Full-card popup when spell/creature played | Med | YES -- CardReveal overlay, spell 2.6s / creature 1.9s, element-colored | Persists independently from animation step queue so it doesn't get cut short. |
| Damage vignette | Red screen flash on player damage | Low | YES -- radial gradient overlay, intensity scaled to damage | DamageVignette with intensity = min(maxPlayerDamage/4, 1). Simple but effective. |
| Element card effects | On-card VFX overlay on damaged permanents | Med | YES -- ElementCardEffect per-permanent, fire-and-forget with timeout | Only on vfxLevel=full. For MTG: map to damage source color. |
| Battlefield backgrounds | Themed backgrounds based on deck color | Low | YES -- 8+ battlefield images with element-auto-selection, user preference | Alchemy auto-selects by deck's primary element. For MTG: auto-select by deck's primary WUBRG color. |
| Battlefield ambience particles | Floating particles (embers, snow, bubbles) themed to battlefield | Low | YES -- BattlefieldAmbience with configurable ParticleConfig per battlefield | Framer-motion CSS particles. Togglable via battlefieldAmbience preference. |
| Board snapshot for death animations | Creatures remain visible during death VFX instead of vanishing | Med | YES -- boardSnapshot preserved pre-dispatch, cleared when death step starts | Without this, creatures disappear before death particles/animations play. Critical for visual coherence. |
| Preferences store | Users control VFX level, UI scale, board scale, stat layout | Low | YES -- comprehensive preferencesStore with localStorage persistence | Port display/audio preferences only. Skip learning-related settings. |
| Action feedback toast | Contextual error when card can't be played | Low | YES -- positioned near card, shows reason ("Not enough energy") | For MTG: "Not enough mana", "Can't cast at sorcery speed", "No valid targets", etc. |
| Double-click to play | Single-click selects, double-click plays -- prevents accidental plays | Low | YES -- handleCardClick selects, handleCardDoubleClick dispatches PLAY_CARD | Especially important on touch devices. Single tap shows card details, double tap plays. |
| Animation speed multiplier | User controls animation pacing | Low | YES -- speedMultiplier scales all step durationMs values | Simple preference. Value of 0.5 = 2x speed, 2 = half speed. |
| VFX quality levels | full/reduced/minimal for performance | Low | YES -- vfxLevel preference; particles skip on minimal, element effects only on full | Critical for older devices and users who prefer snappy gameplay. |
| Discard phase prompt overlay | Visual banner when player must discard to hand size | Low | YES -- animated prompt with count, pulsing border | MTG equivalent: end-of-turn discard to 7. Already matches MTG behavior. |

---

## Anti-Features

Features to explicitly NOT port from Alchemy.

| Anti-Feature | Why Avoid | What to Do Instead |
|--------------|-----------|-------------------|
| Learning challenges system (reading/math/word) | Alchemy is educational for kids. Forge.rs is for MTG players. The entire learning/ directory, LearningChallengeOverlay, AdaptiveLearningToast, onboarding, mastery model, and curriculum systems are irrelevant. | Skip entirely. Do not port any file under learning/ or related components/settings. |
| Tutorial/coach system | Alchemy has extensive tutorial infrastructure (TutorialHelpPanel, CoachOverlay, useTutorialTriggers, tutorialStore, stepRegistry, tipPolicy). Forge.rs MTG complexity needs a fundamentally different tutorial approach. | Skip entirely. PROJECT.md explicitly lists tutorial system as out of scope for v1.1. |
| Easy read mode / TTS narration | Alchemy has easyReadMode for simplified card text and speechSynthesis narration for accessibility in educational context. | Skip. MTG cards have canonical oracle text; simplification would be misleading. Card images from Scryfall already show oracle text. |
| "Element" theming as-is | Alchemy maps 5 elements (fire/water/earth/air/shadow) to card frames, art gradients, VFX colors, and spell sounds. MTG has 5 colors (WUBRG) with different aesthetic traditions. | Adapt, don't copy: remap element infrastructure to WUBRG. White=gold/light, Blue=blue/frost, Black=purple/shadow, Red=fire/red, Green=nature/emerald. Keep the architecture, change the constants. |
| Adventure map / campaign mode | Alchemy has AdventureMapBoard. PROJECT.md explicitly excludes campaign/quest modes. | Skip entirely. |
| Alchemy's network layer | Alchemy uses P2P WebRTC (peer.ts, connection.ts, sessionTransfer.ts). Forge.rs already has WebSocket multiplayer via Axum server with hidden information. | Skip. Forge.rs's EngineAdapter WebSocket adapter is architecturally superior for hidden information games. |
| Alchemy's game engine/reducer | Alchemy has its own game state, reducer, validation, AI. Forge.rs has a comprehensive MTG rules engine in Rust. | Use Forge.rs engine exclusively. Alchemy's gameStore dispatch pattern informs the wiring but the game logic is Forge.rs. |
| Preferences: learning settings | readingLevel, mathLevel, learningFrequency, challengeWeights, adaptiveLearning, adaptiveExplanation, learningOnboardingCompleted, learningAgeRange | Skip. Port only: uiScale, boardScale, vfxLevel, battlefield, battlefieldAmbience, statLayout, combatMathEnabled, mathBreakdownEnabled, animationSpeed (new). |
| Energy crystals as mana display | Alchemy uses simple energy pips (one resource type). MTG has 5 colors of mana. | Build MTG-specific mana pool display. Keep EnergyCrystals component architecture but redesign for WUBRG mana symbols. |
| Card cost as element icons x N | Alchemy shows cost as "fire icon x3". MTG shows mana cost as "{2}{R}{R}" with specific symbols. | Build MTG mana cost renderer using standard WUBRG mana symbols. Scryfall card images already show this, but custom card rendering needs it. |

---

## Features Forge.rs Needs That Alchemy Does Not Have

MTG-specific UI that must be built from scratch or preserved from current Forge.rs.

| Feature | Why Needed | Complexity | Forge.rs Status | Notes |
|---------|------------|------------|-----------------|-------|
| Stack visualization | MTG's stack (LIFO resolution) is fundamental. Players must see queued spells/abilities. | Med | EXISTS -- StackDisplay + StackEntry components | Port existing design into new layout. Alchemy has no equivalent (no stack concept). |
| Mana payment UI | MTG mana (5 colors + colorless, hybrid, phyrexian, X costs) needs dedicated payment interface. | High | EXISTS -- ManaPaymentUI with pool display, land tapping, auto-pay | Redesign visually but preserve logic. Most complex MTG-specific UI element. |
| Instant-speed priority interaction | MTG priority allows responses at every pass. Need pass/respond/hold affordance. | Med | EXISTS -- PassButton + FullControlToggle + autoPass in uiStore | Redesign for Arena-style. Must support: auto-pass (no legal responses), full-control (stop at every priority), and smart-stop (stop when responses available). |
| Multi-zone battlefield layout | MTG has creatures, lands, artifacts, enchantments, planeswalkers simultaneously. Alchemy has one creature row per player. | High | EXISTS -- GameBoard partitions by type (creatures/lands/other) via BattlefieldRow | Current layout is basic (3 rows). Need MTGA-style: creatures front-center, lands below, other permanents flanking. |
| Tapped permanent visualization | Tapped permanents rotate 90 degrees. Core MTG visual indicator. | Low | EXISTS -- in PermanentCard | Preserve in new card component. |
| Attacker/blocker declaration UI | MTG combat needs explicit declare-attackers and declare-blockers steps with visual selection. | Med | EXISTS -- combatMode, selectedAttackers, blockerAssignments, combatClickHandler in uiStore | Wire existing state into new visual components. Add Arena-style "all attack" button. |
| Multiple targets selection | MTG spells can target multiple objects/players. | Med | EXISTS -- validTargetIds, selectedTargets, startTargeting in uiStore | Preserve. Alchemy has simpler single-target for most spells. |
| Power/toughness display | Creatures show P/T, modified values highlighted. Planeswalkers show loyalty. | Low | Handled by Scryfall images currently | If rendering custom card faces (not just Scryfall images), need P/T overlay on battlefield cards. |
| Undo for pre-information-reveal actions | Misclicks happen. Forge.rs supports undo. | Low | EXISTS -- stateHistory + undo() in gameStore | Preserve as-is. |
| WUBRG mana symbols | MTG's 5-color mana symbol system (plus colorless, hybrid, phyrexian, snow). | Low | Handled by Scryfall images | Need symbol renderer for mana costs, mana pool display, card cost display in hand. |

---

## Feature Dependencies

```
CSS Custom Properties (--card-width, --card-height, --ui-scale, --board-scale)
  └─→ Card Component Rendering
       └─→ Board Layout (CreatureSlots, BattlefieldRow)
            └─→ Hand Layout (PlayerHand fan, OpponentHand)
                 └─→ Everything visual

Preferences Store (vfxLevel, uiScale, boardScale)
  └─→ Display Settings panel
  └─→ All visual components read preferences

Canvas ParticleSystem
  └─→ Combat strike projectiles
  └─→ Spell impact bursts
  └─→ Death explosions
  └─→ Summon bursts
  └─→ Damage/heal flash effects

Animation Store (step queue, board snapshots, display health)
  └─→ Floating damage/heal numbers
  └─→ Screen shake (via shakeIntensity)
  └─→ Card reveal overlay
  └─→ Damage vignette
  └─→ Math breakdown overlays
  └─→ Element card effects

Audio Context + Gain Bus
  └─→ SFX system (triggerSoundEffect)
  └─→ Ambient music (ambientMusic.ts)
  └─→ Context music (title, deck select)

Block Assignment Lines (SVG between cards)
  └─→ Combat Math Bubbles (positioned at line midpoints)

Hero HUD container
  └─→ Health Bar / Health Badge
  └─→ Mana pool summary (replaces EnergyCrystals)
  └─→ Phase indicator (replaces PhaseDiamonds for MTG phases)

Stack Visualization ←→ Priority UI ←→ Mana Payment UI
  (Core MTG interaction loop; must work as integrated system)
```

---

## MVP Recommendation

### Phase 1: Core Layout and Card Rendering
Prioritize:
1. CSS custom property card sizing (--card-width, --card-height, --card-font-scale, --ui-scale, --board-scale)
2. Responsive game board layout (opponent board / battle line / player board)
3. MTGA-style hand peek/expand with fan layout and drag-to-play
4. Hero HUD strip (health bar, mana pool summary, phase indicator)
5. Preferences store (display settings: uiScale, boardScale, vfxLevel)
6. Graveyard viewer modal

**Rationale:** Nothing else works until cards render at correct sizes in correct positions. The hand interaction (peek/expand/drag) is the primary touch point -- it must feel right first.

### Phase 2: Animation Pipeline
Prioritize:
1. Port Alchemy's ParticleSystem and particleEffects (remap element colors to WUBRG)
2. AnimationStore with step queue, board snapshots, displayHealth tracking
3. Floating damage/heal numbers with per-step intermediate values
4. Screen shake (3 intensity CSS classes)
5. Card reveal overlay (adapt for MTG card faces)
6. Damage vignette
7. Block assignment lines (SVG)

**Rationale:** Animation pipeline is the largest visual upgrade. It transforms every game event from a state change into a visual experience. Board snapshots must be in place for death animations to look correct.

### Phase 3: Audio System
Prioritize:
1. Audio context + sfx/music gain bus architecture
2. SFX tied to animation effects (combat_strike, damage, death, spell_impact, summon, keyword)
3. Ambient background music with track rotation and cross-fade
4. Sound effect prewarm pipeline

**Rationale:** Audio is high-impact but fully independent of visual features. Can be built in parallel with Phase 2. The sample-based approach with procedural fallback means it works even without all audio assets.

### Phase 4: Combat and Interaction Polish
Prioritize:
1. Double-click to play with single-click select
2. Combat math bubbles (adapted for MTG P/T + keywords)
3. Action feedback toast (MTG-specific messages: "Not enough mana", "Sorcery speed only")
4. Battlefield backgrounds with WUBRG-based auto-selection
5. Battlefield ambience particles
6. Math breakdown overlays
7. Animation speed preference

**Rationale:** Polish features that make "functional" become "enjoyable." Combat math bubbles are particularly valuable for MTG because combat math with first strike, trample, and deathtouch is genuinely complex.

### Phase 5: MTG-Specific UI (New Build, Not Port)
Prioritize:
1. Stack visualization redesign (Arena-style, items resolve top-down)
2. Mana payment UI redesign (WUBRG symbols, hybrid handling, X cost slider)
3. Priority pass/respond controls (auto-pass, full-control toggle, smart-stop)
4. Multi-zone battlefield (creatures front, lands behind, enchantments/artifacts flanking)
5. WUBRG color theming system (constant maps, mana symbols, card border tinting)

**Rationale:** These features define what makes it an MTG client rather than a generic card game. Cannot be ported from Alchemy -- must be designed for MTG's specific complexity. Phase 5 is last because the existing Forge.rs UI already handles these functionally; this phase is about bringing them into the new visual language.

### Defer:
- **Context-specific music** (title, deck select, lobby) -- nice polish, not gameplay
- **Tutorial/coach system** -- explicitly out of scope per PROJECT.md
- **Custom UI layouts** -- default must be good first; customization is v2+
- **Element card effects on permanents** -- subtle, only visible at vfxLevel=full

---

## Sources

- Direct source code analysis: `/Users/matt/dev/alchemy/src/` (all component, store, audio, and hook files)
- Direct source code analysis: `/Users/matt/dev/forge.rs/client/src/` (all component, store, and adapter files)
- Project manifest: `/Users/matt/dev/forge.rs/.planning/PROJECT.md`
