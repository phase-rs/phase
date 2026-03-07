# Feature Landscape

**Domain:** MTG digital game engine and client (port of Java Forge to Rust/TypeScript)
**Researched:** 2026-03-07

## Competitive Landscape

| Client | Type | Rules Engine | AI | Multiplayer | Formats | Card Coverage |
|--------|------|-------------|-----|-------------|---------|---------------|
| **Forge** (Java) | Desktop/Android | Full enforcement | Yes (heuristic) | AI + limited P2P | All | 32,300+ cards |
| **MTG Arena** | Commercial client | Full enforcement | No (human-only) | Online matchmaking | Standard/Historic/Limited | Current Standard+ |
| **XMage** | Desktop (Java) | Full enforcement | Yes (basic) | Server-based | All | 30,000+ cards |
| **Cockatrice** | Desktop | None (honor system) | No | Server-based | All (manual) | All (no enforcement) |
| **Untap.in** | Browser | None (honor system) | No | Browser P2P | All (manual) | All (no enforcement) |
| **MTGO** | Commercial client | Full enforcement | No | Online matchmaking | All | Complete |

Forge.ts competes most directly with **Java Forge** (same scope, modernized) and **XMage** (rules enforcement + multiplayer). MTGA is the quality bar for UI/UX but is commercial and format-limited.

---

## Table Stakes

Features users expect. Missing = product feels incomplete.

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| **Correct rules engine** | Users play MTG to play MTG; wrong rules = broken product | Very High | Turn structure, priority, stack, state-based actions, zones, combat |
| **Priority system with the stack** | Core MTG mechanic; instants, responses, triggers all depend on it | High | LIFO resolution, priority passing, holding priority |
| **Full mana system** | Every spell needs mana; broken mana = unplayable | High | 5 colors, colorless, generic, hybrid, phyrexian, X costs, snow |
| **Combat system** | Win condition for most decks | High | Attack/block declaration, damage assignment, first/double strike, trample, deathtouch, lifelink |
| **Card image display** | Players identify cards visually; text-only is unusable for most | Medium | Scryfall API, on-demand loading, local cache |
| **Card preview/zoom** | Board gets crowded; need to read card text on demand | Low | Hover or click to enlarge, show oracle text |
| **Battlefield layout** | Must clearly show game state: permanents, tapped state, attachments | High | Grid layout, tap rotation, equipment/aura attachment visualization |
| **Hand display** | Player needs to see and interact with cards in hand | Medium | Fan layout, legal-play highlighting, drag or click to play |
| **Phase/turn tracker** | Players must know current phase and whose turn it is | Low | Visual indicator, current phase highlighted |
| **Life total display** | Fundamental game state | Low | Both players' life totals, change animations |
| **Targeting UI** | Many spells/abilities require targets | Medium | Valid target highlighting, click-to-select, cancel |
| **Mana payment UI** | Auto-pay for simple costs, manual selection for complex | Medium | Auto-tap with manual override; Arena's auto-tapper is notoriously imperfect, opportunity to do better |
| **Deck builder** | Players need to build and edit decks before playing | Medium | Card search, filtering by color/type/CMC, mana curve display |
| **Deck import/export** | Players have existing decklists from websites/other tools | Low | .dck/.dec format from Forge, Arena text format, Moxfield-compatible |
| **Standard format support** | Most popular constructed format, stated project goal | High | Card coverage for last 2 years of sets |
| **AI opponent** | Primary play mode for Forge; single-player is the core use case | High | Must play legal moves; heuristic quality determines replayability |
| **Game log** | Players need to understand what happened, especially complex interactions | Low | Text log of actions, zone changes, triggers, damage |
| **Undo (pre-priority-pass)** | Misclicks happen; Arena has Z key undo for unrevealed-information actions | Low | Undo tapping wrong land, undo mana ability activation |

---

## Differentiators

Features that set Forge.ts apart from competitors. Not expected, but valued.

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| **Native + WASM dual target** | Play on desktop (fast) or tablet/browser (convenient) from same codebase | Medium | Rust compiles to native + WASM; same React UI, thin adapter layer |
| **Touch-optimized UI** | No MTG client does tablet well; Arena mobile is compromised, Forge/XMage are desktop-only | Medium | CSS Grid layouts, larger tap targets, gesture support |
| **Smart auto-tap** | Arena's auto-tapper is widely criticized; opportunity to build a better one | Medium | Context-aware: consider cards in hand, floating mana needs, utility lands |
| **Fast AI with game tree search** | Forge's Java AI is heuristic-only; Rust enables deeper search in real time | High | Lookahead search in native Rust is orders of magnitude faster than Java/JS |
| **Per-card AI logic** | Forge has this but it's messy; clean implementation is a differentiator | High | Card-specific heuristics for when to cast, what to target, combat math |
| **Modern UI/animations** | Forge and XMage look dated; modern React + Framer Motion can match Arena's polish | High | Card movement animations, damage numbers, combat arrows, spell effects |
| **Offline-first PWA** | Install on iPad, play offline against AI anywhere | Low | Service worker caching, IndexedDB for card data and images |
| **Auto-yield system** | Skip repetitive triggers (e.g., "Whenever a creature enters" on mass token creation) | Medium | Right-click to auto-yield specific triggers; Forge has this, Arena has partial support |
| **Keyboard shortcuts** | Power users want fast interaction; Arena's shortcuts are popular | Low | Pass turn, full control toggle, tap all lands, undo |
| **Card coverage dashboard** | Show which cards are supported, which are partial; transparency builds trust | Low | Track implemented ApiTypes, show % coverage per set/format |
| **Macro system** | Forge has Shift+R record / @ execute for repeatable action sequences | Medium | Record and replay action sequences for repetitive plays |
| **Multiple AI difficulty levels** | Casual to competitive; vary search depth and heuristic aggression | Medium | Easy (random-ish), Medium (heuristic), Hard (lookahead), per Forge's approach of deck difficulty + AI quality |
| **Customizable UI layout** | Forge has draggable/resizable panels saved as XML profiles | Medium | Let players arrange battlefield, hand, stack, log however they want |

---

## Anti-Features

Features to explicitly NOT build.

| Anti-Feature | Why Avoid | What to Do Instead |
|--------------|-----------|-------------------|
| **Collection/economy system** | Forge.ts is open-source with all cards available; paywalls are antithetical to the project | All cards available, no wildcards/packs/currency |
| **Cosmetics store** | Not a revenue product; adds complexity for zero gameplay value | Focus engineering on gameplay and rules correctness |
| **Draft/Sealed modes (v1)** | Significant UI complexity (pack display, pick tracking, pool building, deck construction timer) beyond core 1v1 | Defer to v2; focus on constructed first |
| **Quest/Adventure mode (v1)** | Narrative framework, progression systems, unlocks, overworld map -- huge scope | Defer to v2+; Forge's quest mode is beloved but is a separate product layer |
| **Commander/multiplayer formats (v1)** | 3-4 player multiplayer adds massive UI, networking, and rules complexity (political mechanics, command zone, commander tax, partner) | 1v1 only for v1; architecture should not preclude future multiplayer |
| **Chat system** | Moderation burden, abuse potential, minimal gameplay value for AI-primary client | Game log is sufficient; if multiplayer added, simple emote system |
| **Social features (guilds, friends, leaderboards)** | Backend infrastructure for user accounts, persistence, matchmaking | Out of scope for open-source desktop/PWA game engine |
| **Alchemy/digital-only mechanics** | Arena-specific card modifications (rebalanced cards, conjure, seek, perpetually) are not real MTG | Support paper MTG rules only |
| **Sound effects/music** | Nice-to-have but significant asset creation/licensing burden with no gameplay impact | Silent by default; audio is a v2+ polish feature |
| **Real-time card rulings lookup** | Fetching Gatherer/Scryfall rulings during gameplay adds network dependency and UI complexity | Correct rules engine makes rulings unnecessary; oracle text on card preview is sufficient |
| **Sideboard between games** | Best-of-3 with sideboarding adds UI flow (game end -> sideboard screen -> re-shuffle -> game start) | Best-of-1 for v1; sideboard import supported for future use |

---

## Feature Dependencies

```
Card Parser ─────────────────────────────────────────────────┐
  │                                                          │
  v                                                          │
Core Types ──> Game State Engine ──> Ability System ─────────┤
                    │                    │                    │
                    v                    v                    │
              Zone Management      Effect Handlers           │
                    │                    │                    │
                    v                    v                    │
              Turn/Phase System    Trigger System             │
                    │                    │                    │
                    v                    v                    │
              Priority/Stack      Replacement Effects         │
                    │                    │                    │
                    v                    v                    │
              Combat System ──> Static Abilities/Layers       │
                    │                    │                    │
                    v                    v                    │
              Keyword System ←── All systems feed into ──────┘
                    │
                    v
              Card Coverage (long tail of effects)
                    │
                    ├──> AI System (needs: legal move enumeration, board eval, game tree)
                    │
                    └──> React UI (can develop in parallel from Phase 2 onward)
                              │
                              ├──> Deck Builder (independent of game engine state)
                              ├──> Card Preview (needs: Scryfall image loading)
                              └──> Targeting/Mana Payment UI (needs: game engine integration)

Multiplayer (needs: working game engine + UI, adds: WebSocket server, hidden info)
```

Key dependency insights:
- **UI can start early** -- basic battlefield/hand rendering only needs zone state, not full rules
- **AI requires a working rules engine** -- it must enumerate legal actions, which needs most subsystems
- **Deck builder is independent** -- can be built any time, only needs card database
- **Multiplayer is last** -- requires everything else to work in single-player first
- **Keyword system depends on triggers, statics, and replacements** -- keywords generate these

---

## MVP Recommendation

**Goal:** A player can play a full game of Standard-legal Magic against an AI opponent with correct rules.

### Must ship (MVP):

1. **Card parser** -- Parse Forge's .txt card definitions (compatibility layer)
2. **Core rules engine** -- Turns, priority, stack, zones, state-based actions, mana
3. **Top 15 effect types** -- Draw, DealDamage, ChangeZone, Pump, Destroy, Counter, Token, Tap/Untap, Mana, Sacrifice, Discard, PutCounter, GainLife, LoseLife, Attach (covers ~60% of cards)
4. **Trigger system** -- At minimum ETB, dies, attacks, spell cast, damage dealt
5. **Combat system** -- Attack/block, damage, core keywords (flying, trample, first strike, deathtouch, lifelink, haste, vigilance, reach, menace)
6. **Static abilities** -- Continuous effects with basic layer evaluation
7. **Basic AI** -- Heuristic: play lands, cast best spell, attack when favorable, block to survive
8. **Game UI** -- Battlefield, hand, stack, phase tracker, life totals, card preview, targeting, mana payment
9. **Deck builder** -- Card search, drag-to-add, mana curve, import .dck files
10. **Game log** -- Text log of all actions and events

### Defer to post-MVP:

- **Advanced AI** (lookahead search, per-card logic) -- Heuristic AI is playable; deep search is polish
- **Remaining 187 effect types** -- Long tail, each independent, add incrementally by card frequency
- **Multiplayer** -- Single-player is the core Forge experience; multiplayer is additive
- **Touch optimization** -- Desktop-first is fine for initial validation; tablet polish comes after
- **Animations** -- Functional card movement first, then smooth transitions
- **Auto-yield / macros** -- Quality of life that matters after many hours of play
- **Custom UI layouts** -- Default layout must be good; customization is later
- **Keyboard shortcuts** -- Nice but not blocking; add incrementally

### Why this order:

The MVP validates the **hardest technical risk** (rules engine correctness) with the **minimum viable surface** (Standard format, ~500-1000 playable cards). If the architecture holds for Lightning Bolt, Counterspell, and a creature combat, it scales to all 32,300 cards. Everything deferred is either additive (multiplayer, modes), incremental (more effects), or polish (animations, shortcuts).

---

## Sources

- [Forge Official Site](https://card-forge.github.io/forge/)
- [Forge GitHub](https://github.com/Card-Forge/forge)
- [Forge User Guide Wiki](https://github.com/Card-Forge/forge/wiki/User-Guide)
- [Forge AI Wiki](https://github.com/Card-Forge/forge/wiki/AI)
- [MTG Arena Official](https://magic.wizards.com/en/mtgarena)
- [MTG Arena Hotkeys Guide (MTG Arena Zone)](https://mtgazone.com/arena-hot-keys-and-interface-guide-simplify-your-game-with-these-easy-tricks/)
- [XMage GitHub](https://github.com/magefree/mage)
- [Untap.in](https://untap.in/)
- [Forge vs XMage comparison (CGomesu)](https://cgomesu.com/blog/forge-xmage-mtg/)
- [SaaSHub: Cockatrice vs Forge](https://www.saashub.com/compare-cockatrice-vs-forgemtg)
- [SaaSHub: XMage vs Forge](https://www.saashub.com/compare-xmage-vs-forgemtg)
- [MTG Arena UX Design (Medium)](https://medium.com/the-space-ape-games-experience/magic-the-gathering-and-ui-design-a897c8b04719)
