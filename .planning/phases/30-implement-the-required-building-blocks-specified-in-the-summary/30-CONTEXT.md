# Phase 30: Implement the Required Building Blocks — Context

**Gathered:** 2026-03-16
**Status:** Ready for planning
**Source:** PRD + discuss-phase

<domain>
## Phase Boundary

This phase delivers four composable engine building blocks identified from the Bonecrusher Giant gap analysis. Each building block is a general-purpose extension that unlocks an entire class of MTG cards, not just Bonecrusher Giant itself. The phase does NOT include wiring these blocks into a working Bonecrusher Giant card — that's integration work for a subsequent phase.

Building blocks:
1. **Event-context target resolution** — TargetFilter variants + trigger resolution plumbing
2. **Parser: event-context possessive references** — "that spell's controller", "that spell's owner", "that player"
3. **Adventure casting subsystem** — CR 715: cast Adventure half, exile on resolution, cast creature from exile
4. **"Damage can't be prevented this turn"** — GameRestriction system + replacement pipeline gate

</domain>

<decisions>
## Implementation Decisions

### Event-Context Target Resolution (Building Block #1)
- **Specific flat TargetFilter variants** (not a nested EventContext(EventRef)): `TriggeringSpellController`, `TriggeringSource`, `TriggeringPlayer` — matches codebase convention of flat enum arms (like `AttachedTo`, `LastCreated`)
- **Trigger event data threading:** Add `trigger_event: Option<GameEvent>` field to the triggered ability's stack entry. When the effect resolves, the resolver reads it to evaluate event-context target filters
- CR 603.7c: triggered abilities can use information about the event that caused them to trigger
- Must work for BecomesTarget events (source_id → controller lookup)

### Parser: Event-Context Possessive References (Building Block #2)
- New patterns in `oracle_target.rs` for all three possessive event references:
  - "that spell's controller" → `TriggeringSpellController`
  - "that spell's owner" → new `TriggeringSpellOwner` variant (trivial addition alongside controller)
  - "that player" → `TriggeringPlayer`
- Patterns should use existing possessive parsing helpers where possible

### Adventure Casting Subsystem (Building Block #3)
- **Casting:** New `CastAdventure` variant in `SpellCastingOptionKind`. Player chooses creature face vs Adventure spell when casting from hand
- **Exile on resolve:** Model as a **replacement effect** — "If an Adventure spell would go to graveyard as it resolves, exile it instead." Fits naturally into the replacement pipeline and correctly interacts with other replacement effects (e.g., Rest in Peace)
- **Cast from exile:** New `CastingPermission` enum (not a bare bool) on `GameObject` — `Vec<CastingPermission>` with an `AdventureCreature` variant. Generalizes to Foretell, Impulse draw, etc. The casting system checks `casting_permissions` to allow casting from exile
- **Non-resolution:** CR 715.4 — if Adventure spell is countered or leaves stack without resolving, goes to graveyard normally (the replacement effect only applies on resolution)
- **Frontend:** Include basic frontend in this phase — casting choice modal (which face?), Adventure card rendering, exile-cast button. Enough to play Adventure cards
- **AI:** Basic AI support — AI can cast Adventure spells, picks the better face based on board state. No deep Adventure-specific evaluation heuristics
- **WASM bridge:** Standard serde + tsify for all new types (CastingPermission, etc.)
- CardLayout::Adventure already exists in types/card.rs — use as foundation

### Damage Prevention Disabling (Building Block #4)
- **GameRestriction enum on GameState** — `Vec<GameRestriction>` with a `DamagePreventionDisabled` variant carrying source, expiry, and optional scope
- **Scope:** Global with optional `RestrictionScope` — Stomp sets scope to None (global). Scoped variants (SourcesControlledBy, SpecificSource, DamageToTarget) for future cards
- **Expiry:** `RestrictionExpiry` enum with `EndOfTurn`, `EndOfCombat`, etc. Cleanup happens at CR 514.2 (cleanup step), alongside other "until end of turn" effects
- **Replacement pipeline integration:** The pipeline checks `GameState.restrictions` before applying prevention replacements. If `DamagePreventionDisabled` matches, prevention effects are skipped
- **Parser wiring:** Connect existing detection in `oracle_replacement.rs:67-77` to produce a functional effect that adds the GameRestriction
- CR 614.16: effects that prevent the application of other prevention effects

### Wave Ordering
- **Wave 1 (parallel):** Event-context TargetFilter variants + parser possessive patterns + GameRestriction enum + cleanup
- **Wave 2:** Wire prevention disable into replacement pipeline + wire parser detection to effect
- **Wave 3:** Adventure subsystem (engine + frontend + AI)

### Testing
- **Per-block scenario tests:** ~3-5 GameScenario tests per building block (trigger fires → event-context target resolves, prevention disable blocks prevention, Adventure casting flow)
- **Integration test:** One comprehensive test that plays Bonecrusher Giant end-to-end, exercising all blocks together

### Claude's Discretion
- Exact function signatures and module placement for new resolvers
- Internal structuring of Adventure zone-change hooks
- Effect handler registration details
- Frontend component design and layout for Adventure cards
- AI evaluation heuristics for face selection

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Trigger System
- `crates/engine/src/game/triggers.rs` — Trigger matching (BecomesTarget matcher at ~line 1485), trigger-to-stack plumbing
- `crates/engine/src/types/triggers.rs` — TriggerMode, TriggerDefinition types
- `crates/engine/src/types/events.rs` — GameEvent::BecomesTarget { object_id, source_id }

### Target System
- `crates/engine/src/types/ability.rs` — TargetFilter enum (line ~625), SpellCastingOption/SpellCastingOptionKind (line ~853)
- `crates/engine/src/game/targeting.rs` — Target resolution and evaluation
- `crates/engine/src/parser/oracle_target.rs` — Oracle text target parsing

### Casting & Stack
- `crates/engine/src/game/casting.rs` — Spell casting flow, BecomesTarget event emission
- `crates/engine/src/game/stack.rs` — Stack resolution (Adventure exile-on-resolve hooks here)
- `crates/engine/src/game/restrictions.rs` — Existing casting restriction checks

### Card Types & Layout
- `crates/engine/src/types/card.rs` — CardLayout::Adventure definition (line ~63)
- `crates/engine/src/database/oracle_loader.rs` — Adventure card loading
- `crates/engine/src/game/game_object.rs` — GameObject struct (casting_options field)

### Replacement & Damage Prevention
- `crates/engine/src/game/replacement.rs` — Replacement pipeline, ReplacementResult::Prevented, apply_replacements()
- `crates/engine/src/parser/oracle_replacement.rs` — "damage can't be prevented" detection (lines 67-77)
- `crates/engine/src/game/effects/deal_damage.rs` — Damage dealing effect

### Engine Core
- `crates/engine/src/game/engine.rs` — apply() reducer pattern, ReplacementResult handling
- `crates/engine/src/types/game_state.rs` — GameState (where GameRestriction vec and cleanup logic live)
- `crates/engine/src/game/turns.rs` — Turn structure, cleanup step (CR 514.2)

### Parser Architecture
- `docs/parser-instructions.md` — Oracle parser contribution guide
- `crates/engine/src/parser/oracle.rs` — Main parser orchestration

### Skills
- `.claude/skills/add-engine-effect/SKILL.md` — Authoritative checklist for adding new engine effects

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `TargetFilter::AttachedTo` and `TargetFilter::LastCreated` — precedent for context-derived targeting (resolve from source object, not board scan)
- `TargetFilter::TrackedSet` — precedent for set-based context targeting (CR 603.7)
- `SpellCastingOption` builder pattern — `alternative_cost()`, `as_though_had_flash()`, `free_cast()` convenience methods
- `GameScenario` test harness — existing infrastructure for rules correctness tests

### Established Patterns
- `ReplacementResult` pipeline — Execute/Prevented/NeedsChoice tri-state in engine.rs and turns.rs
- `#[serde(tag = "type")]` discriminated unions — all new enums must follow this pattern
- Cleanup step in turns.rs — where EndOfTurn expiry cleanup should be added
- Stack entry structure — where `trigger_event: Option<GameEvent>` gets added

### Integration Points
- `game/engine.rs:328-377` — Where ReplacementResult is consumed (add GameRestriction check before prevention)
- `game/casting.rs` — Where CastAdventure option is presented and Adventure casting flow begins
- `game/stack.rs` — Where Adventure exile-on-resolve replacement hooks in
- `game/game_object.rs:40` — Where `casting_permissions: Vec<CastingPermission>` gets added
- `parser/oracle_target.rs` — Where possessive event reference patterns get added
- `crates/engine-wasm/` — Where new types get tsify derives for TypeScript generation

</code_context>

<specifics>
## Specific Ideas

- Bonecrusher Giant creature face trigger: `TriggerMode::BecomesTarget` already works; the gap is target resolution for "that spell's controller"
- Adventure cards: `CardLayout::Adventure(CardFace, CardFace)` already parsed from MTGJSON; WASM bridge and card loading handle both faces
- Prevention disabling: `oracle_replacement.rs` lines 67-77 already detect the phrase but produce a no-op effect
- GameRestriction is the building block, not just damage prevention — same pattern covers "can't gain life" (Erebos), "can't be countered" (Teferi), "creatures can't block" (Bedlam)
- CastingPermission is the building block, not just Adventure — same pattern covers Foretell, Impulse draw, etc.

</specifics>

<deferred>
## Deferred Ideas

- Bonecrusher Giant card integration (wiring all four blocks together) — subsequent phase
- Full Adventure card suite testing (Brazen Borrower, Murderous Rider, etc.) — integration phase
- Additional GameRestriction variants (LifeGainDisabled, CantBeCountered) — add when cards need them
- Additional CastingPermission variants (Foretell, ImpulseDraw) — add when cards need them
- MDFC (Modal Double-Faced Cards) face-casting — separate mechanic, uses different rules than Adventure

</deferred>

---

*Phase: 30-implement-the-required-building-blocks-specified-in-the-summary*
*Context gathered: 2026-03-16 via PRD + discuss-phase*
