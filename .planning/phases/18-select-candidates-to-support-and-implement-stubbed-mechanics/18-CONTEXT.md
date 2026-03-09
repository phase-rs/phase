# Phase 18: Select Candidates to Support and Implement Stubbed Mechanics - Context

**Gathered:** 2026-03-09
**Status:** Ready for planning

<domain>
## Phase Boundary

Analyze Standard-legal card frequency to identify which of the ~160 stubbed mechanics to implement, then implement them in tiered batches — combat keywords first, then common non-combat mechanics, then architecturally-clean subsystems. Includes test infrastructure (unit + integration with real Forge cards), a mechanic coverage report, and card-level UI warnings for remaining unimplemented mechanics. New game modes, new UI features, and deck builder changes are separate phases.

</domain>

<decisions>
## Implementation Decisions

### Selection criteria
- Rank all stubbed mechanics by Standard-legal card frequency (analyze card-data.json or Forge DB)
- Tiebreaker: prefer simpler mechanics when frequency is similar
- Dependency-aware: always include prerequisite mechanics (e.g., Flying before Reach matters, counters before Proliferate)
- Combat keywords get top priority since combat damage infrastructure already exists (First Strike, Double Strike, Trample, Deathtouch, Lifelink implemented)

### Scope & tiering
- Tier 1 (Plan 1): All combat keywords — Flying, Reach, Menace, Vigilance, Defender, Haste, Hexproof, Shroud, Flash, Fear, Intimidate, Skulk, Shadow, Horsemanship, Indestructible
- Tier 2 (Plan 2-3): Common non-combat mechanics by Standard frequency — Scry, Mill, Surveil, Ward, Protection, Proliferate, Explore, Cycling, Kicker, ETB variants, block/attack restrictions
- Tier 3 (Plan 4+ if architecturally clean): Complex subsystems — Transform/DFCs, Morph, Phasing, Day/Night — include only if clean architecture path exists
- Maximize feature coverage: implement everything we feel confident about, no artificial ceiling
- Architecture fit is the gating criterion, not complexity alone — if a mechanic fits naturally in the existing reducer/event pattern, include it even if it's a bigger lift

### Category ordering
- Plan 1: Combat keywords (plug into existing combat logic)
- Plan 2: Effect handlers (Scry, Mill, Surveil) + common triggered abilities
- Plan 3: Static abilities (Ward, Protection, CantBeBlocked, damage prevention, block/attack restrictions)
- Plan 4+: Complex subsystems that pass architecture assessment during research
- Within each plan, batch related mechanics together for engineering efficiency

### Rules authority
- MTG comprehensive rules as primary authority for mechanic behavior
- Forge Java source as reference only, not authoritative — don't replicate Forge bugs
- When rules are ambiguous, consult Forge Java for practical interpretation

### Validation approach
- Unit tests: 2-5 inline Rust tests per mechanic covering basic behavior, interaction with existing mechanics, and MTG rules edge cases
- Integration tests: Load real card definitions from Forge database (e.g., load_card("Serra Angel")) to verify full pipeline (parse → build → play)
- Create reusable test helper utility for loading cards from Forge DB and setting up game scenarios
- Mechanic coverage report: script or test that counts Standard cards with full vs partial vs no mechanic support, outputting percentage
- Card-level UI warning: visual indicator on cards with unimplemented mechanics so players know before playing them

### Claude's Discretion
- Exact mechanic ranking from card frequency analysis (research phase will produce this)
- How to batch mechanics within plans for engineering efficiency
- Whether card-level warning lives engine-side (has_unimplemented_mechanics flag on GameObject) or client-side (keyword checklist)
- Test helper architecture and API design
- Coverage report format and output location
- Whether specific subsystem mechanics (Transform, Morph, etc.) pass architecture assessment

</decisions>

<specifics>
## Specific Ideas

- "Feature coverage is highly important — implement as much as we reasonably can if we feel confident about it"
- "Ideally we build this out if we have an idiomatic clean architecture path" — architecture quality is the gating criterion, not complexity
- Combat keywords first because the combat damage system already handles First Strike, Double Strike, Trample, Deathtouch, Lifelink — extending it is natural
- Integration tests should use real Forge card definitions to prove the full parse-to-gameplay pipeline works

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `crates/engine/src/game/combat_damage.rs`: Implements First Strike, Double Strike, Trample, Deathtouch, Lifelink — pattern for adding more combat keywords
- `crates/engine/src/game/effects/mod.rs`: 15 effect handlers (DealDamage, Draw, ChangeZone, etc.) — pattern for adding Scry, Mill, Surveil handlers
- `crates/engine/src/game/triggers.rs`: ~20 implemented trigger modes + 57 stubbed — add match arms for new triggers
- `crates/engine/src/game/static_abilities.rs`: ~15 implemented static handlers + 37+ stubbed — add handlers for new statics
- `crates/engine/src/game/replacement.rs`: 21 stubbed replacement events — add handlers as mechanics are implemented
- `crates/engine/src/types/keywords.rs`: All 200+ keywords already parsed into typed Rust enums — no parser changes needed
- `crates/engine/src/database/`: Card database with Forge card definitions — basis for integration test helper
- `client/src/adapter/types.ts`: GameObject type with all fields — extend for UI warnings if needed

### Established Patterns
- Pure reducer: `apply(state, action) -> ActionResult { events, waiting_for }` — all new mechanics follow this
- Discriminated unions with `#[serde(tag = "type", content = "data")]` — new events/actions extend existing enums
- Inline `#[cfg(test)]` modules for unit tests — each game logic file has its own tests
- `rpds` persistent data structures for immutable state — new state fields use the same
- Effect handlers: `fn apply_effect(state, effect) -> Vec<GameEvent>` — pattern for new effects

### Integration Points
- `crates/engine/src/game/combat.rs`: Attack/block validation — add evasion checks (Flying, Menace, Fear, etc.)
- `crates/engine/src/game/casting.rs`: Spell casting — add Flash, alternative costs if implementing Kicker etc.
- `crates/engine/src/game/engine.rs`: Main `apply()` reducer — new action/event handling
- `crates/engine/src/game/sba.rs`: State-based actions — some mechanics trigger SBAs
- `crates/engine/src/game/layers.rs`: Layer system (MTG Rule 613) — static abilities that modify P/T or grant abilities

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics*
*Context gathered: 2026-03-09*
