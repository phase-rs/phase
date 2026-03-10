# Phase 17: MTG-Specific UI - Context

**Gathered:** 2026-03-09
**Status:** Ready for planning

<domain>
## Phase Boundary

Players interact with MTG-specific mechanics through polished UI — stack visualization, mana payment, combat assignment, and priority controls. This phase upgrades existing v1.0 stubs (StackDisplay, ManaPaymentUI, CombatOverlay, PassButton) to Arena-quality, using Alchemy's proven components as the starting point where applicable. Audio system (Phase 16) and engine changes are separate phases.

</domain>

<decisions>
## Implementation Decisions

### Alchemy port scope
- Port Alchemy's ActionButton as the unified combat/phase-advance orchestrator, replacing current PassButton, CombatOverlay, AttackerControls, and BlockerControls
- Port BlockAssignmentLines (animated SVG dashed lines with glow and pulse dots) replacing current BlockerArrow
- Port buttonStyles (gameButtonClass helper with tone variants) for consistent button styling
- Port usePhaseInfo hook, fully rewritten for MTG's multi-step turn structure (Untap → Upkeep → Draw → Main 1 → Combat phases → Main 2 → End → Cleanup)
- Skip CombatMathBubbles — not porting combat math preview for MTG due to keyword complexity (first strike, deathtouch, trample, etc.)
- Hybrid architecture: ActionButton handles combat controls + phase advance + pass/resolve, while StackDisplay is a separate persistent component for stack visualization
- Use `legalActions` from gameStore (already populated via `get_legal_actions_js()` WASM export) to drive ActionButton visibility — same pattern as Alchemy

### Stack visualization
- Right-side column position, always visible when stack has items
- Full Scryfall card image previews in a staggered pile, newest on top
- Cards dynamically shrink as more items are added to the stack (container-aware sizing)
- "Resolve" button passes priority for top item; "Resolve All" auto-passes for each item sequentially with animations (not instant batch resolution)
- Player can interrupt Resolve All by playing a card or activating an ability (holds priority)
- "Resolves Next" label on the top stack item
- No special "respond" mode — player plays instants from hand or activates abilities from board normally while stack is visible

### Mana payment
- Silent auto-pay: engine automatically selects optimal mana sources and taps lands without player confirmation
- Mana payment UI only appears for ambiguous costs: hybrid (W/U), phyrexian (2 life or colored mana), and X costs
- X costs: horizontal slider showing 0 to max-affordable-X with resulting total cost display (e.g., "X=3: Pay 3R")
- Phyrexian mana: inline toggle per symbol — tap to switch between mana icon and heart icon (pay life). Shows total life cost
- Hybrid mana: inline toggle per symbol — tap to switch between color options
- No manual mana tapping modal for simple costs

### Combat interaction
- Skip-confirm guard pattern from Alchemy: first tap on "No Attacks" arms it, second tap confirms. Same for "No Blocks"
- "All Attack" button toggles all legal attackers (like Alchemy)
- "Clear Attackers" / "Clear Blocks" buttons when selections exist
- Blocker assignment: click blocker creature, then click attacker to assign (current Forge.rs pattern, works well)
- Multi-blocker damage assignment: engine auto-distributes damage optimally (lethal to first, remainder to next). Player can open override modal to manually redistribute
- Block assignment lines use ported BlockAssignmentLines from Alchemy (animated dashes, glow, pulse dots)
- Blocker order step: "Choose block order" with Resolve button (from Alchemy)
- Combat priority window: inline stack pile display with "Proceed to Blockers" / "Resolve Combat" labels (from Alchemy's CombatPriorityControls)

### Claude's Discretion
- Exact stack card sizing algorithm (min/max sizes, shrink curve as stack grows)
- ActionButton positioning (safe-area-aware, responsive)
- Animation timing for stack entry/exit transitions
- Damage assignment modal layout and slider/button design
- How usePhaseInfo maps MTG phases to display keys and advance actions
- Whether to keep PhaseStopBar separate or integrate into usePhaseInfo

</decisions>

<specifics>
## Specific Ideas

- "I really like the UI and UX in Alchemy, and would REALLY like to be able to use that in whole and build off of it" — Alchemy is the primary UI reference, not just inspiration
- Card rendering stays as-is (Scryfall images via current CardImage component) — Alchemy port is for interaction patterns, layout, and controls
- Stack should feel like a real pile of cards, not a list — full card images with physical stacking
- "Resolve All" auto-passes sequentially with animations, not instant batch — player should see each spell resolve and can interrupt

</specifics>

<code_context>
## Existing Code Insights

### Alchemy Components to Port
- `ActionButton` (alchemy/src/components/board/ActionButton.tsx): 535-line unified orchestrator for combat, phase advance, stack priority, and resolving
- `BlockAssignmentLines` (alchemy/src/components/board/BlockAssignmentLines.tsx): SVG overlay with animated dashes, glow filters, pulsing dots, RAF position polling
- `usePhaseInfo` (alchemy/src/hooks/usePhaseInfo.ts): Phase display mapping with advance actions — needs full rewrite for MTG phases
- `buttonStyles` (alchemy/src/components/ui/buttonStyles.ts): gameButtonClass helper with tone variants (red, blue, slate, amber, indigo)
- `boardSizing` (alchemy/src/components/board/boardSizing.ts): Dynamic card sizing by container/slot count — useful for stack card sizing

### Forge.rs Components to Replace/Upgrade
- `PassButton` → replaced by ActionButton's resolve/pass controls
- `CombatOverlay` + `AttackerControls` + `BlockerControls` → replaced by ActionButton's combat controls
- `BlockerArrow` → replaced by BlockAssignmentLines
- `StackDisplay` + `StackEntry` → upgraded to full card preview pile (keep as separate component, not part of ActionButton)
- `ManaPaymentUI` → upgraded with hybrid/phyrexian/X cost support, but only shown for ambiguous costs

### Reusable Forge.rs Assets
- `gameStore.legalActions`: Already populated via WASM `get_legal_actions_js()` — drives ActionButton visibility
- `WaitingFor` discriminated unions: Still used for game flow state; ActionButton reads both WaitingFor and legalActions
- `CardImage` component: Scryfall image rendering — used for stack card previews
- `preferencesStore`: Persists settings — add combatMathEnabled toggle (future), fullControlMode already exists via uiStore
- `PhaseStopBar` + `PhaseTracker`: Existing MTG phase display — usePhaseInfo complements these
- `useGameDispatch` / dispatch pipeline: Animation-aware dispatch — ActionButton dispatches through this

### Established Patterns
- WaitingFor-driven component visibility (ManaPaymentUI, CombatOverlay already do this)
- legalActions-driven button filtering (PlayerHand highlights, autoPass checks)
- Module-level empty arrays for Zustand selector stability
- Framer Motion AnimatePresence for enter/exit transitions
- createPortal for overlay components (BlockAssignmentLines, CombatMathBubbles in Alchemy)

### Integration Points
- `gameStore.waitingFor` + `gameStore.legalActions`: Both drive ActionButton state
- `dispatch()` in gameStore: ActionButton dispatches GameActions through existing pipeline
- `animationStore`: Stack entry/exit animations feed through animation pipeline
- `uiStore.combatMode` + `uiStore.selectedAttackers` + `uiStore.blockerAssignments`: Combat selection state

</code_context>

<deferred>
## Deferred Ideas

- Combat math bubbles — deferred due to MTG keyword complexity. Could be added as a future enhancement once all keyword interactions are solid
- Manual mana tapping mode / power-user override — could be a future toggle in preferences
- Stack item hover preview (show full-size card on hover) — nice-to-have, not essential for v1.1

</deferred>

---

*Phase: 17-mtg-specific-ui*
*Context gathered: 2026-03-09*
