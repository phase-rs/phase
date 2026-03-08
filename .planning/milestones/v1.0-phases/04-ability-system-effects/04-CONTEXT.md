# Phase 4: Ability System & Effects - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Cards can be cast with costs paid, targets chosen, and effects resolved. A player can cast Lightning Bolt targeting a creature, Counterspell targeting a spell, and Giant Growth on their own creature. Spells and activated abilities both work through the same effect handler system. This phase bridges parsed ability data (AbilityDefinition with api_type + params) to actual game state mutations. Triggers are Phase 5; replacement effects and static abilities are Phase 6.

</domain>

<decisions>
## Implementation Decisions

### Effect Resolution Model
- Follow Forge's implementation as reference, adapted to idiomatic Rust with clean architecture
- All 15 top effect types implemented as full handlers: Draw, DealDamage, ChangeZone, Pump, Destroy, Counter, Token, GainLife, LoseLife, Tap, Untap, AddCounter, RemoveCounter, Sacrifice, DiscardCard
- Sub-ability chaining uses a linear chain model (matching Forge's SpellAbility.getSubAbility() pattern) — each link can independently check conditions, no tree/DAG needed
- Per-link condition checking enables conditional behavior within the linear chain

### Casting Flow & Cost Payment
- Multi-step WaitingFor flow: CastSpell action starts the process, engine returns WaitingFor::TargetSelection or WaitingFor::CostPayment as needed, each step is a separate action/response cycle
- Auto-pay and auto-target: when there's exactly one legal target and mana payment is unambiguous, skip WaitingFor steps and resolve immediately
- Full MTG timing rules: sorcery-speed spells only during your main phase with empty stack; instants and flash cards at any priority point
- Both spell casting (from hand) and activated abilities (ActivateAbility action) supported in Phase 4

### Target System Design
- Unified target enum: TargetRef::Object(ObjectId) | TargetRef::Player(PlayerId) — enables targeting both creatures and players
- Full MTG fizzle rules (rule 608.2b): if ALL targets are illegal on resolution, spell fizzles; if some targets are still legal, spell resolves with only legal ones
- Basic hexproof and shroud checking during target validation (can't be targeted by opponents / can't be targeted by anyone); protection deferred to Phase 5 with keywords

### SVar & Sub-ability Chaining
- Claude's Discretion: SVar resolution approach (lazy lookup vs pre-linking) — Claude picks based on Forge's SpellAbility.getSVar() pattern and correctness
- Claude's Discretion: Condition system scope — Claude determines which condition types to implement based on ABIL-05 requirements and what's testable without Phase 5+ features
- Claude's Discretion: Execute$ branching model — Claude implements based on Forge's actual behavior and what the card corpus uses
- Claude's Discretion: Effect parameter mapping approach (resolve-time HashMap extraction vs typed param structs) — Claude picks based on Forge's getParam() pattern and Rust idioms
- Claude's Discretion: Target filter specification approach (string-based matching vs typed filter structs) — based on Forge's TargetChoices/CardProperty pattern and extensibility needs

### Claude's Discretion
- Internal module organization for the ability/effect system
- Cost parser architecture (how non-mana costs like tap, sacrifice, discard, life integrate with existing mana payment)
- StackEntry expansion (how resolved ability data attaches to stack entries for effect execution)
- Token creation implementation details
- Counter spell mechanics (how Counterspell removes a spell from the stack)

</decisions>

<specifics>
## Specific Ideas

- "Always check Forge's implementation and idiomatic Rust that follows clean architecture" — consistent with all prior phases
- Success criteria explicitly names Lightning Bolt, Counterspell, and Giant Growth as test cases — these three cards exercise targeting (creature/spell/own creature), effect types (DealDamage, Counter, Pump), and timing (instant-speed)
- Auto-pay + auto-target improves gameplay flow and testing ergonomics — reduces boilerplate action sequences in tests
- Activated abilities share the effect handler system with spells — no separate execution path needed

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `AbilityDefinition` with `api_type: String` + `params: HashMap<String, String>` (types/ability.rs) — parsed ability data ready for effect dispatch
- `TriggerDefinition`, `StaticDefinition`, `ReplacementDefinition` (types/ability.rs) — Phase 5/6 will use these
- `StackEntry` with `StackEntryKind::Spell { card_id }` (types/game_state.rs) — needs expansion for effect data and targets
- `resolve_top()` in game/stack.rs — currently only moves to zones, needs to execute effects
- `GameAction::CastSpell { card_id, targets }` and `GameAction::ActivateAbility { source_id, ability_index }` — already defined, currently rejected at runtime
- `WaitingFor::ManaPayment { player }` — already exists as a variant
- `mana_payment` module — full mana payment system with auto-pay (greedy colored-first)
- `CardFace` with `svars: HashMap<String, String>` — raw SVar storage from Phase 2

### Established Patterns
- Action dispatch via `(waiting_for, action)` tuple match in engine.rs — new actions extend this match
- `fn apply(&mut GameState, action) -> Result<ActionResult, EngineError>` — all state mutations go through apply
- Events returned from apply for downstream consumers (triggers in Phase 5, UI in Phase 7)
- SBA fixpoint loop runs after every action during priority
- Auto-advance through phases needing no player input

### Integration Points
- `engine.rs` apply() match needs CastSpell and ActivateAbility arms
- `stack.rs` resolve_top() needs effect execution before zone movement
- `StackEntryKind` needs new variant for activated abilities
- `WaitingFor` needs TargetSelection and CostPayment variants
- `GameAction` may need new variants for target selection and cost payment responses
- `GameObject` fields (abilities, keywords) currently Vec<String> — may need richer types for runtime ability resolution

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 04-ability-system-effects*
*Context gathered: 2026-03-07*
