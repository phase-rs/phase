# Phase 6: Advanced Rules - Context

**Gathered:** 2026-03-07
**Updated:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Implement MTG advanced-rules behavior already scoped in Phase 6: replacement effects that intercept events before they happen, seven-layer continuous effect evaluation per Rule 613, and static ability apply/unapply behavior. Covers requirements REPL-01 through REPL-04 and STAT-01 through STAT-04.

</domain>

<decisions>
## Implementation Decisions

### Replacement pipeline architecture
- Central `replace_event()` function called from every mutation site (damage, zone moves, draw, life change, counters, token creation, discard, tap/untap)
- All mutation sites get interception hooks — not a minimal subset
- Follow Forge's `ReplacementHandler` pattern and CR 616 flow
- Prevention effects (CR 615) are a subtype of replacement — same pipeline, same once-per-event tracking, same choice logic

### Replacement choice flow
- When multiple replacements apply, engine sets `WaitingFor::ReplacementChoice` and returns — consistent with target/attacker/blocker choice patterns
- Pending replacement context (event being replaced, candidates, already-applied set) stored on `GameState` for WaitingFor round-trip — matches Forge's approach
- Full re-evaluation of candidate list after each replacement applies (CR 616.1 requires this — a replacement that didn't match the original event might match the modified one)
- Once-per-event enforcement per CR 616.1f
- Each replacement application emits a `ReplacementApplied` GameEvent for tracing/game log

### Replacement nesting and depth
- Nested replacements supported (a replacement effect itself being replaced, e.g., 'if would die, exile instead' then 'if would be exiled, put into graveyard instead')
- Depth cap of 16 (double Forge's 8 for safety margin)

### Replacement handler registry
- HashMap<String, ReplacementHandler> built per call — same pattern as effect/trigger registries (Phases 4-5)
- Replacement handlers can emit side-effect events that feed into the trigger pipeline

### Event identity model
- Separate `ProposedEvent` enum with exhaustive typed variants (ProposedDamage, ProposedZoneChange, ProposedDraw, etc.)
- Each variant carries the mutable fields replacement handlers need to read/modify
- Applied-replacements tracking (HashSet) lives on ProposedEvent itself — self-contained, no GameState cleanup
- After pipeline completes, ProposedEvent converts to actual mutations + GameEvents

### Layer evaluation
- All 7 layers implemented: copy (1), control (2), text (3), type (4), color (5), ability (6), P/T (7a-7e)
- petgraph toposort for intra-layer dependency ordering; cycle fallback to timestamp ordering per CR 613.8
- Global monotonic u64 counter on GameState for timestamp tracking (CR 613.7)
- Base + computed fields on GameObject — base_power/base_toughness (printed values) vs computed power/toughness/keywords/etc produced by layer engine
- Keyword grants from static abilities computed during layer 6 evaluation

### Static/replacement runtime storage
- `static_definitions: Vec<StaticDefinition>` and `replacement_definitions: Vec<ReplacementDefinition>` on GameObject — populated at creation from CardFace data, consistent with trigger_definitions pattern
- Active effects determined by zone-filtered scan (zone == Battlefield, plus special cases like emblems in Command zone) — no separate active set tracking

### Layer conflict policy
- Inside a layer, dependency ordering takes precedence over timestamp ordering
- In tie/uncertainty cases, follow Forge + MTG rules first; if still silent, use deterministic stable fallback ordering
- Source/condition-based effects stop applying when source/condition is no longer true (on next evaluation pass)

### Claude's Discretion
- Layer evaluation timing strategy (on-demand before checks vs immediate on state change)
- EventId approach (monotonic counter vs structural identity) for once-per-event tracking
- 'Instead' replacement handling (event nullified flag vs return enum from pipeline)
- Self-replacement effect routing ('as enters' vs 'if would' — same pipeline or separate pre-hook)
- Deterministic fallback key design when Forge/MTG do not define an ordering tie-break
- Exact `WaitingFor` and `GameAction` variant shapes for replacement-choice prompts
- Exact split of canonical coverage vs additional handlers across plans

</decisions>

<specifics>
## Specific Ideas

- In areas of uncertainty, follow Forge and MTG rules — defer to Forge's approach and weigh highly idiomatic Rust solutions
- Preserve explicit choice points for replacement ordering even before UI phase
- Correctness over shortcuts remains non-negotiable for this phase
- Must-pass scenarios: "would die -> exile instead" replacement conflict with player choice + once-per-event enforcement; layer-7 static buff apply/unapply as source permanents enter/leave

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `parser/ability.rs` already parses `StaticDefinition` and `ReplacementDefinition` from card data
- `types/card.rs`/`parser/card_parser.rs` already persist raw `S:` and `R:` lines on `CardFace`
- `types/game_state.rs` + `game/engine.rs` already use a robust `WaitingFor` action-response state machine
- `game/stack.rs`, `game/triggers.rs`, and `types/events.rs` provide an event-driven resolution backbone
- Existing explicit selection pattern (`WaitingFor::TargetSelection` + `GameAction::SelectTargets`) is a precedent for replacement choice prompts
- `game/effects/*.rs` — individual effect handlers that will need `replace_event()` calls at mutation points
- `game/combat_damage.rs` — damage assignment that needs replacement interception

### Established Patterns
- Engine flow centralized through `apply()` with strict `WaitingFor` gating
- Rule subsystems use registries and pure-ish handler functions (`effects`, `triggers`) rather than hardcoded monoliths — replacement handlers follow this pattern
- SBA + trigger processing after actions is already established as a correctness loop
- `trigger_definitions` stored on `GameObject` at creation time — static/replacement definitions follow same pattern
- Build registry per call (cheap, avoids static patterns) — established in Phases 4-5

### Integration Points
- `types/game_state.rs` and `types/actions.rs` need replacement-choice wait/action variants + pending replacement context field
- `game/engine.rs` needs dispatch handling for replacement-choice continuation
- `game/game_object.rs` needs `static_definitions` and `replacement_definitions` fields + base vs computed characteristic fields
- All mutation sites (`game/zones.rs`, `game/effects/*.rs`, `game/combat_damage.rs`, stack resolution) need `replace_event()` hook
- Layer evaluation feeds characteristic-dependent checks (target legality, SBA, combat interactions)

</code_context>

<deferred>
## Deferred Ideas

- Any UI-facing replacement ordering UX (belongs with Phase 7 platform/UI work)

</deferred>

---

*Phase: 06-advanced-rules*
*Context gathered: 2026-03-07*
