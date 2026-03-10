# Phase 20: Implement All Remaining Effects, Keywords, Statuses, and Stubbed Mechanics - Context

**Gathered:** 2026-03-09
**Status:** Ready for planning

<domain>
## Phase Boundary

Complete the engine's mechanic coverage to 100% of Standard-legal cards with best-effort Pioneer coverage. Implement all remaining stubbed effects, static abilities, triggers, and replacement effects. Add foundational systems currently missing: mana abilities, equipment/aura attachment, planeswalker loyalty, transform/DFCs, day/night, morph/manifest. Fix simplified implementations (Scry, Dig, Surveil) with proper WaitingFor interactive choices. Add coverage report CI gate to prevent regressions. New game modes, new UI pages, and deck builder changes are separate phases.

</domain>

<decisions>
## Implementation Decisions

### Coverage target & scope
- 100% Standard-legal card coverage as the hard target
- Best-effort Pioneer coverage (high-frequency Pioneer mechanics included)
- Coverage report CI gate: Standard-legal subset of Forge card data checked into repo, coverage-report binary runs in CI, 0 unsupported Standard cards = pass
- Coverage report runs at phase start (baseline) and phase end (final validation), not per-plan

### Foundational systems (Plan 1 — first priority)
- Mana abilities: implement full MTG rule 605 (mana abilities resolve instantly, don't use the stack, special action resolution)
- Equipment/Aura attachment: equip/enchant mechanics, attachment validation, unattach on creature death, SBA enforcement
- WaitingFor interactive choices: proper WaitingFor::ScryChoice, DigChoice, SurveilChoice for player decisions instead of auto-choose
- Scry UI: per-card top/bottom buttons (MTGA-style), not drag-to-reorder
- Choice UI for Dig/Explore/Surveil: Claude's discretion — generic card-selection modal or per-mechanic, whichever fits existing patterns best

### Complex subsystems (all in scope)
- Transform/DFCs: face switching, CardLayout::Transform integration, UI shows hover-to-peek back face (like MTGA)
- Planeswalker loyalty: loyalty counter activation, once-per-turn restriction, damage-to-loyalty redirection
- Day/Night & Daybound/Nightbound: global GameState field, turn-based triggers, werewolf transformation
- Morph/Manifest/Disguise: face-down 2/2 representation, turn face-up triggers, hidden information in multiplayer (server hides identity from opponents, extends existing server-core hidden info pattern)
- Philosophy: Forge implements all these, so we implement all of them — architecture assessment during research but no artificial scope reduction

### AI behavior
- Full AI evaluation for interactive choices (Scry, Dig, Surveil, etc.) using existing card hint system + game tree search
- Not simplified heuristics — AI should make proper decisions

### Batching & ordering
- Foundation-first approach: mana abilities + equipment/aura + WaitingFor system first (bundled)
- Then planeswalker loyalty + transform/DFCs
- Then static abilities (promote stubs to real handlers)
- Then trigger matchers
- Then effect handlers
- Then replacement effects
- Then complex subsystems (day/night, morph/manifest)
- Then coverage CI gate
- Flexible plan count: research determines exact number (estimated 6-12 plans based on dependency analysis)

### Rules authority (carried from Phase 18)
- MTG comprehensive rules as primary authority
- Forge Java source as reference implementation, not authoritative — don't replicate Forge bugs
- When rules are ambiguous, consult Forge Java for practical interpretation

### Validation approach (carried from Phase 18)
- Unit tests: 2-5 inline Rust tests per mechanic
- Integration tests: real Forge card definitions via test_helpers infrastructure
- Forge card data checked into CI for coverage gating

### Claude's Discretion
- Exact plan count and grouping within plans (informed by dependency analysis)
- Choice UI pattern (generic modal vs per-mechanic) for Dig/Explore/Surveil
- Which Pioneer mechanics fall out naturally vs need explicit work
- AI evaluation depth for each choice type
- Exact ordering within each plan's batch

</decisions>

<specifics>
## Specific Ideas

- "Feature coverage is highly important — implement as much as we reasonably can" (carried from Phase 18)
- "Forge implements all these abilities so we should as well" — no artificial scope reduction for complex subsystems
- Mana abilities are first priority because they affect nearly every card with "T: Add {M}"
- Equipment/Aura attachment is a severe gap — cards with these subtypes are playable but attachment mechanics completely absent
- Hover-to-peek for DFC back face matches MTGA behavior

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `crates/engine/src/game/test_helpers.rs`: Forge DB loading helpers (forge_db, load_card, spawn_creature) — extend for new test scenarios
- `crates/engine/src/game/coverage.rs`: Coverage analysis with has_unimplemented_mechanics() — use to validate progress
- `crates/engine/src/bin/coverage-report.rs`: CLI binary for coverage reporting — extend for CI gating
- `crates/engine/src/game/effects/mod.rs`: Effect handler registry pattern (23 handlers) — add new handlers following same pattern
- `crates/engine/src/game/static_abilities.rs`: Static ability registry with handle_stub pattern — promote stubs to real handlers
- `crates/engine/src/game/triggers.rs`: Trigger registry with match_unimplemented pattern — promote to real matchers
- `crates/engine/src/game/replacement.rs`: Replacement effect registry with stub() pattern — promote to real handlers
- `client/src/components/card/CardImage.tsx`: Existing amber warning badge for unimplemented mechanics — will shrink as coverage increases
- `crates/forge-server/` + `crates/server-core/`: Existing hidden information handling for hands — extend for face-down cards (Morph)

### Established Patterns
- Pure reducer: `apply(state, action) -> ActionResult { events, waiting_for }` — all new mechanics follow this
- fn pointer registries built per apply() call — HashMap<String, Handler>
- WaitingFor variants for player choices (ManaPayment, BlockerAssignment exist as reference)
- Discriminated unions with serde tag/content — new events/actions extend existing enums
- Inline #[cfg(test)] modules for unit tests
- matches_filter() shared helper in effects/mod.rs for "All" effect variants

### Integration Points
- `crates/engine/src/game/engine.rs`: Main apply() reducer — new actions and WaitingFor handling
- `crates/engine/src/game/combat.rs`: Blocker validation — already has keyword checks, extend with new statics
- `crates/engine/src/game/casting.rs`: Spell casting — add alternative costs, flashback, adventure casting
- `crates/engine/src/game/mana_payment.rs`: Mana system — needs mana ability integration
- `crates/engine/src/game/sba.rs`: State-based actions — add missing 704.5 rules
- `crates/engine/src/game/layers.rs`: Layer system (MTG Rule 613) — static abilities that modify P/T or grant abilities
- `crates/engine-wasm/src/lib.rs`: WASM bridge — expose new WaitingFor types to frontend
- `client/src/adapter/types.ts`: TypeScript types — add new WaitingFor discriminated unions
- `client/src/components/`: New UI components for Scry choice, card selection, DFC preview

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Context gathered: 2026-03-09*
