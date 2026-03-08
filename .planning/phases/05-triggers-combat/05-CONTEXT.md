# Phase 5: Triggers & Combat - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Creatures can attack, block, deal damage, and die. Triggered abilities fire automatically in correct APNAP order when game events occur. 50+ keyword abilities are fully implemented as a typed enum with match-based dispatch. This phase covers the event bus, trigger matching/placement, full combat system with all damage interactions, and keyword registry. Replacement effects and static abilities (continuous effects, seven-layer system) are Phase 6.

</domain>

<decisions>
## Implementation Decisions

### Trigger Architecture
- Trigger matching approach: Claude's discretion (hybrid recommended — scan-on-event for correctness, with cache/index invalidated on battlefield changes)
- Stack placement ordering: Claude's discretion (APNAP with auto-ordering by timestamp, player choice for ordering deferred to UI phase)
- Condition evaluation: check conditions BOTH at match time (to avoid irrelevant triggers on stack) AND recheck intervening-if conditions on resolution — matches MTG rules 603.4 and Forge's behavior
- Last-known information: dies triggers and other leave-battlefield triggers use the object's characteristics as it last existed on the battlefield (MTG rule 603.10) — requires pre-move state snapshots
- ETB trigger model for tokens: Claude's discretion (based on how Forge handles token ETB triggers)
- **Cross-cutting principle: NEVER take shortcuts that violate MTG comprehensive rules. Correctness is non-negotiable.**

### Keyword System
- Representation: typed `Keyword` enum with variants for each keyword (Flying, Trample, Haste, etc.)
- Parameterized keywords use enum variants with associated data: `Keyword::Protection(ProtectionTarget::Color(Red))`, `Keyword::Kicker(ManaCost)`, `Keyword::Cycling(ManaCost)`
- Dispatch: match-based — combat.rs handles combat keywords, targeting.rs handles hexproof/shroud, etc. No trait-based dispatch
- Parsing: `FromStr` on `Keyword` enum — `Keyword::from_str("Flying")` -> `Keyword::Flying`, colon-delimited for params. Unrecognized keywords become `Keyword::Unknown(String)` for forward compatibility
- Scope: ALL 50+ keyword abilities fully implemented — no stubs
- Keywords currently `Vec<String>` on GameObject — will migrate to `Vec<Keyword>`

### Combat Damage Flow
- Multiple blockers: controller orders blockers per MTG rules 509.2, requires `WaitingFor::OrderBlockers` step. Damage assigned in declared order — must assign lethal to first before any to second
- First strike / double strike: two separate damage sub-steps. If any creature has first/double strike, first-strike damage step runs first, SBAs checked, then regular damage step where normal + double strike creatures deal damage. Matches MTG rules 510
- Trample: auto-assign lethal damage to each blocker (in order), excess automatically assigned to defending player. With deathtouch, 1 damage counts as lethal per blocker
- Combat state: dedicated `CombatState` struct on `GameState` with attackers, blocker assignments, damage assignments, etc. Created at BeginCombat, cleared at EndCombat

### Trigger Scope & Coverage
- ALL 137 trigger mode handlers implemented — full coverage, no stubs (consistent with all 202 effects and all 50+ keywords)
- Typed `TriggerMode` enum matching Forge's TriggerType, with `fn matches(mode, event) -> bool` for event-to-trigger mapping
- Parsing: `FromStr` with `TriggerMode::Unknown(String)` fallback for unrecognized modes (matches Keyword pattern)

### Claude's Discretion
- Trigger registry design details (hybrid scan-on-event with cache — Claude determines cache invalidation strategy)
- APNAP auto-ordering implementation (timestamp-based or other heuristic)
- Token ETB trigger model (zone-change driven vs separate event)
- Internal module organization for triggers, combat, and keywords
- Combat damage assignment data structures
- How `CombatState` integrates with existing `GameState` lifecycle

</decisions>

<specifics>
## Specific Ideas

- "Always look at how Forge does it, then compare against highly idiomatic Rust principles" — consistent mandate across all phases
- "We should NEVER pick something that goes against MTG rules" — correctness is the absolute priority, even if it means more complexity
- Full coverage decisions (all 137 triggers, all 50+ keywords) are consistent with Phase 4's all 202 effects — the project builds complete subsystems, not stubs
- `DeclareAttackers` and `DeclareBlockers` actions already exist in `GameAction` enum — wire them up rather than creating new variants
- Combat phases already defined in `Phase` enum but currently auto-skipped — Phase 5 makes them interactive when creatures are present

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `GameEvent` enum (types/events.rs): 20+ event variants already defined (DamageDealt, ZoneChanged, CreatureDestroyed, SpellCast, etc.) — trigger matching can key off these
- `TriggerDefinition { mode: String, params: HashMap }` (types/ability.rs): parsed from card T: lines, ready for trigger matching
- `GameAction::DeclareAttackers` and `GameAction::DeclareBlockers` (types/actions.rs): already defined with correct shapes
- `Phase` enum with all combat phases: BeginCombat, DeclareAttackers, DeclareBlockers, CombatDamage, EndCombat (types/phase.rs)
- `GameObject` with combat fields: `damage_marked`, `dealt_deathtouch_damage`, `entered_battlefield_turn`, `keywords: Vec<String>` (game/game_object.rs)
- Effect handler registry pattern: `HashMap<String, fn pointer>` (game/effects/mod.rs) — can be adapted for trigger mode handlers
- `ResolvedAbility` struct (types/ability.rs): trigger resolution can reuse this for placing triggered abilities on the stack
- SBA fixpoint loop (game/sba.rs): already runs after actions, needs extension for post-combat cleanup

### Established Patterns
- Action dispatch via `(waiting_for, action)` tuple match in engine.rs — combat actions extend this
- Events returned from `apply()` — trigger system consumes these events
- Auto-advance through phases needing no player input (game/turns.rs) — combat phases skip when no creatures can attack
- `FromStr` pattern used on `ManaCostShard`, `Supertype`, `CoreType` enums — consistent with Keyword/TriggerMode parsing
- Effect handlers as fn pointers in HashMap — reusable pattern for trigger mode handlers

### Integration Points
- `engine.rs` apply() needs combat action handling (DeclareAttackers, DeclareBlockers, OrderBlockers)
- `turns.rs` auto-advance needs to stop at combat phases when attackers are possible
- `WaitingFor` enum needs new variants: OrderBlockers, TriggerOrder (maybe)
- `GameState` needs optional `CombatState` field
- `GameObject.keywords` migrates from `Vec<String>` to `Vec<Keyword>`
- Post-effect event processing needs trigger matching → stack placement pipeline
- `sba.rs` needs post-combat cleanup (damage assignment, creature death from combat)

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 05-triggers-combat*
*Context gathered: 2026-03-07*
