# Milestones

## v1.1 Arena UI (Shipped: 2026-03-10)

**Delivered:** A polished MTGA-quality game client with art-crop cards, cinematic animations, audio system, and 100% Standard-legal card coverage — replacing the v1.0 prototype UI.

**Phases:** 8 | **Plans:** 43 | **Commits:** 305 | **LOC:** ~51.5k (33.4k Rust + 18.1k TypeScript)
**Timeline:** 2 days (2026-03-08 → 2026-03-09)
**Git range:** v1.0..HEAD
**Requirements:** 87/87 satisfied

**Key accomplishments:**
1. MTGA-quality game board with responsive layout, hand fan, player HUD, zone viewers, and game log
2. Canvas particle VFX engine with step-based animation queue, screen shake, floating damage numbers, and death shatter
3. AI auto-play game loop with auto-pass heuristics, phase stops, and opponent controller abstraction
4. Web Audio API sound effects (39 SFX) and WUBRG-themed background music with volume controls
5. Stack visualization, smart mana auto-pay with hybrid/phyrexian/X cost UI, combat assignment, and priority controls
6. 20+ new engine mechanics: evasion keywords, Ward/Protection, Wither/Infect, Prowess, and coverage reporting
7. MTGA-faithful art-crop cards, golden targeting arcs, cinematic turn banners, mode-first menu, and deck gallery
8. Complete engine: mana abilities (Rule 605), planeswalkers, DFCs, day/night, morph/manifest — 100% Standard coverage with CI gate

---

## v1.0 MVP (Shipped: 2026-03-08)

**Delivered:** A playable MTG game engine in Rust/WASM with React UI, AI opponent, WebSocket multiplayer, and deck builder — covering 60%+ of Standard-legal cards.

**Phases:** 12 | **Plans:** 40 | **Commits:** 207 | **LOC:** ~29,700 (22.5k Rust + 7.2k TypeScript)
**Timeline:** 2 days (2026-03-07 → 2026-03-08)
**Git range:** `feat(01-01)` → `feat(12-02)`
**Requirements:** 63/63 satisfied

**Key accomplishments:**
1. Complete MTG rules engine with 202 effect handlers, 137 trigger modes, 45 replacement effects, and seven-layer continuous effects (Rule 613)
2. Full combat system with first strike, double strike, trample, deathtouch, lifelink, flying/reach, menace, and keyword interactions
3. Card parser loading Forge's 32,300+ card definitions with multi-face support (Split, Transform, MDFC, Adventure)
4. React game UI with battlefield, hand, stack, targeting, mana payment, animations, and deck builder with .dck import
5. AI opponent with alpha-beta search, board evaluation, per-card hints, and 5 difficulty levels
6. WebSocket multiplayer server with hidden information, action sync, reconnection support, and server-side deck resolution

**Tech debt (informational):**
- 117 rare trigger modes return false until cards need them
- Orphaned createAdapter() export (superseded by mode-aware selection)
- coverage-data.json is placeholder (needs real card data from coverage_report binary)

---

---

