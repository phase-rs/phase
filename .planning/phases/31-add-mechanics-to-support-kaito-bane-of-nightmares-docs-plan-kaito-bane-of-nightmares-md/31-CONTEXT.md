# Phase 31: Kaito, Bane of Nightmares Mechanics — Context

**Gathered:** 2026-03-16
**Status:** Ready for planning

<domain>
## Phase Boundary

Deliver five engine building blocks motivated by Kaito, Bane of Nightmares but each covering a broad class of MTG cards: Ninjutsu runtime (~30 cards), Emblem infrastructure (dozens of planeswalkers), compound static conditions, planeswalker-to-creature conditional animation, and a scalable "for each" dynamic quantity system. Full parser coverage for all new patterns. Kaito should be fully playable when complete.

</domain>

<decisions>
## Implementation Decisions

### Ninjutsu Runtime (Building Block #1)
- **Activation via WaitingFor:** During declare blockers priority window, if player has a Ninjutsu card in hand + an unblocked attacker, add a NinjutsuActivation option to WaitingFor. Fits existing WaitingFor pattern
- **No attack triggers (CR 702.49c):** Ninjutsu creature is put onto battlefield attacking — never declared as attacker, so "whenever ~ attacks" triggers do NOT fire
- **Compound cost:** New `AbilityCost::Ninjutsu` variant with mana_cost + return_unblocked_attacker as typed compound cost. Engine validates both parts atomically
- **Handler location:** `game/keywords.rs` — activate_ninjutsu() function, consistent with Equip/Crew keyword activation pattern
- **Basic AI included:** AI recognizes ninjutsu as legal action during declare blockers, picks highest-value creature to ninjutsu, returns smallest unblocked attacker

### Emblem Infrastructure (Building Block #2)
- **Full GameObject model:** Emblems are GameObjects in Zone::Command with `is_emblem: bool` flag (like is_commander). Participate in layer system naturally via standard static abilities
- **Effect::CreateEmblem:** New Effect variant carrying the emblem's abilities as data. Loyalty ability resolution creates new GameObject in Zone::Command with is_emblem=true and specified statics/triggers
- **Standard statics through layers:** Kaito's emblem has StaticDefinition with ContinuousModification (AddPT +1/+1) filtered by TargetFilter::And(Subtype Ninja, YouControl). Layer system evaluates in layer 7c exactly like any other P/T modification — no special emblem path
- **Engine-level immunity (CR 114.4):** change_zone, destroy, exile, bounce handlers skip objects with is_emblem=true. Emblems cannot be destroyed, exiled, or otherwise removed
- **Command zone row UI:** Render emblems as mini-cards in the command zone strip alongside commanders. Visible, not hidden behind tooltips

### Compound Static Conditions (Building Block #3)
- **StaticCondition::And(Vec<StaticCondition>) combinator:** Evaluates all sub-conditions. Kaito becomes And([DuringYourTurn, HasCounters { counter_type: Loyalty, minimum: 1 }])
- **StaticCondition::Or(Vec<StaticCondition>) combinator:** Added alongside And for completeness. Future cards need both
- **StaticCondition::HasCounters { counter_type, minimum }:** Self-referential — always checks the object the static is on. No explicit target needed (MTG "as long as ~ has counters" always means self)
- **Normal summoning sickness (CR 302.6):** Kaito as creature follows standard summoning sickness rules. No special case — existing logic handles it correctly

### Planeswalker-to-Creature Animation (Building Block #4)
- **ContinuousModification list on StaticDefinition:** Vec<ContinuousModification> with SetPT(3,4), AddType(Creature), AddSubtype(Ninja), AddKeyword(Hexproof). Layer system applies each in correct layer (4 for type, 7b for P/T, 6 for keyword). Reuses Phase 28 infrastructure
- **Full parser coverage:** Parser handles "during your turn, as long as ~ has one or more [counter type] counters on [pronoun], [pronoun]'s a [P/T] [types] and has [keyword]" — emits compound StaticCondition + ContinuousModification list directly from Oracle text

### Dynamic Quantity System — "For Each" (Building Block #5)
- **QuantityExpr on ALL count-based effects:** Draw, DealDamage, GainLife, Mill, LoseLife all accept QuantityExpr instead of fixed i32. QuantityExpr::Fixed { value: N } for literal counts, QuantityExpr::Ref { qty: QuantityRef } for dynamic
- **Three new QuantityRef variants:**
  - `ObjectCount { filter: TargetFilter }` — counts matching objects on battlefield ("for each creature you control")
  - `PlayerCount { filter: PlayerFilter }` — counts matching players ("for each opponent who lost life this turn")
  - `CountersOnSelf { counter_type: CounterType }` — counts counters on source object ("for each counter on ~")
- **PlayerFilter enum:** `Opponent`, `OpponentLostLife`, `OpponentGainedLife`, `All` — extensible for future patterns
- **Scalable parse_for_each() parser:** Detects "for each [noun phrase]" in Oracle text, routes to parse_target_filter for object counts (reusing existing filter parsing), parse_player_filter for player counts. Covers dozens of cards with one composable parser pass
- **No migration needed:** All card data is generated from Oracle text by the parser pipeline. Updating types + parser is sufficient

### Testing
- Per-block scenario tests: ~3-5 GameScenario tests per building block
- Kaito integration test: full card played end-to-end — cast via Ninjutsu, create emblem, animate as creature, Surveil + variable draw
- Parser snapshot tests for new Oracle text patterns

### Claude's Discretion
- Exact function signatures and module placement for new resolvers
- Internal structuring of Ninjutsu combat interaction hooks
- Emblem GameObject field defaults (no P/T, no mana cost, etc.)
- PlayerFilter variant set beyond the initial four
- parse_for_each() routing heuristics for ambiguous noun phrases
- Frontend emblem mini-card visual design

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Card-Specific
- `docs/plan-kaito-bane-of-nightmares.md` — Full gap analysis for Kaito: 5 gaps with complexity ratings and engine area references

### Keyword System
- `crates/engine/src/types/keywords.rs` — Keyword::Ninjutsu(ManaCost) definition (line ~131), keyword parsing
- `crates/engine/src/game/keywords.rs` — Keyword runtime handlers (Equip pattern to follow)
- `crates/engine/src/parser/oracle.rs` — Ninjutsu keyword parsing (line ~598)

### Static Abilities & Conditions
- `crates/engine/src/types/ability.rs` — StaticCondition enum (line ~783), QuantityRef (line ~720), QuantityExpr (line ~736), ContinuousModification, StaticDefinition
- `crates/engine/src/game/static_abilities.rs` — Static ability evaluation, condition checking
- `crates/engine/src/game/layers.rs` — Layer system: type changes (L4), keyword grants (L6), P/T modifications (L7b/7c)

### Effects & Animation
- `crates/engine/src/game/effects/animate.rs` — Existing Animate effect handler (P/T + type setting)
- `crates/engine/src/game/effects/mod.rs` — Effect handler registry

### Combat System
- `crates/engine/src/game/combat.rs` — Declare blockers step, unblocked attacker detection
- `crates/engine/src/game/combat_damage.rs` — Entering-attacking logic

### Zones & Game Objects
- `crates/engine/src/types/zones.rs` — Zone::Command (line ~12)
- `crates/engine/src/game/game_object.rs` — GameObject struct, is_commander flag pattern
- `crates/engine/src/game/effects/change_zone.rs` — Zone change handlers (emblem immunity here)

### Life Tracking
- `crates/engine/src/types/player.rs` — life_lost_this_turn (line ~42), life_gained_this_turn (line ~40)
- `crates/engine/src/game/effects/life.rs` — Life change tracking

### Parser Architecture
- `docs/parser-instructions.md` — Oracle parser contribution guide
- `crates/engine/src/parser/oracle_target.rs` — Target parsing (reuse for ObjectCount filter parsing)

### Skills
- `.claude/skills/add-engine-effect/SKILL.md` — Authoritative checklist for adding new engine effects
- `.claude/skills/add-keyword/SKILL.md` — Keyword ability implementation guide
- `.claude/skills/add-static-ability/SKILL.md` — Static ability and layer system guide

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `Keyword::Ninjutsu(ManaCost)` — already parsed from Oracle text, cost extracted
- `Zone::Command` — exists for commanders, reusable for emblems
- `is_commander: bool` on GameObject — pattern for `is_emblem: bool`
- `StaticCondition::DuringYourTurn` — already exists, one half of Kaito's compound condition
- `QuantityRef`/`QuantityExpr` — existing pattern for dynamic quantities, extend with new variants
- `ContinuousModification` — Phase 28 infrastructure for layer-based modifications
- `TargetFilter` combinators (And/Or/Not) — reuse for ObjectCount filter
- `life_lost_this_turn: u32` / `life_gained_this_turn: u32` — already tracked per Player, reset each turn
- `GameScenario` test harness — existing infrastructure for rules correctness tests
- `SpellCastingOption` / `WaitingFor` — existing patterns for presenting player choices

### Established Patterns
- Keyword activation in `game/keywords.rs` (Equip, Crew) — follow for Ninjutsu
- Effect handler registry in `effects/mod.rs` — follow for CreateEmblem
- `#[serde(tag = "type")]` discriminated unions — all new enums must follow
- Phase 30 building blocks (GameRestriction, CastingPermission) — same "composable enum" approach

### Integration Points
- `game/combat.rs` — Where Ninjutsu activation check happens (during declare blockers)
- `game/engine.rs` — Where NinjutsuActivation GameAction is dispatched
- `game/effects/mod.rs` — Where CreateEmblem handler is registered
- `game/static_abilities.rs` — Where And/Or condition evaluation is added
- `game/layers.rs` — Where emblem statics enter layer evaluation (already iterates Zone::Command objects if using full GameObject)
- `parser/oracle_effect.rs` — Where "for each" quantity parsing is added
- `crates/engine-wasm/` — Where new types get tsify derives for TypeScript generation

</code_context>

<specifics>
## Specific Ideas

- Ninjutsu is a high-value building block covering ~30 cards across Magic's history
- Emblem infrastructure unlocks dozens of planeswalker cards
- The "for each" parser should be a general-purpose composable helper, not card-specific
- PlayerFilter should start minimal (4 variants) and grow as cards need them
- Emblem UI as command zone row mini-cards (visible alongside commanders), not hidden tooltips
- Full parser coverage for Kaito's compound static — no hand-authored card data

</specifics>

<deferred>
## Deferred Ideas

- Additional PlayerFilter variants beyond Opponent/OpponentLostLife/OpponentGainedLife/All — add when cards need them
- "For each" patterns involving zones other than battlefield (graveyard, exile) — extend ObjectCount when needed
- Gideon-style activated ability animation (different trigger than Kaito's static) — separate card support
- Emblem triggered abilities (some planeswalker emblems have triggers, not just statics) — extend when cards need them

</deferred>

---

*Phase: 31-add-mechanics-to-support-kaito-bane-of-nightmares-docs-plan-kaito-bane-of-nightmares-md*
*Context gathered: 2026-03-16*
