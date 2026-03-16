# Phase 30: Implement the Required Building Blocks — Context

**Gathered:** 2026-03-16
**Status:** Ready for planning
**Source:** PRD Express Path (.planning/phases/30-implement-the-required-building-blocks-specified-in-the-summary/need.md)

<domain>
## Phase Boundary

This phase delivers four composable engine building blocks identified from the Bonecrusher Giant gap analysis. Each building block is a general-purpose extension that unlocks an entire class of MTG cards, not just Bonecrusher Giant itself. The phase does NOT include wiring these blocks into a working Bonecrusher Giant card — that's integration work for a subsequent phase.

Building blocks:
1. **Event-context target resolution** — TargetFilter variant + trigger resolution plumbing for "that spell's controller" and similar event-derived targets
2. **Parser: "that spell's controller"** — Oracle text pattern in oracle_target.rs
3. **Adventure casting subsystem** — CR 715: cast Adventure half, exile on resolution, cast creature from exile
4. **"Damage can't be prevented this turn"** — Wire existing parser detection to a functional effect + replacement pipeline check

</domain>

<decisions>
## Implementation Decisions

### Event-Context Target Resolution (Building Block #1)
- New `TargetFilter` variant (e.g., `TriggeringSpellController` or more general `EventContext(EventRef)`) that resolves targets from the triggering event rather than static game state
- Trigger resolution plumbing must thread event context through to target evaluation
- CR 603.7c: triggered abilities can use information about the event that caused them to trigger
- Must work for BecomesTarget events (source_id → controller lookup)

### Parser: "That Spell's Controller" (Building Block #2)
- New pattern in `oracle_target.rs` to parse "that spell's controller" into the event-context TargetFilter variant
- Pattern should be general enough to handle "that spell's owner" and similar possessive event references

### Adventure Casting Subsystem (Building Block #3)
- CR 715.3a: New `SpellCastingOption` variant for casting the Adventure half from hand
- CR 715.4: Adventure spell resolves → exile (not graveyard), gains "castable as creature" permission
- CR 715.5: Exiled Adventure card can be cast as creature face from exile (zone-aware casting permission)
- CR 715.4 (non-resolution): Countered/removed Adventure spell → graveyard normally
- CardLayout::Adventure already exists in types/card.rs — use it as the foundation
- Frontend needs: Adventure casting mode UI, visual treatment of Adventure cards, exile-cast flow

### Damage Prevention Disabling (Building Block #4)
- Parser already detects "damage can't be prevented" (oracle_replacement.rs:67-77) but creates hollow ReplacementDefinition
- Need turn-scoped game state flag or continuous effect that the replacement pipeline checks before allowing `ReplacementResult::Prevented`
- Similar pattern to "can't gain life" / "can't be countered" — temporary restriction on a game mechanic
- CR 614.16 analog: effects that prevent the application of other replacement effects

### Claude's Discretion
- Exact naming of new TargetFilter variants (TriggeringSpellController vs EventContext)
- Whether damage prevention disabling uses a game state flag vs continuous effect system
- Internal structuring of Adventure zone-change hooks
- Wave ordering and plan decomposition
- Test strategy and scenario coverage

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Trigger System
- `crates/engine/src/game/triggers.rs` — Trigger matching (BecomesTarget matcher at ~line 1485)
- `crates/engine/src/types/triggers.rs` — TriggerMode, TriggerDefinition types
- `crates/engine/src/types/events.rs` — GameEvent::BecomesTarget { object_id, source_id }

### Target System
- `crates/engine/src/types/ability.rs` — TargetFilter enum definition
- `crates/engine/src/game/targeting.rs` — Target resolution and evaluation
- `crates/engine/src/parser/oracle_target.rs` — Oracle text target parsing

### Casting & Stack
- `crates/engine/src/game/casting.rs` — Spell casting flow, BecomesTarget event emission
- `crates/engine/src/game/stack.rs` — Stack resolution (Adventure exile-on-resolve hooks here)

### Card Types & Layout
- `crates/engine/src/types/card.rs` — CardLayout::Adventure definition
- `crates/engine/src/database/oracle_loader.rs` — Adventure card loading

### Replacement & Damage Prevention
- `crates/engine/src/game/replacement.rs` — Replacement pipeline, ReplacementResult::Prevented
- `crates/engine/src/parser/oracle_replacement.rs` — "damage can't be prevented" detection (lines 67-77)
- `crates/engine/src/game/effects/deal_damage.rs` — Damage dealing effect
- `crates/engine/src/game/keywords.rs` — Keyword-related damage prevention

### Parser Architecture
- `docs/parser-instructions.md` — Oracle parser contribution guide
- `crates/engine/src/parser/oracle.rs` — Main parser orchestration

### Engine Core
- `crates/engine/src/game/engine.rs` — apply() reducer pattern
- `crates/engine/src/types/game_state.rs` — GameState (where turn-scoped flags would live)

</canonical_refs>

<specifics>
## Specific Ideas

- Bonecrusher Giant creature face trigger: `TriggerMode::BecomesTarget` already works; the gap is target resolution for "that spell's controller"
- Adventure cards: `CardLayout::Adventure(CardFace, CardFace)` already parsed from MTGJSON; WASM bridge and card loading handle both faces
- Prevention disabling: `oracle_replacement.rs` lines 67-77 already detect the phrase but produce a no-op effect
- All four building blocks are independent of each other and can be developed in parallel waves

</specifics>

<deferred>
## Deferred Ideas

- Bonecrusher Giant card integration (wiring all four blocks together) — subsequent phase
- Frontend visual treatment of Adventure cards — can be a follow-up polish phase
- Full Adventure card suite testing (Brazen Borrower, Murderous Rider, etc.) — integration phase
- "That spell's owner" and other possessive event references beyond controller — extend when needed

</deferred>

---

*Phase: 30-implement-the-required-building-blocks-specified-in-the-summary*
*Context gathered: 2026-03-16 via PRD Express Path*
