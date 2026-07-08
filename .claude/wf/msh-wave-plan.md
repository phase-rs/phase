# MSH/MSC Remaining Cards — Deconflicted Implementation Wave Plan

Generated: 2026-06-20  
Scope: 77 remaining unsupported Marvel Super Heroes (MSH/MSC) cards after deducting all excluded clusters (Teamwork ×17, PR #3907 ×2, PR #3918 ×3, PR #3909 merged ×2, Power-up ×7).

## Data Summary

| Category | Count |
|----------|-------|
| Explicit parser/engine gap (`supported: false` in any `parse_details`) | 18 |
| All parse_details `supported: true` but card unsupported (misparse/runtime) | 59 |
| **Total to plan** | **77** |

Of the 77 cards:
- 9 have `gap_count=0` (parser sees no gaps; likely misparse producing wrong AST or a runtime-only failure)
- 65 have `gap_count=1`
- 3 have `gap_count=2` or `gap_count=3`

---

## Cross-PR Dependency Flags

| Card | Depends on |
|------|-----------|
| Beast, Erudite Aerialist | PR #3918 — introduces `CountersPutOnThisTurn` filter primitive; must be deferred until after merge |

---

## Part I: Explicit Gap Clusters (18 cards)

These cards have at least one `parse_details` entry with `supported: false`.

---

### Cluster A — Connive Trigger (`TriggerMode::Connive`)  
**4 cards · NEW primitive needed · HIGH ROI**

The trigger phrase "whenever [subject] connives" does not map to any `TriggerMode` variant. `Effect::Connive` (`EffectKind::Connive`) exists and fires from `connive.rs`, but no `GameEvent::EffectResolved { kind: Connive }` is currently emitted and no `TriggerMode::Connive` exists. `EffectKind::Connive` sits in trigger_index's "no production EffectResolved matcher" list (line 773).

**Primitive needed:**
1. Add `TriggerMode::Connive` to `crates/engine/src/types/triggers.rs`
2. Emit `GameEvent::EffectResolved { kind: EffectKind::Connive, … }` from `crates/engine/src/game/effects/connive.rs`
3. Add `TriggerEventKey::EffectResolved(EffectKind::Connive)` keying in `game/trigger_index.rs` (following the pattern used by `Explored`, `Adapt`, `Exploit`, etc.)
4. Parse "whenever [subject] connives" and "whenever one or more creatures [you control] connive" in `parser/oracle_trigger.rs`

**Leader, Super-Genius also needs a replacement primitive**: "If a creature you control would connive, instead you draw a card, then that creature connives." This is a connive interception replacement — `ReplacementEvent::Connive` (new) that re-routes connive resolution. `/add-replacement-effect` skill.

| Card | Gap labels |
|------|-----------|
| Glorious Purpose | `Whenever a creature you control connives` (False) |
| Iron Monger, Sadistic Tycoon | `Whenever a creature you control connives` (False) |
| Ultron, Unlimited | `Whenever a creature you control connives` (False) |
| Leader, Super-Genius | `replacement_structure` (False) + Phase trigger (True) |

**Skill:** `/add-trigger` + (Leader) `/add-replacement-effect`  
**Collision files:** `types/triggers.rs`, `game/effects/connive.rs`, `game/trigger_index.rs`, `parser/oracle_trigger.rs`  
**Cross-PR dependency:** None  
**ROI score:** 4 cards / medium complexity = **HIGH**

---

### Cluster B — Replacement Structure (parser: `replacement_structure`)  
**4 cards · Parser extension — 3 distinct replacement types · MEDIUM ROI**

The oracle_replacement.rs parser emits `Effect::Unimplemented { name: "replacement_structure" }` for these patterns. Each needs a distinct new parser arm; some may need a new `ReplacementMode` variant.

| Card | Replacement pattern | Engine type status |
|------|---------------------|-------------------|
| Divine Visitation | "If one or more creature tokens would be created under your control, [Angels created instead]" | `ReplacementMode::TokenCreation` — check if exists |
| Mjölnir, Hammer of Thor | "Double all damage equipped creature would deal" | `DamageModification::Double` — check if exists |
| Wolverine, Fierce Fighter | "If damage would be dealt to Wolverine, instead that damage is dealt, but all other damage already dealt to him is healed" | `ReplacementMode::HealAllPrevious` — likely new |
| Leader, Super-Genius | "If a creature you control would connive, instead…" | Covered under Cluster A |

**Implementation note:** Mjölnir's damage doubling is a `DamageReplacement` that doubles damage output from the equipped creature. Check if `ReplacementMode::MultiplyDamage` (or `DamageModification`) already covers this. Divine Visitation's token-substitution replacement may reuse `ReplacementMode::Created` if it exists.

**Skill:** `/add-replacement-effect` (up to 3 separate sub-implementations)  
**Collision files:** `parser/oracle_replacement.rs`, `types/replacements.rs`, `types/ability.rs`  
**ROI score:** 3+ cards / medium-high complexity = **MEDIUM**

---

### Cluster C — Static Structure (`static_structure`)  
**2 cards · Parser extension — 2 distinct static patterns · MEDIUM ROI**

| Card | Static pattern | Notes |
|------|---------------|-------|
| Dragon Man, Reformed Robot | "Dragon Man's power is equal to the greatest mana value among noncreature permanents you control and noncreature cards in your graveyard" | CDA QuantityRef: `MaxManaCost { filter: TypedFilter(noncreature), zones: [Battlefield, Graveyard] }` — likely new `QuantityRef` variant |
| Wolverine, Claws Out | "You may have Wolverine assign his combat damage as though he weren't blocked" | Optional unblocked-damage-assignment static; check for `CombatDamageScope::AsThoughUnblocked` or similar |

**Skill:** `/add-static-ability`  
**Collision files:** `parser/oracle_static/`, `parser/oracle.rs`, `types/ability.rs`, `game/quantity.rs` (Dragon Man), `game/combat_damage.rs` (Wolverine)  
**ROI score:** 2 cards / medium complexity = **MEDIUM-LOW**

---

### Cluster D — Play Card from Exile Trigger (parser-only fix)  
**1 card · EXISTING `TriggerMode::PlayCard` · parser extension · HIGH ROI/effort**

`TriggerMode::PlayCard` exists and fires on both land-play and spell-cast events. The `parse_play_card_trigger_subject` function (oracle_trigger.rs:12514) currently rejects any qualifier after "a card" — it requires EOF or comma immediately after. "from exile" fails because the parser hits the zone qualifier and returns `None`.

**Fix:** Extend `parse_play_card_trigger_subject` to parse an optional `from <zone>` tail after "a card" and set `valid_card` zone constraint (mirroring how `parse_land_play_trigger_subject` handles `from <zone>`).

| Card | Gap |
|------|-----|
| Klaw, Master of Sound | `Whenever you play a card from exile` (False) |

**Skill:** parser-only misparse fix  
**Collision files:** `parser/oracle_trigger.rs` only  
**ROI score:** 1 card / very low complexity = **FAST WIN**

---

### Cluster E — Becomes Target of Ability You Control (parser extension)  
**1 card · EXISTING `TriggerMode::BecomesTarget` · parser gap · HIGH ROI/effort**

`TriggerMode::BecomesTarget` and `parse_target_source_controller` (which handles "you control"/"an opponent controls") both exist. The gap is that `parse_simple_event` has no arm for "becomes the target of an ability" (only: "a spell or ability", "a spell", "an Aura spell", "an instant or sorcery spell", "a backup ability"). Loki needs specifically an *ability* source with controller filter and a broad recipient ("a player or permanent").

**Fix:** Add a `SimpleEvent::BecomesTargetAbility { controller: Option<ControllerRef> }` variant to `parse_simple_event`, handling "becomes the target of an ability [you control / an opponent controls / ∅]" with the appropriate `becomes_target_source_filter` call. The "player or permanent" recipient is a broader `valid_target` filter (any permanent or player).

| Card | Gap |
|------|-----|
| Loki, God of Mischief | `Whenever a player or permanent becomes the target of an ability you control` (False) |

**Skill:** parser-only misparse fix  
**Collision files:** `parser/oracle_trigger.rs` only  
**ROI score:** 1 card / very low complexity = **FAST WIN**

---

### Cluster F — Goad Combat Damage Trigger  
**1 card · NEW trigger pattern · MEDIUM complexity**

"Whenever one or more goaded creatures deal combat damage to one of your opponents" — no existing `TriggerMode` variant filters on the source being goaded. This needs a new `TriggerMode::GoadedDamage` (or extending `DamageDone` with a `goaded_source` filter) plus oracle_trigger.rs parsing for "whenever one or more goaded creatures deal combat damage to …".

| Card | Gap |
|------|-----|
| Puppet Master, String Puller | `Whenever one or more goaded creatures deal combat damage to one of your opponents` (False) |

**Skill:** `/add-trigger`  
**Collision files:** `types/triggers.rs`, `parser/oracle_trigger.rs`, `game/trigger_matchers.rs` or `game/triggers.rs`  
**ROI score:** 1 card / medium complexity = **LOW**

---

### Cluster G — Noncombat Damage by Source You Control (partial parser gap)  
**1 card · Existing primitives + parser extension · MEDIUM complexity**

`DamageDoneOnceByController` trigger mode exists. `DamageKindFilter::NoncombatOnly` exists. "during your turn" is expressible as a `TriggerConstraint`. The gap is that the oracle_trigger.rs parser does not recognize "a source you control deals noncombat damage to one or more of your opponents during your turn" as a combined DamageDone trigger with controller + noncombat + player-turn constraints.

| Card | Gap |
|------|-----|
| Molten Lavamancer | `Whenever a source you control deals noncombat damage to one or more of your opponents during your turn` (False) |

**Skill:** parser-only fix (extend noncombat-damage trigger parsing arm)  
**Collision files:** `parser/oracle_trigger.rs`  
**ROI score:** 1 card / low-medium complexity = **MEDIUM-LOW**

---

### Cluster H — Power Greater Than Base Power Filter  
**1 card · NEW `FilterProp` · MEDIUM-HIGH complexity**

"with power greater than its base power" requires checking whether a creature's effective power exceeds its printed/base power (i.e., it has received pumps from counters or layer effects). This is a new `FilterProp::PowerExceedsBase` that reads from layer-resolved state vs. printed state.

| Card | Gap |
|------|-----|
| Ms. Marvel, Elastic Ally | `Whenever a creature you control with power greater than its base power deals combat damage to a player` (False) |

**Skill:** `/add-trigger` (new FilterProp + game/filter.rs evaluation + parser/oracle_target.rs parsing)  
**Collision files:** `types/ability.rs`, `parser/oracle_target.rs`, `game/filter.rs`  
**ROI score:** 1 card / high complexity = **LOW**

---

### Cluster I — Named/Unknown Abilities  
**2 cards · Case-by-case · LOW ROI**

| Card | Gap | Notes |
|------|-----|-------|
| Galactus, Devourer of Worlds | `unknown` (False) | "Insatiable Hunger — Galactus attacks an opponent with the most life among your opponents each combat if able unless you control more creatures than that opponent" — attack requirement with dynamic conditional; complex new `AttackRequirement` with MostLife+CreatureCount comparison |
| M.O.D.O.K. | `unknown` (False) | "Mental Organism — Pay 3 life: M.O.D.O.K. connives. Activate only during your turn." — the ability word is not blocking (ability words are skipped), but the "Activate only during your turn" activation restriction on a life-cost connive ability may be the gap. After Cluster A lands, this may reduce to a parser fix for the timing restriction |

**Skill:** `/add-engine-effect` (Galactus: conditional attack requirement) + parser fix (M.O.D.O.K. after Cluster A)  
**ROI score:** 2 cards / high-very-high complexity = **VERY LOW**

---

### Cluster J — Modal Return from Graveyard (modal targeting)  
**1 card · Parser fix · MEDIUM complexity**

"Choose up to two. Return those cards from your graveyard to your hand. • Target artifact card. • Target creature card. • Target enchantment card. • Target land card." — the `target: false ×4` suggests each mode's type-filtered return-from-graveyard target is not parsed. The "choose up to two" modal structure with return effects should map to an existing `Effect::ChangeZone` from `Zone::Graveyard` to `Zone::Hand` with `TargetFilter::Typed`. The gap is likely the parser not recognizing these as valid `target` entries within the modal structure.

| Card | Gap |
|------|-----|
| Call Damage Control | `target` (False ×4) |

**Skill:** parser-only fix  
**Collision files:** `parser/oracle.rs` or `parser/oracle_effect/`  
**ROI score:** 1 card / medium complexity = **MEDIUM-LOW**

---

### Cluster K — Harness Mechanic (new set-specific mechanic)  
**1 card · NEW game-state mechanic · VERY LOW ROI**

"Harness" is an MSH-specific mechanic: a permanent can be "harnessed" via an activated ability, which then enables a second `∞` ability. This requires new game state tracking (harnessed flag), UI state, and a novel ability-gate mechanic. Out of scope for normal `/add-engine-effect` — requires dedicated planning.

| Card | Gap |
|------|-----|
| The Mind Stone | `harness` (False) |

**Skill:** Custom — defer until dedicated Harness infrastructure sprint  
**ROI score:** 1 card / very high complexity = **DEFER**

---

## Part II: All-Supported Cards (59 cards — misparse/runtime failures)

All 59 cards have `parse_details` entirely `supported: true` but are still `supported: false` at the card level. These fail at runtime or have incorrect AST that bypasses the category check.

### Sub-cluster M0 — Gap=0 Cards (9 cards — most opaque)

These have `gap_count=0` AND all parse_details True. The parser sees no issues but the cards don't work. Likely causes: runtime filtering on card-type enums missing from CoreType (e.g. Plan, though Plan is not confirmed as the cause), AST correct but sub-clause value wrong, or the coverage classifier is over-counting supported labels.

| Card | Likely cause | Notes |
|------|-------------|-------|
| Intrepid Ace | "as long as it isn't attacking or blocking" static condition | Likely ContinuousModification condition mismatch; Continuous static classified as supported but the `not attacking or blocking` condition may map incorrectly at layer eval time |
| Armed Assailant | "As long as this creature is equipped" condition | Static Continuous; condition "is equipped" should be handled — may be an AST verification gap, requires runtime test |
| Patriot, Young Avenger | Prowess + equipped static | Both individually supported; may be a parse-order or double-registration issue |
| Fleecemane Lion | Monstrosity + "as long as monstrous" static | Both supported; likely the static condition on `monstrous` flag fails at some edge case |
| Captain America, Super-Soldier | Shield counter replacement | Replacement `Moved` is True; may be an AST detail in the shield counter condition |
| The Ruinous Wrecking Crew | "choose up to X" where X = entering counter count | The ChangesZone trigger with variable modal X count; may be a quantity resolution gap |
| Hawkeye, Master Marksman | Trick Arrows — modal nested choices from Taps trigger | Complex nested "when you do, choose up to N" triggered trigger with multiple modes |
| Doctor Doom | "As long as you control an artifact creature or a Plan" | "Plan" may be an unregistered card type in the static filter at runtime, even though SearchLibrary for Plans (Masters of Evil) works |
| Beast, Erudite Aerialist | "as long as you've put one or more +1/+1 counters on Beast this turn" | **BLOCKED by PR #3918** — `CountersPutOnThisTurn` filter is the shared primitive introduced by that PR |

**Action for M0:** Each requires a targeted runtime scenario test to confirm the failure mode. Defer Beast pending #3918. Rest can be investigated one by one.

---

### Sub-cluster M1 — Keyword/Name False-Detection Misparsed (1 card)

| Card | Gap | Fix |
|------|-----|-----|
| Storm, Queen of Wakanda | Parser attributes the MTG `Storm` keyword to this card because her name starts with "Storm" | Add a name-guard in keyword detection to skip keyword attribution when the word is the card's own name; oracle_util.rs `SELF_REF_PARSE_ONLY_PHRASES` or keyword parser guard |

Parser-only fix in keyword scanning. **ROI: HIGH** (unblocks Storm; prevents future name-keyword collision on any "Storm" creature).

---

### Sub-cluster M2 — "Entered This Turn" / "First Time This Turn" Conditions (3 cards)

"Activate only if an artifact entered under your control this turn", "costs less if another creature with flying entered this turn", "if it's the first time that creature has become tapped this turn" — no `TriggerConstraint` or `ActivationRestriction` for "entered this turn" exists in the engine.

| Card | Condition |
|------|----------|
| Fixer, Techno Terror | "Activate only if an artifact entered under your control this turn" |
| Flying Drone | "This ability costs {1}{U} less to activate if another creature with flying entered under your control this turn" |
| Captain America, Living Legend | "if it's the first time that creature has become tapped this turn" (intervening-if on Taps trigger) |

**Primitive needed:** `ActivationRestriction::EnteredThisTurn { filter }` and `TriggerCondition::FirstTimeThisTurn { event }`. This is a NEW engine primitive. ROI: 3 cards / medium-high complexity.

---

### Sub-cluster M3 — Random Choice Conditional Override (1 card)

| Card | Gap |
|------|-----|
| Typhoid Mary, Fractured | "choose one at random. If you discarded a card this turn, you choose one instead" |

Random modal selection (`selection: Random`) exists. The conditional override "if you discarded a card this turn, you choose one instead" needs a `TriggerCondition::DiscardedThisTurn` guard that switches selection from Random to controller-choice. Partially tractable but requires conditional-override on modal selection. **ROI: 1/medium.**

---

### Sub-cluster M4 — Ongoing Restrictions from Triggers (2 cards)

| Card | Gap |
|------|-----|
| Spider-Woman, Secret Agent | "That creature can't become untapped for as long as you control Spider-Woman" |
| Willie Lumpkin, Postman | "that player can't attack you or permanents you control for as long as it has a vow counter on it" |

Both need `AddRestriction` with a "while-you-control-source" or "while-counter-present" duration. `AddRestriction` exists (`Effect::AddRestriction`). The parser gaps are: (1) "for as long as you control Spider-Woman" duration tied to controller's battlefield state, and (2) "for as long as it has a vow counter" counter-presence duration. Both are parser extension tasks.

---

### Sub-cluster M5 — Cast/Enter-Battlefield Effects in Trigger Chains (7 cards)

Complex effect sequences within trigger bodies that are parsed at category level but have wrong sub-effect AST:

| Card | Key gap in trigger body |
|------|------------------------|
| Nick Fury, Spymaster | "put a creature card with MV 3 or less from your hand onto the battlefield tapped and attacking" — ETB tapped+attacking in Attacks trigger |
| Helmut Zemo, Mastermind | "cast target instant/sorcery card with MV ≤ power from your graveyard" — cast from graveyard with dynamic MV ceiling |
| Cosmic Cube | "look at top 6, cast one with MV ≤ greatest power among attackers" — impulse-draw with greatest-attacker-power MV ceiling |
| Silver Surfer, Cosmic Voyager | "exile any number of other target permanents… return them at beginning of next end step" — multi-target blink with delayed return |
| Sauron, Dino Devotee | "Put a saurian counter on another target creature. It's a green Dinosaur with base power and toughness equal to its total number of +1/+1 counters" — counter-type + type change + CDA on target |
| Karolina Dean, Runaway | "add {W}{U}{B}{R}{G}. You can't spend this mana to cast spells from your hand" — restricted WUBRG mana production |
| Captain Marvel, Apex Avenger | "put the same number and kind of counters on Captain Marvel" — mirror counter type and count from a CounterAdded event |

---

### Sub-cluster M6 — Damage Tracking / Combat Conditions (4 cards)

| Card | Key gap |
|------|---------|
| Hawkeye, Avenging Archer | "if Hawkeye dealt damage to it this turn" on a dies trigger — source-to-specific-creature damage tracking this turn |
| Whiplash, Vengeful Engineer | "if he's equipped, each opponent loses X life where X = number of Equipment attached" — count of attached equipment as X |
| Wasp, Shrinking Savior | "draw a card for each creature with power less than 0" — `power < 0` filter |
| The Incredible Hulk | "If he's attacking, untap him and there is an additional combat phase" — conditional chain within Enrage trigger (`Effect::AdditionalPhase` exists; the conditional "if he's attacking" within the DamageReceived trigger body may be parsed incorrectly) |

---

### Sub-cluster M7 — Complex ETB / Zone-Change Effects (8 cards)

| Card | Key gap |
|------|---------|
| Batroc the Leaper | "deals damage equal to his power divided as you choose among any number of targets" — divided damage targeting |
| Alex Wilder, Runaway | "if you cast it from anywhere other than your hand" — cast-origin condition on ETB trigger |
| Baron Helmut Zemo | Boast body produces `CopySpell` misparse — "whenever a player casts a black spell, create a 2/2 token" (delayed trigger within Boast) |
| Klaw, Sonic Subjugator | Complex ETB "target player reveals cards from hand equal to 1 + graveyard creature count; you choose one to discard" — dynamic count reveal + opponent discard |
| Iron Fist, Living Weapon | "gains '{T}: Iron Fist deals damage equal to his power to any other target' until end of turn" — grants activated ability via trigger |
| Machine Man, Model X-51 | "he gains flying until end of turn" — keyword grant in trigger response (should work, but may be misparse of conjunction with +1/+1 counter) |
| Selfless Police Captain | "put its +1/+1 counters on target creature" — transfer specific counter count from dying creature |
| Lockjaw, Slobbering Teleporter | "Lockjaw and up to one other target creature you control can't be blocked this turn" — "when you do" triggered trigger from Phase trigger sub-chain |

---

### Sub-cluster M8 — Remaining One-Offs (33 cards)

The following cards have gap_count=1 and all parse_details True but need per-card investigation to identify the exact runtime/AST gap:

Armor Wars, Avenge, Black Widow Super Spy, Captain America Liberator, Captain America Super-Soldier (gap=0, M0), Conduit of Worlds, Doctor Doom (M0), Fleecemane Lion (M0), Hawkeye Young Avenger, Hulkling Burgeoning Bruiser, Intrepid Ace (M0), Lady Loki Agent of Chaos, Machine Man Model X-51, Patriot Young Avenger (M0), Pick Up the Pace, Promise of Loyalty, Rhino's Rampage, Rocket-Powered Goblin Glider, Ronin Shadow Stalker, Ruinous Wrecking Crew (M0), Selfless Police Captain, Storm Queen of Wakanda (M1), Super-Adaptoid, T'Chaka Venerable King, Terrific Team-Up, The Kingpin of Crime, Typhoid Mary (M3), Ultimate Nullification, Ultron Unlimited (partially covered by Cluster A), Vision Spectral Synthezoid, Vision Synthezoid Avenger, Willie Lumpkin Postman, Winter Soldier Icy Assassin, Winter Soldier Reborn Avenger, Wisecrack, Worlds Within Worlds.

Most of these are already covered in M0–M7; the remainder need scenario tests to diagnose actual failure mode.

---

## Wave Plan (Ordered by ROI + Dependency)

### Wave 1 — Fast Primitives (target: 6 cards unlocked)

**Rationale:** All changes land in shared files on a single checkout. No cross-PR blockers. The connive trigger is the largest single new primitive and unlocks 4 cards.

| Cluster | Cards | Skill |
|---------|-------|-------|
| A — Connive Trigger | Glorious Purpose, Iron Monger, Ultron, Leader | `/add-trigger` + `/add-replacement-effect` (Leader) |
| D — Play Card from Exile parser fix | Klaw Master of Sound | parser-only |
| E — Becomes Target of Ability parser fix | Loki God of Mischief | parser-only |

**Files touched:** `types/triggers.rs`, `game/effects/connive.rs`, `game/trigger_index.rs`, `parser/oracle_trigger.rs`  
**Sequential order within wave:** Implement A first (adds oracle_trigger patterns); then D and E (additional oracle_trigger arms — all in same file, implement sequentially).

---

### Wave 2 — Misparse Sweep (target: ~10 cards unlocked)

**Rationale:** Parser-only fixes for name-collision (Storm) and targeted sub-effect misparsed cards. No new engine types. All changes in parser layer.

| Sub-cluster | Cards | Skill |
|-------------|-------|-------|
| M1 — Keyword name false-detection | Storm Queen of Wakanda | parser-only |
| M2 — Entered-this-turn conditions | Fixer, Flying Drone, Captain America Living Legend | `/add-engine-effect` or parser-only (activation restriction) |
| M7 — Baron Helmut Zemo Boast misparse | Baron Helmut Zemo | parser-only |
| M7 — Machine Man flying grant | Machine Man Model X-51 | parser-only |
| M5 — Nick Fury ETB tapped+attacking | Nick Fury Spymaster | parser/engine fix |
| Cluster G — Noncombat damage trigger | Molten Lavamancer | parser-only |

**Files touched:** `parser/oracle_trigger.rs`, `parser/oracle.rs`, `parser/oracle_effect/`, `types/keywords.rs` (Storm guard)

---

### Wave 3 — Replacement Effects (target: ~3 cards unlocked)

| Cluster | Cards | Skill |
|---------|-------|-------|
| B — Token-creation substitution | Divine Visitation | `/add-replacement-effect` |
| B — Damage doubling | Mjölnir Hammer of Thor | `/add-replacement-effect` |
| B — Damage reset | Wolverine Fierce Fighter | `/add-replacement-effect` |

**Files touched:** `parser/oracle_replacement.rs`, `types/replacements.rs`, `types/ability.rs`, `game/effects/`  
**Sequential order within wave:** Implement separately (each has a distinct `ReplacementEvent`); can share a checkout but must be sequential.

---

### Wave 4 — Static Structure + New Filters (target: ~3 cards unlocked)

| Cluster | Cards | Skill |
|---------|-------|-------|
| C — Dragon Man CDA | Dragon Man Reformed Robot | `/add-static-ability` |
| C — Wolverine unblocked assignment | Wolverine Claws Out | `/add-static-ability` |
| H — Ms. Marvel power > base power | Ms. Marvel Elastic Ally | `/add-trigger` (new FilterProp) |

**Files touched:** `types/ability.rs`, `parser/oracle_static/`, `parser/oracle_target.rs`, `game/filter.rs`, `game/layers.rs`

---

### Wave 5 — M0 Investigation + Runtime Gap Fixes (target: ~8 cards unlocked)

Investigate the 9 gap=0 cards with scenario tests to confirm failure modes, then fix. Skip Beast (blocked by #3918).

| Card | First action |
|------|-------------|
| Intrepid Ace | Add runtime test; likely Continuous static condition fix |
| Armed Assailant | Add runtime test; likely misparse of `IsEquipped` condition |
| Patriot, Young Avenger | Add runtime test |
| Fleecemane Lion | Add runtime test; likely Monstrosity static condition |
| Captain America, Super-Soldier | Add runtime test; shield counter replacement |
| The Ruinous Wrecking Crew | Add runtime test; variable-X modal |
| Hawkeye, Master Marksman | Add runtime test; Trick Arrows nested modes |
| Doctor Doom | Add runtime test; check Plan type filter at layer eval time |

---

### Wave 6 — Complex Mechanics (target: ~4 cards unlocked)

| Cluster | Cards | Skill |
|---------|-------|-------|
| F — Goad damage trigger | Puppet Master | `/add-trigger` |
| M4 — Ongoing restrictions | Spider-Woman, Willie Lumpkin | parser extension + `AddRestriction` duration |
| J — Modal return from graveyard | Call Damage Control | parser fix |

---

### Wave 7 — Deferred / Blocked (post-#3918 + novel mechanics)

| Card | Blocker |
|------|---------|
| Beast, Erudite Aerialist | **Blocked by PR #3918** (`CountersPutOnThisTurn` filter) |
| The Mind Stone | Novel Harness mechanic — requires dedicated sprint |
| Galactus, Devourer of Worlds | Complex conditional attack requirement |
| M.O.D.O.K. | Partially unlocked by Cluster A (connive); re-evaluate after Wave 1 |
| Typhoid Mary, Fractured | Random-choice conditional override |

---

## File Collision Matrix

| Shared file | Waves touching it |
|-------------|-------------------|
| `parser/oracle_trigger.rs` | 1, 2, 6 — **HIGHEST collision risk** — serialize all waves that modify this file |
| `types/triggers.rs` | 1, 6 |
| `game/effects/connive.rs` | 1 |
| `game/trigger_index.rs` | 1 |
| `parser/oracle_replacement.rs` | 3 |
| `types/replacements.rs` | 3 |
| `types/ability.rs` | 3, 4 |
| `parser/oracle_static/` | 4 |
| `game/filter.rs` | 4 |
| `parser/oracle.rs` | 2, 4 |

Because `parser/oracle_trigger.rs` is touched in waves 1, 2, and 6, these waves must run **sequentially** on the same checkout. Waves 3 and 4 can run after Wave 1 completes (different file sets).

---

## Recommendation: Start with Wave 1

**Wave 1 is the best first implementation.** It unlocks **6 cards** from a single checkout touching a coherent set of files. The Connive Trigger primitive (Cluster A) is the highest-ROI single new primitive in the backlog: 4 cards from one `TriggerMode` addition + event emission. The two parser-only fast-wins (Clusters D and E) stack onto the same `oracle_trigger.rs` edit without any new engine types.

Wave 1 has no cross-PR blockers, clean collision profile (no overlap with PRs #3916/#3918/#3909), and establishes the connive-trigger infrastructure that also partially unblocks M.O.D.O.K. in Wave 7. Implement in this order within Wave 1: (1) Cluster A — add `TriggerMode::Connive`, event emission, oracle_trigger parsing; (2) Cluster D — extend `parse_play_card_trigger_subject` for zone qualifier; (3) Cluster E — add `SimpleEvent::BecomesTargetAbility` arm. All sequential on `main` checkout.
