# `CombatDamageTiming::OnStack` — Design

**Status:** design only, no code. This is the dedicated design pass that
`PLAN.md` §6/§7/§8 and `IMPLEMENTATION_PLAN.md` require before this axis is
built. It is the last `LegacyRuleSet` axis, and it gates the two remaining
Eternal Central presets, `middle_school()` and `classic_magic()`.

**Revision 2** (2026-09-15). This revision incorporates two independent
reviews: an architecture review, whose verdict was *REVISE* with four HIGH
findings, and an adversarial fact-check of the rules premises. §11 records
what each finding changed.

**As of:** `upstream/main` @ `05c27e0d5`. Every `file:line` below is pinned to
that commit. Paths are relative to `crates/engine/src/` unless stated.
Re-verify before implementing, because `combat_damage.rs` and `stack.rs` move
quickly.

---

## 0. Summary

**The rule.** Before M10, combat damage was **assigned**, put on the stack as
**one object per combat damage step**, and **dealt when that object resolved**,
after a full priority round. That object was neither a spell nor an ability,
so it could not be countered or targeted.

**The seam.** The engine already separates assignment from dealing internally
(`CombatState.pending_damage`). This design inserts a stack object and a
priority round at that seam. Assignment, the batch-replacement pipeline,
lifelink batching and the trigger/SBA loop are reused. They are not
duplicated.

**New surface:**
- one `StackEntryKind::CombatDamage` variant;
- an exhaustive spell / ability / combat-damage classification;
- a dealing-time resolver that turns each frozen assignment into one typed
  record (live source if it is still the same incarnation, otherwise
  incarnation-keyed LKI; recipients that are gone are dropped);
- a typed origin on the parked lifelink batch;
- step-start trigger handling at push time;
- one widening of the CR 609.7a "choose a source" options;
- a policy module like `game::mana_burn`.

**Plan.** Three implementation phases (§9). The gate flips, and the two
presets register, only in the last.

**Corrections to the charter:**
1. `PLAN.md:644`, `RESEARCH.md:331` and the `CombatDamageTiming` doc comment
   (`types/custom_format.rs:79-84`) call this rule "pre-6th-edition". That is
   backwards. Damage on the stack was **introduced** by the Classic Sixth
   Edition rules (1999) and **removed** by M10 (July 2009); see §1.3.
2. `PLAN.md`/`RESEARCH.md` treat both EC formats as "old rules = one overlay".
   Classic Magic actually describes itself as "Sixth Edition rules"; §1.4
   records how this design handles that.

Phase 3a fixes the doc comment. The merged design documents stay as reviewed,
per `IMPLEMENTATION_PLAN.md`'s convention; this document records the
corrections.

---

## 1. The rule being implemented

### 1.1 Sources

The current CR (`docs/MagicCompRules.txt`) has no damage-on-the-stack rule.
The historical text below comes from archived Comprehensive Rules files. Petr
Hudeček's rulebook archive is a fan mirror of the Wizards text; its files carry
the original Wizards headers.

| Document | URL |
|---|---|
| CR current as of **May 1, 2009** (last pre-M10) | <https://hudecekpetr.cz/other/rulebooks/comprehensive-2009-05-01.txt> |
| CR dated **April 23, 1999** (Classic Sixth Edition) | <https://hudecekpetr.cz/other/rulebooks/comprehensive-1999-04-23.txt> |
| CR effective **July 11, 2009** (M10) | <https://hudecekpetr.cz/other/rulebooks/comprehensive-2009-07-08.txt> |
| "Magic 2010 Rules Changes" (Forsythe/Gottlieb, 2009-06-10) | <https://magic.wizards.com/en/news/feature/rules-changes-2009-06-10> |
| Eternal Central — Middle School rules | <https://www.eternalcentral.com/middleschoolrules/> |
| Eternal Central — Classic Magic rules | <https://www.eternalcentral.com/classicmagicrules/> |
| Scryfall — Sixth Edition set record | <https://api.scryfall.com/sets/6ed> |

**Numbering trap:** in the pre-M10 CR the combat damage step is **rule 310**,
not 510; rule 510 was then "Status". Everything cited as `310.x`, `502.x`,
`419.x` or `600.x` below is a *historical* number. **Such numbers must never
appear in engine CR annotations**, because the annotation gate checks against
the current CR, where 310 is Battles. §8 gives the convention to use instead.

### 1.2 The 2009 text (verbatim)

> **310.1.** As the combat damage step begins, the active player announces how
> each attacking creature will assign its combat damage. Then the defending
> player announces how each blocking creature will assign its combat damage.
> All assignments of combat damage go on the stack as a single object. Then any
> abilities that triggered on damage being assigned go on the stack. […] Then
> the active player gets priority and players may play spells and abilities.
>
> **310.2a** Each attacking creature and each blocking creature will assign
> combat damage equal to its power. Creatures with power less than 0 assign 0
> combat damage.
>
> **310.3.** Although combat-damage assignments go on the stack as an object,
> they aren't spells or abilities, so they can't be countered.
>
> **310.4.** Combat damage resolves as an object on the stack. When it
> resolves, it's all dealt at once, as originally assigned. The combat damage
> object is then removed from the stack and ceases to exist. After combat
> damage finishes resolving, the active player gets priority.
>
> **310.4a** Combat damage is dealt as it was originally assigned even if the
> creature dealing damage is no longer in play, its power has changed, or the
> creature or planeswalker receiving damage has left combat.
>
> **310.4b** The source of the combat damage is the creature as it currently
> exists, if it's still in play. If it's no longer in play, its last known
> information is used.
>
> **310.4c** If a creature or planeswalker that was assigned combat damage is no
> longer in play, or is neither a creature nor planeswalker, the damage
> assigned to it isn't dealt.
>
> **310.5.** At the start of the combat damage step, if at least one attacking
> or blocking creature has first strike […] or double strike […], creatures
> without first strike or double strike don't assign combat damage. Instead of
> proceeding to end of combat, the phase gets a second combat damage step […].
> In the second combat damage step, any attackers and blockers that didn't
> assign combat damage in the first step, plus any creatures with double
> strike, assign their combat damage.
>
> **502.2c** Adding or removing first strike any time after combat damage has
> been put on the stack in the first combat damage step won't prevent a
> creature from dealing combat damage or allow it to deal combat damage twice.
>
> **502.28d** Giving double strike to a creature with first strike after it has
> already put first strike combat damage onto the stack in the first combat
> damage step will allow the creature to assign combat damage in the second
> combat damage step.
>
> **200.8** […] Combat damage on the stack is also an object, although many
> uses of the term "object" in these rules don't apply to it.
>
> **413.1.** Each time all players pass in succession, the object (a spell, an
> ability, or combat damage) on top of the stack resolves.
>
> **419.8a** […] he or she may choose […] a creature that assigned combat damage
> on the stack, even if the creature is no longer in play or is no longer a
> creature.
>
> Glossary, **Removed from Combat:** […] if combat damage assigned to or by that
> permanent is already on the stack, it will resolve normally.

The M10 CR's 510.2 confirms the change: "This turn-based action doesn't use
the stack. No player has the chance to cast spells or activate abilities
between the time combat damage is assigned and the time it's dealt. **This is a
change from previous rules.**"

### 1.3 Era: Sixth Edition through M10

**The 1999 CR already has the full model** (310.2–310.4, 408.1g):
- "All announcements of combat damage go on the stack as a single entry";
- it "can't be countered";
- it is "dealt as originally assigned", with the source "as it currently
  exists, or as it most recently existed".

**By 2009 the model is materially the same.** The 2009 text adds:
- the word "object";
- triggers on assignment;
- double strike;
- planeswalker recipients;
- 310.4c's "is neither a creature nor planeswalker" clause;
- an explicit priority pass after resolution.

**Before Sixth Edition** (Fourth/Fifth Edition) there was no stack at all:
batches, interrupts, and a damage-prevention step. **Nothing in this project
targets that model.** The Old School 93/94 and 95 presets use modern rules
plus mana burn (RESEARCH.md §1).

**Dates.** Sixth Edition released **1999-04-21** (Scryfall `sets/6ed`;
MTG Wiki). The earliest archived CR is dated 1999-04-23. M10's rules took
effect **2009-07-11**.

### 1.4 What each preset actually says, and design decision D0

The two Eternal Central pages frame their rules differently:

- **Middle School:** "Damage Uses the Stack (as it did with Sixth Edition rules
  for example, so combat tricks such as Morphling, Triskelion, and Mogg Fanatic
  work)", and "**Everything else in Middle School works the same as modern
  Magic rules** (current London Mulligan rule, etc.)". Its C.1.Ruling.3 gives
  the sequence: "6. Assign combat damage (but don't deal it yet) 7. Chance for
  instants and abilities. 8. Deal combat damage. 9. Triggers on damage being
  dealt".
- **Classic Magic:** "DAMAGE USES THE STACK (as in, combat tricks such as
  Morphling, Triskelion, and Mogg Fanatic work)", but the format's
  description says it "**uses Sixth Edition rules** (including stacking
  damage, and mana burn)". There is no "everything else is modern" sentence.
  It also prints card-text overrides, for example Time Vault ("Play this
  ability if only there's a time counter on Time Vault") and a note that
  Illusionary Mask works "as … in a previous rules update, prior to the modern
  wording".

**Design decision D0 (a project decision, not a quotation): `OnStack` changes
only combat damage *timing* and dealing-time identity. Everything else follows
the current CR and current Oracle text.**
- For **Middle School** this is exactly what the page requires.
- For **Classic Magic** it is a deliberate approximation: the page names
  damage on the stack and mana burn as the rules it cares about, and
  reproducing the full 1999–2009 rules is out of scope for this axis.

**Known departures from 2009 behavior that D0 accepts,** so reviewers don't
rediscover them:

| 2009 rule | Current rule D0 uses | Observable difference |
|---|---|---|
| 502.63a "Deathtouch is a triggered ability" + 502.9d (trample considers only actual toughness) | CR 702.2c + CR 702.19b: deathtouch is static; 1 counts as lethal for trample assignment | A deathtouch trampler assigns 1 per blocker, not toughness |
| 502.68a "Lifelink is a triggered ability" | CR 702.15b: static, part of the damage event | No lifelink trigger to respond to; life is gained when the damage object resolves |
| 310.5 read literally: "any attackers and blockers that didn't assign combat damage in the first step" | CR 702.7b: participation is fixed by who had first/double strike "as the first combat damage step began" | A first striker whose blocker left (so it assigned nothing) does not assign again in the second step |
| Division among multiple blockers: 310.2c/d "divided as its controller chooses" | CR 510.1c/d: "divided as its controller chooses among them" | None |

**Classic Magic card-text overrides** (Time Vault, Illusionary Mask) are a
separate, per-card concern, **outside this axis**. They must be resolved or
explicitly disclosed before `classic_magic()` registers (§9 Phase 3c), under
the same no-caveated-exposure rule as PLAN.md §7.

**Triggers on damage being assigned** (310.1) are out of scope, because no
current Oracle text has one:
- Scryfall `o:"is assigned combat damage"` returns zero cards (2026-09-15).
- `o:"assigns combat damage"` with when/whenever returns five cards, all
  static "assigns combat damage equal to its toughness" or "as though it
  weren't blocked" text.

---

## 2. How combat damage works today

| Concern | Location | Note |
|---|---|---|
| Step entry | `game/turns.rs:3554` (`Phase::CombatDamage` arm of `auto_advance_once`) | `flush_layers`, then `resolve_combat_damage`; `Some(wf)` means wait, otherwise Priority. |
| Main resolver (re-entrant) | `game/combat_damage.rs:107` `resolve_combat_damage` | Lifelink-resume guard (`:115`), `regular_damage_done` guard (`:141`), first-strike snapshot (`:145-153`). |
| First-strike sub-step | `combat_damage.rs:157-190` | `collect_damage_assignments` → **`take_pending_damage` → `apply_combat_damage`** (`:162-163`). |
| Regular sub-step | `combat_damage.rs:193-218` | Same pair at `:197-198`. |
| Sub-step bookkeeping | `combat_damage.rs:226` `finish_combat_damage_sub_step` | Sets flags; computes `include_phase_event` (the CR 500.6 step-start marker, `:239-247`); runs `process_combat_damage_triggers` (`:46`); returns Priority after first strike if the stack is non-empty. |
| Lifelink park/resume | `combat_damage.rs:332` `resume_pending_combat_lifelink` | After finishing a parked batch it **re-enters `resolve_combat_damage`** (`:377`) to run the mandatory next sub-step. Reached from `engine_replacement.rs:197` and the elimination path (`turns.rs:3299`). |
| Assignment collection | `combat_damage.rs:745` `collect_damage_assignments` | Resumable through `damage_step_index`; `WaitingFor::AssignCombatDamage` / `AssignBlockerDamage`; handlers at `game/engine_combat.rs:528,750`. |
| Assigned-but-not-dealt data | `game/combat.rs:299` `CombatState.pending_damage: Vec<(ObjectId, DamageAssignment)>` | `DamageAssignment { target: DamageTarget, amount: u32 }` (`combat.rs:392-402`). |
| Dealing | `combat_damage.rs:1535` `apply_combat_damage` | Phase A gate (`effects/deal_damage.rs:445`, reads recipients live at `:484-485`); Phase B `replacement::replace_combat_damage_batch`; Phase C apply, commander damage (`source_is_commander` read live, `:1571-1575`), per-source lifelink (`drain_combat_lifelink`, `:1778`). |
| Source context | `effects/deal_damage.rs:308` `DamageContext::from_source` | Reads `state.objects[id]` live and records `source_incarnation`, with **no zone check**. `fallback` (`:355`) keeps only the controller and **no keywords**. |
| LKI | `types/game_state.rs:19856` `lki_cache` (id-keyed); `:19872` `lki_by_incarnation` (id → incarnation → snapshot) | Both are cleared on step transition (`turns.rs:1125-1127`). `LKISnapshot` (`:372`) has `keywords`, `controller` and `colors`, but **no `is_commander`**. |
| Completeness gate | `game/priority.rs:81-91` | Empty stack + all pass + `CombatDamage && !regular_damage_done` → re-enter `auto_advance`. |
| Elimination | `game/elimination.rs:970-981` | Removes every stack entry whose `stack_object_controller == leaver`. For id-only entries this falls back to `entry.controller` (`stack.rs:1033-1038`). |

**The seam:** in both sub-steps, `take_pending_damage` is immediately followed
by `apply_combat_damage`. Everything before that pair is *assignment*;
everything from `apply_combat_damage` on is *dealing*. `OnStack` splits the
pair across a stack object.

**Legacy-axis pattern to follow:** `game/mana_burn.rs:31-42` (`policy_of`,
`applies`); `game/legend_scope.rs:46,61`.

---

## 3. Design

### 3.1 The stack object — `StackEntryKind::CombatDamage`

```rust
// types/game_state.rs — new StackEntryKind variant (sketch)
/// Pre-M10 combat damage on the stack: one object per combat damage sub-step,
/// holding every assignment made in it. Neither a spell nor an ability.
CombatDamage {
    sub_step: CombatDamageSubStep,          // existing enum, game_state.rs:14785
    assignments: Vec<AssignedCombatDamage>,
}

/// One assignment, frozen when damage went on the stack.
pub struct AssignedCombatDamage {
    pub source: ObjectIncarnationRef,       // types/identifiers.rs:147
    pub target: AssignedDamageRecipient,
    pub amount: u32,
}
pub enum AssignedDamageRecipient {
    Object(ObjectIncarnationRef),
    Player(PlayerId),
}
```

**D1 — one object per sub-step, not per source.** 310.1 says "All assignments …
go on the stack as a single object" (1999: "a single entry"). One entry per
source would open a priority window *between* two creatures' damage. It would
also break the current CR 510.2 simultaneity that `replace_combat_damage_batch`
and per-source lifelink batching depend on.

**D2 — carry incarnations, not bare ids.** 310.4b and 310.4c turn on whether a
source or recipient is *still the same object*. The engine keeps an `ObjectId`
across zone changes and bumps an incarnation (CR 400.7). `ObjectIncarnationRef`
is the established identity (`attacking_incarnations_this_combat`,
`lki_by_incarnation`). Every read that D2 protects must key on the incarnation
too; see §3.4 and D6. The modern path keeps `pending_damage` unchanged, and
`OnStack` converts it at the seam.

**Object controller: none.** The 2009 rules give the combat damage object no
controller; 600.4a removed only "spells and abilities controlled by that
player" when a player left. Current CR 800.4a makes "objects on the stack not
represented by cards" controlled by a leaving player cease to exist. So an
engine-assigned controller is a **rules hazard**: if the active player left a
multiplayer game, every pending assignment — including the defenders' — would
vanish. The entry therefore needs an explicit *no-controller* answer, not the
active player:
- `stack_object_controller` (`stack.rs:1033`) and the elimination sweep
  (`elimination.rs:970-981`) must skip `CombatDamage`.
- A leaving player's *recipients* left the game with them (CR 800.4a), so
  damage assigned to them is dropped at dealing time (§3.4). Whether damage
  assigned *by* their departed creatures is still dealt (310.4a says damage is
  dealt "even if the creature dealing damage is no longer in play") is open
  question Q5.
- `StackEntry.controller` is a non-optional `PlayerId`, so Phase 3a decides
  between two options:
  - (a) make controller lookups go through the classification (§3.5), which
    has no controller for `CombatDamage`;
  - (b) widen the field.

  (a) is recommended: it touches only the readers, not the serialized shape.

**Entry `id` / `source_id`.**
- `id`: fresh `ObjectId(next_object_id++)` with no `GameObject`, exactly like
  `push_keyword_action` (`engine.rs:17196`).
- `source_id`: the entry's own id. There is no single source, and `ObjectId(0)`
  is the monarch-trigger sentinel (`triggers.rs:5968`).
- Readers of `entry.source_id` are audited in Phase 3a (§7 R4).

**Why not the alternatives:**

| Alternative | Why rejected |
|---|---|
| A synthesized `TriggeredAbility` | A legal target for "target triggered ability" (Stifle), and counterable. Violates 310.3. The charter's earlier draft made this mistake (CONTEXT.md:537). |
| A `KeywordAction` variant | `stack_ability_kind()` classifies `KeywordAction` as an *activated ability* (`game_state.rs:16943`). Wrong layer. |
| No stack object; a special "damage window" priority state | Responses must resolve *above* pending damage. Every stack-based path (resolve order, split second, "counter target spell", AI stack awareness, end-the-turn) would need to know about an invisible pseudo-object. |

### 3.2 Pushing: the seam in `resolve_combat_damage`

```text
collect_damage_assignments(state, sub_step)  -> Some(wf) => return Some(wf)   // unchanged
match combat_damage_timing::policy(state):
  Modern  => take_pending_damage; apply_combat_damage; finish_…               // unchanged
  OnStack => let pending = take_pending_damage(state);
             reset per-sub-step collection state (damage_step_index, damage_assignments)
             collect_step_start_triggers_and_run_sbas(state, sub_step, events)  // CR 500.6 + CR 704.3
             push_combat_damage_object(state, sub_step, freeze(pending), events) // journaled, §3.10
             reset_priority(state);
             return Some(WaitingFor::Priority { player: turn_decision_maker(state) })
```

**Exhaustive `match` on `CombatDamageTiming`,** never `if uses_stack`. A future
third timing must be a compile error at this site.

**`freeze(pending)`** records each source's and recipient's current
incarnation.

**Step-start triggers (CR 500.6)** go on the stack *before* the first priority
window: "They are put on the stack the next time a player would receive
priority."
- **Today:** the `PhaseChanged { CombatDamage }` marker is synthesized inside
  `process_combat_damage_triggers` at *dealing* time, gated by
  `include_phase_event` (`combat_damage.rs:239-247`, `:56-60`). Keeping that
  unchanged would move such triggers to after the window, which is wrong.
- **Under `OnStack`:** the push step collects the marker's triggers and runs
  SBAs before granting priority.
- **Ordering:** triggered abilities are put on the stack above the damage
  object. They resolve first, which is what "the next time a player would
  receive priority" means.
- **Resolution side:** the resolver arm (§3.3) passes
  `include_phase_event = false`.
- **Each sub-step emits the marker once,** matching today's rule of first
  strike always, regular only when it is the sole sub-step.
- Modern keeps its current computation. The value becomes a parameter the
  caller supplies, not something `finish_combat_damage_sub_step` infers from
  flags.

**Priority seat.** Use `turn_control::turn_decision_maker(state)`
(`turn_control.rs:169`, CR 723 player control), which is what step entry uses
(`turns.rs:1121`). `finish_combat_damage_sub_step` currently hard-codes
`active_player`. Phase 3b aligns both on `turn_decision_maker`, so the two
paths cannot disagree.

**Empty assignment.** When `pending` is empty (no creature assigns damage),
this design pushes **no object**, and the sub-step finishes as it does today
(Q1 records why this is a simplification). Consequence, matching the engine
today: an empty *first-strike* sub-step flows straight into the regular
sub-step's collection, in one priority window.

**D3 — "damage is already on the stack" is derived from the stack, not
stored.** `resolve_combat_damage` has four callers:
- the turns arm;
- the assignment handlers;
- the lifelink resume;
- the priority completeness gate.

It must not re-collect assignments for a sub-step whose object is still on the
stack.
- A helper in the style of `crew_pending_on_stack` (`engine.rs:17043`),
  `combat_damage_on_stack(state) -> Option<CombatDamageSubStep>`, guards next
  to the `regular_damage_done` check (`:141`).
- A stored `CombatState` flag would be a second authority that can drift from
  the stack, e.g. after CR 724.1b/724.2b exile the stack.
- D3 guards only *while the object is on the stack*. Re-entry *after*
  resolution is governed by the batch origin (§3.3, D7).

**Collection state at push.**
- Reset `damage_step_index` and `damage_assignments`, as
  `finish_combat_damage_sub_step`'s first-strike arm does.
- **Leave `first_strike_done` / `regular_damage_done` unset.** They mean
  "damage for this sub-step has been *dealt*". The priority completeness gate
  depends on that meaning.

### 3.3 Resolving: `stack.rs` arm

Template: the `KeywordAction` early return at `game/stack.rs:1372`. Add a
sibling early return before `bind_resolution_scope`:

```text
StackEntryKind::CombatDamage { sub_step, assignments } =>
    combat_damage::resolve_combat_damage_object(state, sub_step, &assignments, events)
    // StackResolved + finish_resolving_stack_entry(Resolved), as KeywordAction does
    // (placement relative to a lifelink park: §7 R2)
```

**`resolve_combat_damage_object`:**
1. **Resolve every frozen assignment into one typed record** (§3.4, H3):
   ```rust
   struct ResolvedCombatDamage {
       ctx: DamageContext,          // live-if-same-incarnation, else incarnation-keyed LKI
       source_is_commander: bool,   // same authority as ctx, never a separate live read
       target: DamageTarget,        // only recipients that survived 310.4c validation
       amount: u32,
   }
   ```
   Assignments whose recipient fails 310.4c are dropped here, before Phase A,
   so no prevention event fires for damage that isn't dealt.
2. **Call `apply_combat_damage` with `&[ResolvedCombatDamage]` on both paths.**
   - The modern path builds the same records with `DamageContext::from_source`
     and the live commander flag. There is one input shape and one authority
     per path, not an `Option` override.
   - `apply_combat_damage` stops reading `state.objects` for the source itself.
   - The Phase A recipient gate is unchanged. It still applies protection and
     prohibition against the live recipient, which 310.4c has already
     confirmed is the same incarnation.
3. **Batch outcome.**
   - `Complete`: call `finish_combat_damage_sub_step(…, include_phase_event =
     false)`.
   - `Paused`: park through `pending_combat_lifelink` **with origin
     `StackObject`** (D7).

**D7 — typed batch origin.**
- **The bug it prevents:** `resume_pending_combat_lifelink` (`:332`) currently
  re-enters `resolve_combat_damage` after finishing a parked batch (`:377`),
  to run the mandatory next sub-step. Under `OnStack` that re-entry would
  collect and push the *next* object from inside the replacement-choice
  handler, with an empty stack, skipping 310.4's "After combat damage finishes
  resolving, the active player gets priority".
- **The fix:** add `origin: CombatDamageBatchOrigin { TurnBasedAction,
  StackObject }` to `PendingCombatLifelink`. The resume re-enters only for
  `TurnBasedAction`. For `StackObject` it returns Priority to the decision
  maker, and the next sub-step starts through the ordinary completeness gate.
- **Why typed:** an exhaustive match, not a boolean, so a future origin has to
  decide.
- **Both resume doors are covered:** `engine_replacement.rs:197` and the
  elimination path at `turns.rs:3299`.

**After resolution.**
- The decision maker gets priority (310.4; engine `priority.rs:154-165`).
- **After the first-strike object:** once all players pass with an empty stack,
  the completeness gate (`priority.rs:81-91`) sees `CombatDamage &&
  !regular_damage_done`. It re-enters `auto_advance` → `resolve_combat_damage`,
  which collects regular assignments and pushes the second object. This is
  310.5's second combat damage step, with its own priority window.
- **After the regular object:** `regular_damage_done` is set, so the next
  all-pass on an empty stack advances to end of combat.

**D4 — no counter or target interaction at resolution.** Nothing can counter
or retarget the object (310.3, §3.5). There is no fizzle check, no
`bind_resolution_scope`, and no zone routing.

### 3.4 Dealing semantics (310.4a–c)

These rules live **only** in the record resolver of §3.3 step 1, never inside
the batch.

| Case | Rule | Behavior |
|---|---|---|
| Source is the frozen incarnation, on the battlefield | 310.4b | `DamageContext::from_source`, using **current** characteristics: lifelink granted after assignment counts, deathtouch lost after assignment doesn't. `source_is_commander` read live. |
| Source left the battlefield or changed incarnation (sacrificed Mogg Fanatic, bounced Morphling) | 310.4a + 310.4b | Dealt with the assigned amount. Source = **LKI of the frozen incarnation**, `lki_by_incarnation[id][incarnation]` (`game_state.rs:19872`), never the id-keyed `lki_cache`. New `DamageContext::from_lki(snapshot, incarnation)` sets `source_incarnation` to the frozen incarnation and reads controller plus lifelink, deathtouch, wither, infect and toxic from `snapshot.keywords`. |
| Commander flag for an LKI source | current CR 903.10a + CR 704.6c (21 combat damage from the same commander) | `LKISnapshot` has **no `is_commander`**. Add it to `LKISnapshot` (`#[serde(default)]`, captured with the rest of the snapshot), or freeze it into `AssignedCombatDamage` at assignment. Recommendation: **add it to `LKISnapshot`**, so every LKI damage source, not only combat, gets it. |
| Source power changed after assignment | 310.4a | Amount stays as assigned. |
| Recipient creature, planeswalker or battle is the frozen incarnation, still that type, on the battlefield | 310.4a | Dealt, **even if removed from combat**. |
| Recipient left, changed incarnation (flickered), or is no longer a creature, planeswalker or battle | 310.4c | Dropped before Phase A. No damage, no prevention event. |
| Recipient player has left the game | current CR 800.4a (the player and their objects left the game) | Dropped: there is no recipient to deal damage to. |
| Protection, prevention or redirection created after assignment | 310.4 + current CR 615 | Applies. The Phase A gate and Phase B batch evaluate them when the damage would be dealt. |

- **Battles.** 310.4c predates battles; under D0 current object types apply.
  Per §8, code annotates the current rules and names 310.4c only in prose.
- **LKI lifetime.** Both LKI maps clear on step transition (`turns.rs:1125-1127`).
  The step cannot end while the object is on the stack: the engine advances
  only on an empty stack, and current CR 117.4/405.5 say the same for spells
  and abilities. §3.5's classification makes the engine treat this object like
  them for that purpose; this is an engine invariant, not a literal CR
  application. A debug assertion requires an off-battlefield source to have an
  incarnation-keyed LKI entry.
- **Existing modern-path gap (out of scope):** `from_source` checks no zone.
  Under modern rules nothing changes between assignment and dealing, so this
  is unreachable. It is recorded only.

**D8 — damage events name the frozen incarnation.**
- **The problem:** observer triggers ("whenever a creature you control deals
  combat damage to a player") match `CombatDamageDealtToPlayer.source_amounts`
  and `DamageDealt` sources against the **live** object
  (`trigger_matchers.rs:1339-1346` → `valid_source_matches` →
  `target_filter_matches_object`). For a sacrificed source that is a graveyard
  card, or a missing token.
- **The fix:** the event must carry the source incarnation (the ctx already
  has `source_incarnation`), and the matcher must evaluate the source filter
  against the incarnation-keyed LKI when the live incarnation differs.
- **Scope:** a pre-existing LKI source-matching building block is the
  preferred authority. Phase 3b traces what `valid_source_matches` does with
  `ZoneChangeRecord`/LKI today before adding anything (§7 R8).

### 3.5 Classification: not a spell, not an ability

`StackEntry::stack_ability_kind()` (`game_state.rs:16943`) returns
`Option<StackAbilityKind>`, where `None` means spell.
- **Direct caller:** exactly one, `matches_stack_ability_kind`
  (`game_state.rs:16961`).
- **Real exposure:** the ~87 non-test `src` files that pattern-match
  `StackEntryKind` with `matches!`/`if let` (§3.6). There, "not a Spell and has
  no `ability()`" is implicitly read as one of the existing kinds.

**D5 — an exhaustive projection of the kind:**

```rust
pub enum StackObjectClass {
    Spell,
    Ability(StackAbilityKind),
    /// Neither a spell nor an ability (pre-M10 combat damage, 310.3).
    CombatDamage,
}
```

This is the one place that decides what a stack object *is* for rules
purposes, and that includes the controller question from §3.1.
`matches_stack_ability_kind` becomes `matches!(class, Ability(k) if …)`. It
parameterizes an existing axis rather than adding a sibling predicate
(CLAUDE.md "Parameterize, don't proliferate").

**Consequences:**
- **Counterspells and Stifle can't target it.** `effects/counter.rs:141,412`
  use `matches!(… Spell)`; `TargetFilter::StackAbility` goes through
  `matches_stack_ability_kind`; `targeting.rs:2383-2388` is exhaustive.
- **Split second** (current CR 702.61a): `keywords::stack_has_split_second`
  (`keywords.rs:97`) reads `objects[entry.id]`, and there is no object. Safe.
- **Storm** counts casts (`derived_views.rs:2205`). Unaffected.
- **Ending the turn / ending combat** (current CR 724.1b / 724.2b, "Exile every
  object on the stack"): `effects/end_phase.rs:51-62` makes non-card entries
  cease to exist. See §7 R3.

### 3.6 Match sites

The first draft sized this with a `KeywordAction` grep, which misses real
sites. The review found `elimination.rs:984`, `zones.rs:43`,
`resolved_commands.rs:1139-1142` (journaled stack-kind rewrite), server
constructors and several `phase-ai` modules. The rule for Phase 3a is:

> Every `match`, `matches!`, `if let` and `let … else` on `StackEntryKind`, and
> every reader of `StackEntry::ability()` that treats `None` as a particular
> kind, across `engine`, `phase-ai`, `server-core`, `phase-server`,
> `engine-wasm` and `manabrew-compat`, gets an explicit decision.

```bash
git grep -n "StackEntryKind::" -- 'crates/*/src/**' ':!*tests*'   # 87 files at pin
git grep -n "\.ability()" -- 'crates/*/src/**' ':!*tests*'
```

**Known decisions** (not exhaustive; the grep is authoritative):

| Site | Decision for `CombatDamage` |
|---|---|
| `game_state.rs:16281,16294` `ability()`/`ability_mut()` | `None` |
| `game_state.rs:16042` `StackResolutionEntryProvenance` | new `CombatDamage` provenance |
| `game_state.rs:16946` `stack_ability_kind` | replaced by D5 |
| `game_state.rs:25101` yield-scope matching | a yield "until stack empty" waits for it like any entry |
| `stack.rs:1033` `stack_object_controller`; `elimination.rs:970-984` | no controller (§3.1) |
| `stack.rs:1372` / `:1424` | resolver arm / `unreachable!` |
| `stack.rs:5031,5086,5102` display grouping | never grouped |
| `resolved_commands.rs:1139-1142` stack-kind rewrite | not rewritable; decide and journal (§3.10) |
| `zones.rs:43` | decide explicitly (trace) |
| `effects/copy_spell.rs:172,200,856,943` | not a copy source |
| `derived_views.rs:2194,2810-2831,2849,2915` | not a spell; label; assignment lines (§5) |
| `triggers.rs:3017-3022` | `None` |
| `analysis/resource.rs:6648,7011,7189,7245`; `ai_support/targeted_exchange.rs:568` | no ability / not a target |
| `phase-ai` `stack_awareness.rs:244`, `deck_knowledge.rs:122`, planner, search, tactical_gate, auto_play, `bin/resolve_bench.rs:202` | §6 |

### 3.7 "Choose a source" (CR 609.7a) — the Circle of Protection class

In Middle School and Classic Magic this is the most-played interaction the
overlay creates: activating Circle of Protection: Red *after* damage is on the
stack, choosing a creature that may already have been sacrificed.
- **Historical rule** (419.8a): allowed exactly this.
- **Current CR 609.7a already allows it:** "any object referred to by an object
  on the stack … (even if that object is no longer in the zone it used to be
  in)".
- **Gap:** `effects/choose_damage_source.rs:44` `damage_source_options`
  enumerates objects in the battlefield and stack zones, plus the command zone.

**D6:**
- Widen `damage_source_options` to chain in each `AssignedCombatDamage.source`
  of every `CombatDamage` entry on the stack.
- For a source whose live incarnation differs, the filter ("a red source")
  evaluates against **`lki_by_incarnation` for the frozen incarnation**, not
  `lki_cache`. The same identity discipline as §3.4 applies.
- The chosen-source shield must match damage by id **and incarnation**:
  `DamageContext.source_incarnation` already exists (`deal_damage.rs:311`),
  and `from_lki` sets it to the frozen one (§7 R5).
- Not gated by the format policy: this is a correct application of current
  CR 609.7a, and under modern rules no `CombatDamage` entry exists.

### 3.8 First strike and double strike

`first_strike_participants` is snapshotted as the step begins
(`combat_damage.rs:145-153`), and `deals_in_substep` (`:442`) admits to the
regular sub-step anything not in the snapshot, or with live double strike.
That yields:
- **502.2c:** removing first strike after first-strike damage is on the stack
  → still in the snapshot, no double strike → no regular damage. Granting first
  strike to a creature outside the snapshot → still deals regular damage.
- **502.28d:** granting double strike after first-strike damage is on the stack
  → deals regular damage.

One literal 2009 reading differs (§1.4 table, third row); D0 follows current
CR 702.7b. Phase 3b tests 502.2c and 502.28d. They are the cheapest proof that
the snapshot survives the new window.

### 3.9 Policy module

```rust
// game/combat_damage_timing.rs — mirrors game::mana_burn
pub(crate) fn policy_of(format_config: &FormatConfig) -> CombatDamageTiming { … }
pub(crate) fn policy(state: &GameState) -> CombatDamageTiming { policy_of(&state.format_config) }
```

- Callers `match` on the enum; there is no `bool` wrapper.
- **It lands in Phase 3b with its first caller.** A caller-less module in
  Phase 3a would trip `clippy -D warnings` dead code.

### 3.10 Journaling

Stack mutations go through journaled authorities.
- Removal is by position "so a replay reproduces" (`elimination.rs:965-970`).
- Trigger claims require "a live journal cause" (`combat_damage.rs:411`).

`push_combat_damage_object` runs inside a turn-based action (from
`auto_advance` or the assignment handlers), not a player-action handler. So
Phase 3b must:
- name the journaled push command and its cause (the combat-damage turn-based
  action of the current step), reusing the stack-push journal authority
  (`stack.rs:272` `journal_stack_push`) rather than adding a parallel one;
- decide `resolved_commands.rs:1139-1142`'s stack-kind rewrite for this kind;
- add a replay round-trip test (§9 Phase 3b, test 14).

---

## 4. Serialization and protocol

- A new `StackEntryKind` variant, `AssignedCombatDamage`, the `PendingCombatLifelink.origin`
  field and an `LKISnapshot.is_commander` field all change serialized
  full-game state.
- **Bump `lobby-broker` `PROTOCOL_VERSION`** (71 at pin, `crates/lobby-broker/src/protocol.rs:519`)
  with its changelog line, value pins and client mirror (precedent: #8870,
  changelog entries 18/19). Additive fields take `#[serde(default)]`.
- `LOBBY_PROTOCOL_VERSION` does **not** move; format selection is unchanged.
- Check `size_of::<StackEntry>() <= 768` (`types/game_state_size.rs:66`) after
  the edit.

## 5. Frontend — display only

- **Types:** TS types are hand-written. Add the variant to the
  `client/src/adapter/types.ts:1855` `StackEntryKind` union.
- **Label:** `components/stack/StackEntry.tsx:103-166` prefers
  `details?.kind_label`. The engine supplies the label ("Combat damage",
  optionally "— first strike") and the assignment lines through
  `stack_entry_detail` / `stack_entry_targets` (`derived_views.rs:2806,2915`).
- **No client-side derivation** of who deals what to whom.
- **Arcs:** `StackTargetArcs.tsx` renders the engine-provided lines.
- **No new `WaitingFor` or `GameAction`.** Follow the `add-frontend-component`
  skill.

## 6. AI

- **Must not break:**
  - `stack_awareness.rs:244` `assess_spell_impact` needs an arm (not a counter
    target, value 0).
  - `deck_knowledge.rs:122`: no source card.
  - Planner, search, tactical_gate and auto_play readers found by the §3.6
    grep.
  - The AI gets ordinary Priority with the object on the stack.
- **Not required to exploit.** "Sacrifice Mogg Fanatic after damage is on the
  stack" is a strength improvement, not rules fidelity. It is a follow-up
  policy (`add-ai-feature-policy`).
- **`cargo ai-gate`:** default baselines run built-in formats, where the new
  path is unreachable. Run it once in Phase 3c to confirm no change. No
  baseline refresh.

## 7. Risks to trace in the implementation plans

| # | Risk | Where to trace |
|---|---|---|
| R1 | **Double trigger collection.** Dealing now runs inside `resolve_next`, under a `PassPriority` pipeline that scans `events[event_start..]`, which includes damage events `process_combat_damage_triggers` already collected. The lifelink claim (`combat_damage.rs:389`) only covers the *paused* path. The modern `Complete` path relies on the post-action dedup. Phase 3b must name that mechanism (`triggers.rs:11103` `filter_already_collected_trigger_events_from` and its callers in `engine_priority.rs`) and prove it with a probe: a "whenever a creature deals combat damage" trigger fires **exactly once**. | `triggers.rs:11103`, `engine_priority.rs` |
| R2 | **Lifelink pause during stack resolution.** Beyond D7's re-entry fix: when `finish_resolving_stack_entry` runs relative to the park, and whether `resolving_stack_entry` / `settle_resolving_stack_entry_after_continuation_resume` (`stack.rs:37,53,1304-1330`) expects a continuation. | `stack.rs`, `combat_damage.rs:332` |
| R3 | **Ending the turn (CR 724.1) and ending combat (CR 724.2, Mandate of Peace)** with the object on the stack. It must cease to exist. The CR 724.2 path goes `end_combat_phase_to_postcombat` → `complete_end_combat_teardown` (`turns.rs:269-296`, `combat = None`) and shares `exile_nonresolving_stack_objects` (`end_phase.rs:51`). Neither may leave the completeness gate armed. | `end_phase.rs`, `turns.rs:269-296` |
| R4 | **Readers of `entry.source_id` / `entry.controller`** that assume a real object or a controlling player: display, `stack_object_controller`, and incarnation capture in `push_to_stack_with_firing` (`stack.rs:131`; Activated/Triggered only, so this kind is skipped). | grep `\.source_id` / `\.controller` over stack readers |
| R5 | **The chosen-source shield (D6)** matches by id and incarnation for an LKI-only source. | `effects/prevent_damage.rs`, `effects/create_damage_replacement.rs` |
| R6 | **`DamageResult::NeedsChoice => 0`** (`combat_damage.rs:1641`) cites "no player gets priority between combat damage being assigned and dealt". A replacement choice during resolution is still not priority, so the behavior stays. Reword the comment so it no longer rests on the modern-only premise. | `combat_damage.rs:1637-1642` |
| R7 | **Auto-pass and phase stops:** a new priority window per sub-step. `AutoPassMode::UntilStackEmpty` baselines count entries; yield scopes (`game_state.rs:25101`). | `priority.rs`, `tests/integration/issue_1969_combat_damage_auto_pass.rs` |
| R8 | **Observer triggers on LKI sources (D8).** `valid_source_matches` → `target_filter_matches_object` evaluates the live object. Find the existing LKI-aware source-matching authority before adding one. | `trigger_matchers.rs:1339-1346`, `filter.rs:3865` |

## 8. CR annotation convention for this axis

Current-CR annotations stay mandatory and grep-verified (the
`validate-cr-annotations` skill). Historical rules are **not** current CR
numbers, so:

- **Annotate the current rule the code departs from,** and name the historical
  source in prose. Example: `// CR 510.2: modern combat damage doesn't use the
  stack; under CombatDamageTiming::OnStack the pre-M10 rule (1999–2009 CR,
  combat damage step) puts it on the stack instead.` Precedent:
  `game/mana_burn.rs:1-19` cites the current glossary entry "Mana Burn
  (Obsolete)".
- **Current rules, each used only for what it says.** All were grep-verified
  2026-09-15:

  | Rule | What it's cited for |
  |---|---|
  | CR 510.2 | simultaneity |
  | CR 500.6 | step-start triggers wait for priority |
  | CR 704.3 | SBAs before priority |
  | CR 609.7a | choosing a source |
  | CR 616.1 | only where an actual replacement-ordering choice is prompted |
  | CR 702.4b / 702.7b | first/double strike participation |
  | CR 702.2c + 702.19b | deathtouch trample |
  | CR 702.15b | lifelink |
  | CR 724.1b / 724.2b | stack exile |
  | CR 800.4a | leaving player's stack objects cease to exist |
  | CR 400.7 | new object on zone change |
  | CR 120.3 | damage results |

- **CR 117.4 / 405.5 speak only of spells and abilities.** Don't cite them as
  though they governed this object; cite the engine invariant (§3.4).
- **Never write `CR 310.x`** (current 310 = Battles), `CR 502.x`, `CR 419.x`
  or `CR 600.x` for historical rules.

## 9. Implementation phases

Each phase is its own PR, through plan → plan review → implement → impl
review, like 1a–2cd. `OnStack` stays **unreachable from any selectable
format** until Phase 3c: `LegacyAxis::CombatDamageTiming` stays out of
`IMPLEMENTED_LEGACY_AXES` (`types/custom_format.rs:698`). Phases 3a–3b are
exercised only through `FormatConfig::for_custom_rules` in tests
(`legend_scope.rs:78-89` pattern).

### Phase 3a — Stack object and classification (no behavior)

- **Types:** `StackEntryKind::CombatDamage`, `AssignedCombatDamage`,
  `AssignedDamageRecipient`.
- **Classification:** `StackObjectClass` (D5), including the no-controller
  answer (§3.1), plus every §3.6 decision.
- **Display:** derived-view label and lines; TS union; `StackEntry.tsx` label
  path.
- **Protocol:** bump for the variant (§4).
- **Doc fix:** the `CombatDamageTiming` doc comment's "pre-6th-edition".
- **Tests** (by construction; nothing pushes the object yet). Each rejection is
  paired with an accepted control:
  1. A hand-pushed entry is not a legal target for "target spell", "target
     activated ability", "target triggered ability" or "target spell or
     ability". Control: the same filters against a real spell or ability.
  2. Eliminating the active player leaves the entry on the stack. Control: that
     player's own triggered ability is removed.
  3. Serde round trip.
  4. The engine provides `kind_label`.

### Phase 3b — Engine behavior

- **Seam:** exhaustive `match` (§3.2), including step-start triggers and SBAs
  at push, the decision-maker seat, and the D3 guard.
- **Resolution:** resolver arm and `resolve_combat_damage_object`; the
  `ResolvedCombatDamage` input to `apply_combat_damage` on both paths (§3.3).
- **Lifelink:** D7 batch origin.
- **LKI:** `DamageContext::from_lki` over `lki_by_incarnation`;
  `LKISnapshot.is_commander`; 310.4c recipient validation (§3.4).
- **Triggers and sources:** D8 source identity in damage events; the D6
  widening.
- **Plumbing:** the journaled push (§3.10); `combat_damage_timing.rs` with its
  caller.
- **Risks:** R1–R8 traced and resolved in the phase plan.
- **Test shape:** each test asserts `OnStack` behavior **and** a `Modern`
  control on the same board, and is proven sharp by mutating the fix and
  pasting the failure.

**Tests:**
1. *Window exists:* after assignment, Priority with exactly one `CombatDamage`
   entry. Modern control: Priority with damage already dealt and no entry.
2. *Source sacrificed:* damage dealt in full, with the LKI controller.
3. *LKI keywords by incarnation:* a lifelink source sacrificed with damage on
   the stack still gains life; a deathtouch source bounced still destroys its
   blocker. **Plus the identity case:** the source is bounced, recast and
   leaves again in the same step; the damage uses the *first* incarnation's
   snapshot.
4. *Current characteristics:* lifelink granted after assignment gains life.
5. *Power change after assignment:* the amount is unchanged.
6. *Recipients:* bounced or flickered → not dealt, no prevention event.
   Removed from combat but still on the battlefield → dealt. No longer a
   creature → not dealt.
7. *Commander damage from an LKI source:* counted.
8. *Prevention after assignment:* applies, including a
   Circle-of-Protection-class shield choosing an already-sacrificed source
   (D6). **Control:** a different incarnation with the same id is not
   shielded.
9. *First strike:* two objects, a priority window after each, no regular
   damage before the first-strike object resolves; 502.2c and 502.28d cases.
10. *First strike + lifelink + two life-gain replacements (the D7 case):* after
    answering the first-strike object's replacement choice, a Priority window
    exists before the second object is pushed. Control: under Modern the
    resume still flows straight into the regular sub-step.
11. *Beginning-of-combat-damage-step trigger (CR 500.6):* it goes on the stack
    above the damage object and resolves before damage is dealt.
12. *CR 724.1 end the turn and CR 724.2 end combat* with the object on the
    stack: no damage, clean teardown, the gate doesn't re-fire.
13. *Observer trigger on a sacrificed source (D8):* "whenever a creature you
    control deals combat damage to a player" fires **exactly once** (also
    R1's probe).
14. *Journal replay:* a game that pushes and resolves a combat damage object
    replays to an identical state.
15. *Multiplayer:*
    - two defenders, APNAP priority with the object on the stack;
    - a recipient player eliminated before resolution (their assignments are
      dropped, others dealt);
    - **the active player** eliminated before resolution: the object remains
      on the stack. Damage assigned *to* the departed player's creatures is
      dropped (recipients gone). Damage assigned *by* them follows whatever Q5
      decides, and the test pins that decision.
16. *Counter/Stifle-class* effects have no legal target (reachable-flow version
    of 3a's test).

New tests go in `crates/engine/tests/integration/` (not a top-level test
binary), next to `mana_burn.rs` and `wish_outside_game_scope.rs`.

### Phase 3c — Release: gate, presets, client and AI polish

- **Gate:** add `LegacyAxis::CombatDamageTiming` to `IMPLEMENTED_LEGACY_AXES`,
  and re-check every reachability claim against the widened gate.
- **Presets:** **write** `middle_school()` and `classic_magic()`. **Neither
  constructor exists at the pin.** Use PLAN.md §2 and RESEARCH.md §1's
  sourced lists; verify set codes against `set_catalog`; assert rosters by
  name.
- **Classic Magic gate:** `classic_magic()` additionally needs the
  Time Vault / Illusionary Mask card-text overrides (§1.4) resolved or
  explicitly disclosed before it may register. If they are not resolved,
  Middle School registers alone and Classic stays withheld with a documented
  reason, the same shape as `swedish_old_school()`.
- **AI and client:** the AI arms from §6; `cargo ai-gate` once, no refresh;
  `StackTargetArcs` assignment lines.
- **Docs:** update `README.md` and `IMPLEMENTATION_PLAN.md` status.

**Size.**
- **3a:** medium, compiler-guided plus a grep-driven audit (~87 files).
- **3b:** large, where the rules risk lives; D7, D8, R1 and R2 are the likeliest
  multi-round items.
- **3c:** medium, mostly data and tests, plus the Classic card-text question.

## 10. Open questions

- **Q1 — empty assignment.**
  - The design pushes no object when no creature assigns damage (§3.2).
  - Read literally, 2009 rules point the other way: 310.2a says a creature
    with power less than 0 still "assign[s] 0 combat damage", so 310.1's
    single object would exist.
  - But 419.5a says "If a source would deal 0 damage, it does not deal damage
    at all", so that object would do nothing.
  - No pre-M10 ruling was found; the one forum thread located is post-M10.
  - The only observable difference is whether a priority window opens with an
    inert object on the stack. No assignment triggers exist to see it.
  - **Recommendation:** keep "no object" as a documented simplification.
    Revisit only if a period ruling surfaces.
- **Q2 — resolved.** No controller (§3.1).
- **Q3 — scope beyond the presets.** CONTEXT.md's "Lost Legacy 606" also lists
  "Damage uses the stack". It is expressible with this axis, but it is not a
  bundled preset.
- **Q5 — damage from a departed player's sources.** When a player leaves a
  multiplayer game with damage on the stack, their creatures leave the game
  (CR 800.4a). 310.4a/b would deal their assigned damage anyway, using
  last-known information. But the 2009 rules predate the current CR 800.4a
  framing, and no multiplayer ruling was found. Options:
  - (a) deal it, using LKI (literal 310.4a);
  - (b) drop it, treating a departed player's sources as gone from the game,
    not merely out of play.

  Recommendation: **(a)**. It is the literal rule and needs no special case.
  Confirm during Phase 3b plan review.
- **Q4 — Classic Magic fidelity boundary.** Is D0 plus disclosed card-text
  overrides acceptable for `classic_magic()`, or does the "Sixth Edition
  rules" framing require more (for example triggered deathtouch/lifelink)?
  This is a product decision for the user and maintainers, not a code
  question. It gates only Classic, not Middle School.

## 11. Review log

**Architecture review (verdict REVISE):**

| Finding | Change |
|---|---|
| H1 lifelink resume skips the post-resolution priority window | D7 typed batch origin; test 10 |
| H2 CR 500.6 step-start triggers moved after the window | step-start triggers and SBAs at push; `include_phase_event` becomes a caller parameter; test 11 |
| H3 an `Option<DamageContext>` override doesn't cover commander, recipients or event source ids; `LKISnapshot` lacks `is_commander` | one `ResolvedCombatDamage` input on both paths; add `LKISnapshot.is_commander`; test 7 |
| H4 LKI read by id defeats D2 | `lki_by_incarnation` everywhere (§3.4, D6); identity test in 3 and control in 8 |
| M1 CR 724.2 Mandate of Peace | R3, test 12 |
| M2 elimination removes the object by controller | no controller (§3.1); 3a test 2; test 15 |
| M3 D5 rationale and site count | §3.5 rationale corrected; §3.6 grep rule |
| M4 journaling the turn-based push | §3.10; test 14 |
| M5 R1 evidence covered only the paused path | R1 rewritten; probe in test 13 |
| M6 observer triggers on LKI sources | D8, R8, test 13 |
| L1 dead-code policy module in 3a | moved to 3b |
| L2 empty first-strike sub-step | noted in §3.2 |
| L3 priority seat | `turn_decision_maker` on both paths |
| L4 stale cites | fixed |

**Rules fact-check:**

| Finding | Change |
|---|---|
| Classic Magic is not framed as "modern + exceptions" | §1.4 rewritten; D0 is a stated project decision; Q4; 3c Classic gate |
| D0 claimed no assignment conflict | §1.4 departures table (deathtouch/trample, lifelink, first-strike literal reading) |
| Sixth Edition release date | 1999-04-21 |
| Active player as controller is a CR 800.4a hazard | no controller (§3.1) |
| Q1 unsupported | Q1 rewritten with 310.2a / 419.5a |
| CR 616.1 and CR 117.4/405.5 over-cited | §8 table; §3.4 invariant wording |
| First-strike literal reading | §1.4 table, §3.8 |

---

*Research inputs:*
- the archived CR texts and EC pages in §1.1 (fetched 2026-09-15);
- Scryfall API queries (§1.4);
- traces of `combat_damage.rs`, `combat.rs`, `stack.rs`, `priority.rs`,
  `turns.rs`, `elimination.rs`, `choose_damage_source.rs`, `end_phase.rs`,
  `trigger_matchers.rs` and `types/custom_format.rs` at `05c27e0d5`.
