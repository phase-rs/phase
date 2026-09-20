# `CombatDamageTiming::OnStack` — Design

**Status:** design only, no code. This is the dedicated design pass that
`PLAN.md` §6/§7/§8 and `IMPLEMENTATION_PLAN.md` require before this axis is
built. It is the last `LegacyRuleSet` axis, and it gates the two remaining
Eternal Central presets, `middle_school()` and `classic_magic()`.

**Revision 5** (2026-09-15; Q4 decision and maintainer review on phase-rs/phase#8898 applied). Final revision after three review rounds, each
pairing an architecture review with an adversarial fact-check of the rules
premises. The round-3 verdict was **APPROVE WITH CHANGES**; this revision
applies all of those changes. §11 records what every finding changed.

**As of:** `upstream/main` @ `05c27e0d5`. Every `file:line` below is pinned to
that commit (paths relative to `crates/engine/src/` unless stated). Re-verify
before implementing: `combat_damage.rs` and `stack.rs` move quickly.

---

## 0. Summary

**The rule.** Before M10, combat damage was **assigned**, put on the stack as
**one object per combat damage step**, and **dealt when that object resolved**,
after a full priority round. That object was neither a spell nor an ability,
so it could not be countered or targeted, and a player leaving the game did not
affect it.

**The seam.** The engine already separates assignment from dealing internally
(`CombatState.pending_damage`). This design inserts a stack object and a
priority round at that seam. Assignment, the batch-replacement pipeline,
lifelink batching and the trigger/SBA loop are reused, not duplicated.

**New surface:**
- one `StackEntryKind::CombatDamage` variant;
- an exhaustive spell / ability / combat-damage classification;
- a stack-entry controller carried by that classification (none for combat
  damage);
- **incarnation-aware damage-source identity** through the whole damage
  pipeline (protection, prevention filters, events, trigger matchers);
- a dealing-time resolver for 310.4a–c;
- a typed origin on the parked lifelink batch;
- step-start trigger handling after the push;
- a widened CR 609.7a "choose a source";
- a policy module like `game::mana_burn`.

**Plan.** Four implementation phases (§9). The first two are no-behavior
groundwork. The gate flips, and **Middle School** registers, only in the last.
**Classic Magic is deferred** to a separate follow-up (decided 2026-09-15, Q4).

**Corrections to the charter:**
1. `PLAN.md:644`, `RESEARCH.md:331` and the `CombatDamageTiming` doc comment
   (`types/custom_format.rs:79-84`) call this rule "pre-6th-edition". That is
   backwards. Damage on the stack was **introduced** by the Classic Sixth
   Edition rules (1999) and **removed** by M10 (July 2009); see §1.3.
2. Classic Magic's page opens by saying it "uses Sixth Edition rules". Its
   operative ruleset, though, is the enumerated "Notable Rules/Differences
   from Modern Era of Magic" list (§1.4), which asks for nothing this design
   doesn't provide except two card-text overrides.

Phase 3a fixes the doc comment. The merged design documents stay as reviewed,
per `IMPLEMENTATION_PLAN.md`'s convention, and this document records the
corrections.

---

## 1. The rule being implemented

### 1.1 Sources

The current CR (`docs/MagicCompRules.txt`) has no damage-on-the-stack rule. The
historical text below comes from archived Comprehensive Rules files. Petr
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
not 510 (510 was then "Status"). Everything cited below as `310.x`, `408.x`,
`419.x`, `420.x`, `502.x` or `600.x` is a *historical* number. **Such numbers
must never appear in engine CR annotations**, because the annotation gate
checks against the current CR, where 310 is Battles. §8 gives the convention to
use instead.

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
> **600.4a** […] all objects (see rule 200.8) owned by that player leave the
> game, all spells and abilities controlled by that player on the stack cease
> to exist, […] **A player leaving the game doesn't affect combat damage on the
> stack.**
>
> Glossary, **Removed from Combat:** […] if combat damage assigned to or by that
> permanent is already on the stack, it will resolve normally.

The M10 CR's 510.2 confirms the change: "This turn-based action doesn't use the
stack. No player has the chance to cast spells or activate abilities between
the time combat damage is assigned and the time it's dealt. **This is a change
from previous rules.**"

### 1.3 Era: Sixth Edition through M10

**The 1999 CR already has the full model** (310.2–310.4, 408.1g): "All
announcements of combat damage go on the stack as a single entry"; it "can't be
countered"; it is "dealt as originally assigned", with the source "as it
currently exists, or as it most recently existed".

**By 2009 the model is materially the same.** 2009 adds:
- the word "object";
- triggers on assignment;
- double strike;
- planeswalker recipients;
- 310.4c's "is neither a creature nor planeswalker" clause;
- an explicit priority pass after resolution.

**Before Sixth Edition** (Fourth/Fifth Edition) there was no stack at all:
batches, interrupts, and a damage-prevention step. **Nothing in this project
targets that model.** The Old School 93/94 and 95 presets use modern rules plus
mana burn (RESEARCH.md §1).

**Dates:**
- Sixth Edition released **1999-04-21** (Scryfall `sets/6ed`; MTG Wiki). The
  earliest archived CR is dated 1999-04-23.
- M10's rules took effect **2009-07-11**.

### 1.4 What each preset actually says, and design decision D0

The two Eternal Central pages frame their rules differently.

- **Middle School:**
  - "Damage Uses the Stack (as it did with Sixth Edition rules for example, so
    combat tricks such as Morphling, Triskelion, and Mogg Fanatic work)".
  - "**Everything else in Middle School works the same as modern Magic rules**
    (current London Mulligan rule, etc.)".
  - Its C.1.Ruling.3 gives the order: "6. Assign combat damage (but don't deal
    it yet) 7. Chance for instants and abilities. 8. Deal combat damage.
    9. Triggers on damage being dealt".
- **Classic Magic:**
  - "DAMAGE USES THE STACK (as in, combat tricks such as Morphling, Triskelion,
    and Mogg Fanatic work)". The format's description says it "**uses Sixth
    Edition rules** (including stacking damage, and mana burn)".
  - It has no "everything else is modern" sentence, although its list is headed
    "Notable Rules/Differences from Modern Era of Magic", so the page mixes both
    framings.
  - It prints card-text overrides. Time Vault: "Play this ability if only [sic]
    there's a time counter on Time Vault". Illusionary Mask: "Note: this is
    similar to how Illusionary Mask worked in a previous rules update, prior to
    the modern wording."

**Design decision D0: `OnStack` changes only combat damage *timing* and
dealing-time source/recipient identity. Everything else follows the current CR
and current Oracle text.**
- **Middle School:** this is exactly what the page requires ("Everything else
  … works the same as modern Magic rules").
- **Classic Magic:** this also matches what the page requires. "Uses Sixth
  Edition rules (including stacking damage, and mana burn)" is its
  introduction. The operative list, "Notable Rules/Differences from Modern Era
  of Magic", has exactly four items:
  1. mana burn (`ManaBurnPolicy`, shipped);
  2. damage uses the stack (this axis);
  3. pre-M10 Wish (`WishOutsideGameScope`, shipped);
  4. updated text for Time Vault and Illusionary Mask.

  It asks for no other 2009 rule, so the table below lists differences from
  2009 play, not gaps in either preset's ruleset.

**Known departures from 2009 behavior that D0 accepts:**

| 2009 rule | Current rule D0 uses | Observable difference |
|---|---|---|
| 502.63a "Deathtouch is a triggered ability"; 502.63b multiple instances "each triggers separately"; 502.9d trample "considers only the actual toughness of a blocking creature" | CR 702.2b (destruction by SBA), CR 702.2c + CR 702.19b (1 is lethal for trample assignment), CR 702.2f (redundant instances) | (1) A deathtouch trampler assigns 1 per blocker, not toughness. (2) Deathtouch destruction can no longer be responded to or Stifled. (3) Extra instances do nothing. |
| 502.68a "Lifelink is a triggered ability"; 502.68b "each triggers separately"; 420.3 + 408.1f (SBAs are checked before triggers go on the stack) | CR 702.15b (life gain is part of the damage event), CR 702.15f (redundant) | (1) **A player brought to 0 by combat damage can be kept alive by their own lifelink.** In 2009 they lost to the SBA before the trigger resolved. (2) No trigger to respond to. (3) Two instances no longer gain double life. |
| 310.5 read literally: "any attackers and blockers that didn't assign combat damage in the first step" | CR 702.7b: participation fixed "as the first combat damage step began" | A first striker whose blocker left (so it assigned nothing) doesn't assign again in the second step. |
| Division among multiple blockers: 310.2c/d "divided as its controller chooses" | CR 510.1c/d "divided as its controller chooses among them" | None. |
| 310.2a: "Creatures with power less than 0 assign 0 combat damage" (they still assign) | CR 510.1a: creatures that would assign 0 or less "don't assign combat damage at all" | **`OnStack` follows 2009 here**, because the assignment is part of the observable stack object (§3.2). Dealing is identical: a 0-amount assignment deals no damage (current CR 120.8 / 2009 419.5a). |

**Classic Magic card-text overrides** (Time Vault, Illusionary Mask) are the
one remaining gap for Classic, and they are **outside this axis**: they need
format-scoped card text, not combat changes. **Decision (2026-09-15): Classic
Magic is deferred.** `classic_magic()` is not written or registered in this
sub-project. It follows as a separate PR that adds those two overrides, under
PLAN.md §7's no-caveated-exposure rule. Middle School has no such overrides and
registers in Phase 3d.

**Triggers on damage being assigned** (310.1) are out of scope, because no
current Oracle text has one. Scryfall `o:"is assigned combat damage"` returns
zero cards. `o:"assigns combat damage"` with when/whenever returns five static
"assigns combat damage equal to its toughness" or "as though it weren't
blocked" cards (2026-09-15).

---

## 2. How combat damage works today

| Concern | Location | Note |
|---|---|---|
| Step entry | `game/turns.rs:3554` (`Phase::CombatDamage` arm of `auto_advance_once`) | `flush_layers`, then `resolve_combat_damage`; `Some(wf)` means wait, otherwise Priority. |
| Main resolver (re-entrant) | `game/combat_damage.rs:107` `resolve_combat_damage` | Lifelink-resume guard (`:115-136`), `regular_damage_done` guard (`:141`), first-strike snapshot (`:145-153`). |
| First-strike sub-step | `combat_damage.rs:157-190` | `collect_damage_assignments` → **`take_pending_damage` → `apply_combat_damage`** (`:162-163`). |
| Regular sub-step | `combat_damage.rs:193-218` | Same pair at `:197-198`. |
| Sub-step bookkeeping | `combat_damage.rs:226` `finish_combat_damage_sub_step` | Sets flags. Computes `include_phase_event` (CR 500.6 step-start marker, `:239-247`) and runs `process_combat_damage_triggers` (`:46`). After first strike it returns Priority **only if the stack is non-empty**; otherwise the regular sub-step runs inline with no window between them. |
| Lifelink park/resume | `combat_damage.rs:332` `resume_pending_combat_lifelink` | After finishing a parked batch it **re-enters `resolve_combat_damage`** (`:377`). Three entry points: `engine_replacement.rs:197`, the guard at `combat_damage.rs:135`, and the elimination path (`turns.rs:3299`). |
| Assignment collection | `combat_damage.rs:745` `collect_damage_assignments` | Resumable via `damage_step_index`; `WaitingFor::AssignCombatDamage` / `AssignBlockerDamage`; handlers at `game/engine_combat.rs:528,750`. |
| Assigned-but-not-dealt data | `game/combat.rs:299` `CombatState.pending_damage: Vec<(ObjectId, DamageAssignment)>` | `DamageAssignment { target: DamageTarget, amount: u32 }` (`combat.rs:392-402`). |
| Dealing | `combat_damage.rs:1535` `apply_combat_damage` | **Phase A:** gate (`effects/deal_damage.rs:445`); object protection reads `state.objects.get(&ctx.source_id)` (`:484-488`), player protection uses `player_protection_from(Some(ctx.source_id))` (`:504-507`). **Phase B:** `replace_combat_damage_batch`; `damage_source_filter` is `matches_target_filter(state, source_id, …)` (`replacement.rs:7298-7307`). **Phase C:** apply, commander damage (`source_is_commander` read live, `:1571-1575`), per-source lifelink. |
| Source context | `effects/deal_damage.rs:308` `DamageContext::from_source` | Live `state.objects[id]`, records `source_incarnation`, **no zone check**. `fallback` (`:355`) keeps only the controller. |
| Events and matchers | `GameEvent::DamageDealt`, `CombatDamageDealtToPlayer.source_amounts` | Sources carried as bare ids. Matchers evaluate them live (`trigger_matchers.rs:1339-1346` → `valid_source_matches` → `target_filter_matches_object`). |
| Chosen source | `effects/choose_damage_source.rs:44` → `Vec<ObjectId>`; `create_damage_replacement.rs:184` | Stored as `TargetFilter::SpecificObject { id }` (`types/ability.rs:7017`), with **no incarnation**. |
| LKI | `types/game_state.rs:19856` `lki_cache` (by id); `:19872` `lki_by_incarnation` | Written together in the zone-move path (`zones.rs:268-273`). Both clear on step transition (`turns.rs:1125-1127`). `LKISnapshot` (`:372`) has `keywords`, `colors`, `card_types`, `controller`, but **no `is_commander`**. |
| Stack controller | `StackEntry.controller: PlayerId` (`game_state.rs:16241`) | Read directly (e.g. `filter.rs:2998,3019`); only a few readers use `stack_object_controller` (`stack.rs:1033`). |
| Completeness gate | `game/priority.rs:81-91` | Empty stack + all pass + `CombatDamage && !regular_damage_done` → re-enter `auto_advance`. |
| Elimination | `game/elimination.rs:970-981` | Removes stack entries whose controller is the leaver. No LKI capture of its own. |

**The seam:** in both sub-steps `take_pending_damage` is immediately followed
by `apply_combat_damage`. Everything before that pair is *assignment*;
everything from `apply_combat_damage` onward is *dealing*.

**Legacy-axis pattern to follow:** `game/mana_burn.rs:31-42`,
`game/legend_scope.rs:46,61`.

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
source would open a priority window between two creatures' damage. It would
also break the CR 510.2 simultaneity that `replace_combat_damage_batch` and
per-source lifelink batching depend on.

**D2 — carry incarnations, not bare ids.** 310.4b and 310.4c turn on whether a
source or recipient is *still the same object*. The engine keeps an `ObjectId`
across zone changes and bumps an incarnation (CR 400.7).
`ObjectIncarnationRef` is the established identity
(`attacking_incarnations_this_combat`, `lki_by_incarnation`). D2 is only
meaningful if **every** downstream source read honors the incarnation; §3.11
makes that a single design decision.

**D9 — the stack entry controller is carried by the classification, not a
public field.**
- **The rule.** 2009 600.4a: "A player leaving the game doesn't affect combat
  damage on the stack." Current CR 800.4a makes stack objects not represented
  by cards that are controlled by a leaving player cease to exist. An
  engine-assigned controller (e.g. the active player) would therefore erase
  every pending assignment, the defenders' included, when that player left a
  multiplayer game. So the object has **no controller**.
- **Why not a lookup rule.** The controller is read directly in many places
  (`filter.rs:2998,3019` and more). Only the four non-test callers of
  `stack_object_controller` (`ability_utils.rs:784`, `casting.rs:709`,
  `elimination.rs:974`, `stack.rs:1469`) go through a helper. A convention
  can't be enforced.
- **Why not a bare `Option<PlayerId>`.** It would enumerate readers, but it
  invites `.expect(..)` / `unwrap_or(active_player)` at sites that only ever see
  spells and abilities. That fallback silently reintroduces the CR 800.4a
  hazard.
- **The change:**
  - `StackEntry.controller` becomes private, stored as `Option<PlayerId>`.
  - The only public reader is D5's projection, which carries the controller
    where one exists: `StackObjectClass::Spell { controller }`,
    `Ability { kind, controller }`, `CombatDamage` (none).
  - Readers that need a controller `match` the class, so `None` is visible
    only in the `CombatDamage` arm. Every existing constructor passes a
    controller; `CombatDamage` doesn't.
- **Serialization.** Old saved states and replays deserialize unchanged: serde
  reads an old bare `PlayerId` straight into `Option<PlayerId>` as `Some`. Only
  `None` payloads are new, and the protocol bump (§4) covers old clients.
- **Elimination** (`elimination.rs:970-981`) then never removes a `CombatDamage`
  entry, matching 600.4a.

**Entry `id` / `source_id`.**
- `id`: a fresh `ObjectId(next_object_id++)` with no `GameObject`, like
  `push_keyword_action` (`engine.rs:17196`).
- `source_id`: the entry's own id. There is no single source, and `ObjectId(0)`
  is the monarch sentinel (`triggers.rs:5968`).
- Readers of `entry.source_id` are audited in Phase 3a (R4).

**Rejected alternatives:**

| Alternative | Why rejected |
|---|---|
| A synthesized `TriggeredAbility` | A legal Stifle target and counterable, which violates 310.3. The charter's earlier draft made this mistake (CONTEXT.md:537). |
| A `KeywordAction` variant | Classified as an *activated ability* (`game_state.rs:16943`); wrong layer. |
| No stack object; a special "damage window" priority state | Responses must resolve *above* pending damage, so every stack-based path would need to know about an invisible pseudo-object. |

### 3.2 Pushing: the seam in `resolve_combat_damage`

```text
collect_damage_assignments(state, sub_step)  -> Some(wf) => return Some(wf)   // unchanged
match combat_damage_timing::policy(state):
  Modern  => take_pending_damage; apply_combat_damage; finish_…               // unchanged
  OnStack =>
    let pending = take_pending_damage(state);
    reset per-sub-step collection state (damage_step_index, damage_assignments)
    // ALWAYS exactly one object per combat damage step, even if every entry is 0
    // or the list is empty (2009 310.1: "All assignments … go on the stack as a single object")
    push_combat_damage_object(state, sub_step, freeze(pending), events)        // journaled, §3.10
    // AFTER the push, so abilities land ABOVE the object and D3 blocks re-entry
    collect_step_start_triggers_and_run_sbas(state, sub_step, events)           // CR 500.6 + CR 704.3
    if let Some(wf) = pending_combat_damage_waiting(state) { return Some(wf) } // OrderTriggers, targets, …
    reset_priority(state)
    return Some(WaitingFor::Priority { player: turn_decision_maker(state) })
```

- **Exhaustive `match` on `CombatDamageTiming`,** never `if uses_stack`.
- **`freeze(pending)`** records each source's and recipient's current
  incarnation.

**Order: push, then step-start triggers.**
- Step-start triggers go on the stack the next time a player would receive
  priority: 2009 408.1f ("each time a player would receive priority") and
  current CR 500.6. That moment is after the push.
- Trigger processing pushes immediately (`triggers.rs:12993`), so running it
  *before* the push would place those abilities **below** the object and
  resolve them after damage.
- Collected after the push, they sit above it and resolve first.
- **A prompt raised here** (e.g. `OrderTriggers` for two same-controller
  triggers, or a target choice) is returned before Priority is granted.
  Re-entry after the answer can't re-collect or re-emit the marker: an object
  is always on the stack by then, so the D3 guard blocks it.

**Step-start marker.** Today it is synthesized at *dealing* time, inside
`process_combat_damage_triggers`, gated by `include_phase_event`
(`combat_damage.rs:239-247`, `:56-60`).
- `include_phase_event` becomes a **parameter supplied by the caller**, computed
  by **one shared helper** (today's `combat_damage.rs:239-247` logic). The
  inline Modern path, the `OnStack` push and D7's `TurnBasedAction` resume all
  call it, so the computations can't drift.
- The `OnStack` push emits it with the same per-sub-step rule the engine uses
  today: the first-strike sub-step always, the regular sub-step only when it is
  the sole sub-step.
- The `OnStack` resolver passes `false` (§3.3). The lifelink resume derives it
  from the batch origin (D7).
- A literal CR 500.6 reading could fire such triggers again when the second
  combat damage step begins. No current card can tell the difference (Scryfall
  `o:"beginning of the combat damage step"` returns 0 cards), so this keeps
  today's behavior.

**Every combat damage step puts exactly one object on the stack**, including
an inert one. This follows the maintainer review on #8898.
- **Why.** 2009 310.1 puts "All assignments of combat damage" on the stack "as a
  single object" as the step begins, and 310.4 resolves it and removes it. A
  creature with power 0 or less still *assigns* (310.2a: "assign 0 combat
  damage"), so the historical object exists and is observable. Skipping it
  would omit a stack state the rule creates.
- **Zero-amount assignments are recorded under `OnStack`.**
  - `collect_damage_assignments` today skips a creature whose
    `combat_damage_amount` is 0 (`combat_damage.rs:768`, `:938`), per current
    CR 510.1a.
  - That skip becomes an exhaustive `match` on the policy:
    - `Modern` keeps skipping;
    - `OnStack` records a 0-amount entry to the creature's recipient(s), per
      310.2a.
  - A 0 amount has exactly one division, so no `AssignCombatDamage` prompt is
    raised for it.
  - A creature that "assigns no combat damage" by an effect
    (`assigns_no_combat_damage`), or that has no legal recipient
    (310.2b–d: "will assign no combat damage"), makes no entry.
- **An empty assignment list still pushes the object.** For example, every
  attacker's blockers were removed and nothing tramples. The object resolves
  with nothing to deal.
- **Resolution of 0 entries:** no damage is dealt, so there are no
  `DamageDealt` events, no damage triggers and no prevention events (current
  CR 120.8; 2009 419.5a). The Phase A gate already drops 0-amount damage.
- **Consequences:**
  - there is no special empty branch;
  - an empty *first-strike* step gets its object, its resolution and its
    priority window like any other step, as 2009 310.4/310.5 and current
    CR 510.3/510.4 require;
  - D3 always has an object to guard on.
- **Pre-existing modern-path gap, out of scope:** `finish_combat_damage_sub_step`
  gives no window between sub-steps when the stack is empty. That departs from
  current CR 510.3 under Modern. It is recorded as a separate issue to file and
  is not changed here.

**Priority seat.** Use `turn_control::turn_decision_maker(state)`
(`turn_control.rs:169`), as step entry does (`turns.rs:1121`). Phase 3c aligns
`finish_combat_damage_sub_step` on it too.

**D3 — "damage is already on the stack" is derived from the stack, not
stored.**
- `resolve_combat_damage` has four callers: the turns arm, the assignment
  handlers, the lifelink resume, and the completeness gate. None of them may
  re-collect assignments for a sub-step whose object is still on the stack.
- The guard is `combat_damage_on_stack(state) -> Option<CombatDamageSubStep>`,
  in the style of `crew_pending_on_stack` (`engine.rs:17043`), placed next to
  the `regular_damage_done` check.
- It is derived rather than stored because a stored flag would drift from the
  stack, e.g. after CR 724.1b or 724.2b exile the stack.
- Re-entry *after* resolution is governed by D7.

**Completion flags.** `first_strike_done` and `regular_damage_done` mean
"damage for this sub-step has been dealt". They are set when the object
resolves (every step has an object).

### 3.3 Resolving: `stack.rs` arm

The template is the `KeywordAction` early return at `game/stack.rs:1372`. The
new arm is a sibling early return before `bind_resolution_scope`:

```text
StackEntryKind::CombatDamage { sub_step, assignments } =>
    combat_damage::resolve_combat_damage_object(state, sub_step, &assignments, events)
    // StackResolved + finish_resolving_stack_entry(Resolved), as KeywordAction does
    // (placement relative to a lifelink park: §7 R2)
```

`resolve_combat_damage_object` does three things.

1. **Resolve every frozen assignment into one typed record** (§3.4):
   ```rust
   struct ResolvedCombatDamage {
       source: DamageSourceRef,     // §3.11 — incarnation-aware identity
       ctx: DamageContext,          // built from that identity: live-if-same-incarnation, else LKI
       source_is_commander: bool,   // same authority as ctx
       target: DamageTarget,        // only recipients that survived 310.4c validation
       amount: u32,
   }
   ```
   Recipients that fail 310.4c are dropped here, before Phase A, so no
   prevention event fires for damage that isn't dealt.
2. **Call `apply_combat_damage(&[ResolvedCombatDamage])`.** Both paths use it.
   The modern path builds the same records from live objects. The function no
   longer reads the source from `state.objects` by id: every source read goes
   through §3.11.
3. **Batch outcome:**
   - `Complete` → `finish_combat_damage_sub_step(…, include_phase_event = false)`.
   - `Paused` → park via `pending_combat_lifelink` **with origin `StackObject`**
     (D7).

**D7 — typed batch origin.** `PendingCombatLifelink` gains
`origin: CombatDamageBatchOrigin { TurnBasedAction, StackObject }`.
`resume_pending_combat_lifelink` matches on it exhaustively:
- **`TurnBasedAction`:** unchanged. It re-enters `resolve_combat_damage` for the
  mandatory next sub-step, with today's `include_phase_event` computation.
- **`StackObject`:** calls `finish_combat_damage_sub_step(…, false)` and returns
  Priority to the decision maker. Re-entering here would push the next object
  from inside the replacement-choice handler and skip 310.4's "After combat
  damage finishes resolving, the active player gets priority". The next
  sub-step starts via the completeness gate instead.

The match lives inside the resume function, so all three entry points
(`engine_replacement.rs:197`, `combat_damage.rs:135`, `turns.rs:3299`) are
covered.

**After resolution:**
- The decision maker gets priority (310.4; engine `priority.rs:154-165`).
- **After the first-strike object:** all players pass with an empty stack → the
  completeness gate → `resolve_combat_damage` collects regular assignments and
  pushes the second object. This is 310.5's second step, with its own window.
- **After the regular object:** `regular_damage_done` is set, so the next
  all-pass advances to end of combat.
- **Departed active player:** giving Priority to the active player follows
  current CR 800.4j ("If the active player would receive priority, instead the
  next player in turn order receives priority"). `priority.rs:154-165` gives it
  to `state.active_player` unconditionally. Test 15 reaches this, so Phase 3c
  traces whether elimination already reseats priority (R10).

**D4 — no counter or target interaction at resolution** (310.3, §3.5). There is
no fizzle check, no `bind_resolution_scope`, and no zone routing.

### 3.4 Dealing semantics (310.4a–c)

These checks live **only** in the record resolver of §3.3 step 1.

| Case | Rule | Behavior |
|---|---|---|
| Source is the frozen incarnation, on the battlefield | 310.4b | `DamageSourceRef::Live`: **current** characteristics. |
| Source left the battlefield, or changed incarnation (sacrificed Mogg Fanatic, bounced Morphling, **or its owner left the game**) | 310.4a + 310.4b + 2009 600.4a last sentence | Dealt with the assigned amount. `DamageSourceRef::Lki`, reading `lki_by_incarnation[id][incarnation]`; `DamageContext::from_lki` reads controller and keywords from that snapshot. |
| Commander flag for an LKI source | current CR 903.10a + CR 704.6c (21 combat damage from the same commander) | Add `LKISnapshot.is_commander` (`#[serde(default)]`, captured with the rest of the snapshot), so every LKI damage source gets it, not only combat. |
| Source power changed after assignment | 310.4a | Amount stays as assigned. |
| Recipient creature, planeswalker or battle: the frozen incarnation, still that type, on the battlefield | 310.4a | Dealt, even if removed from combat. |
| Recipient left, changed incarnation, or no longer a creature, planeswalker or battle (including when its **owner left the game**) | 310.4c | Dropped before Phase A. |
| Recipient player has left the game | design reading of current CR 800.4a; CR 800.4e covers only *assigning* damage to a departed player | Dropped. |
| Protection, prevention or redirection created after assignment | 310.4 + current CR 615 | Applies, evaluated against the incarnation-aware source (§3.11). |

**A player leaving the game** (current CR 800.4a runs in order: objects the
player owns leave the game, and effects giving that player control end; then
objects the player *still* controls are exiled). The three cases differ:

| Object | What happens to it | As a damage source | As a recipient |
|---|---|---|---|
| **Owned** by the leaver | Leaves the game | Dealt from LKI (2009 600.4a, 310.4a/b) | Dropped (310.4c) |
| Controlled through a **control-changing effect** (Control Magic, Threaten) | Returns to its owner and stays on the battlefield as the **same incarnation**; removed from combat (CR 506.4) | **Live** characteristics; the new controller is the controller (e.g. for lifelink) | **Dealt**: still on the battlefield ("has left combat", 310.4a) |
| **Still controlled** after those effects end, not owned (e.g. reanimated from another player's graveyard) | Exiled | Dealt from LKI | Dropped |

**Life gain for a departed controller.** When an LKI source's controller has
left the game, lifelink gains nothing: current CR 702.15b gives the life to the
source's controller, and a player who has left can't gain life. 2009 agrees
(600.4c: a trigger controlled by a departed player isn't put on the stack).
`drain_combat_lifelink` must skip a departed player rather than crediting the
owner or anyone else.

**Battles.** 310.4c predates battles; under D0 current object types apply. Per
§8, annotate the current rules and name 310.4c only in prose.

**LKI lifetime.**
- Both LKI maps clear on step transition. The step can't end while the object
  is on the stack, because the engine advances only on an empty stack. That is
  an engine invariant; current CR 117.4/405.5 speak only of spells and
  abilities.
- A debug assertion requires an off-battlefield source to have an
  incarnation-keyed LKI entry.
- **R9:** the leave-game removal path must reach that capture.

### 3.5 Classification: not a spell, not an ability

`stack_ability_kind()` (`game_state.rs:16943`) returns `Option<StackAbilityKind>`,
where `None` means spell. It has one direct caller, `matches_stack_ability_kind`
(`:16961`). The real exposure is the ~87 non-test `src` files that
pattern-match `StackEntryKind` (§3.6).

**D5 — an exhaustive projection of the kind:**

```rust
pub enum StackObjectClass {
    Spell { controller: PlayerId },
    Ability { kind: StackAbilityKind, controller: PlayerId },
    /// Neither a spell nor an ability, and controlled by no one
    /// (pre-M10 combat damage, 310.3 + 600.4a).
    CombatDamage,
}
```

`matches_stack_ability_kind` becomes `matches!(class, Ability { kind, .. } if …)`. This
parameterizes an existing axis rather than adding a sibling predicate.

**Consequences:**
- **Counterspells and Stifle can't target it.** `effects/counter.rs:141,412` use
  `matches!(Spell)`; `TargetFilter::StackAbility` goes through the class;
  `targeting.rs:2383-2388` is exhaustive.
- **Split second** (CR 702.61a): `stack_has_split_second` (`keywords.rs:97`)
  reads `objects[entry.id]`, and there is no object, so it's safe.
- **Storm** counts casts (`derived_views.rs:2205`), so it's unaffected.
- **Ending the turn / combat** (CR 724.1b / 724.2b): `end_phase.rs:51-62` makes
  non-card entries cease to exist. R3 covers it.

### 3.6 Match sites

**The audit rule for Phase 3a — classification only.** Every `match`,
`matches!`, `if let` and `let … else` on `StackEntryKind`, and every reader of
`StackEntry::ability()` that treats `None` as a particular kind, gets an
explicit decision. This applies across `engine`, `phase-ai`, `server-core`,
`phase-server`, `engine-wasm` and `manabrew-compat`.

**`StackEntry.controller` readers are NOT part of this audit.** They belong to
Phase 3a-II together with the rest of the D9 work, so the two phases keep the
disjoint break sets the split exists to create. 3a does not read, write, or
re-type that field.

```bash
git grep -n "StackEntryKind::" -- 'crates/*/src/**' ':!*tests*'   # 87 files at pin
git grep -n "\.ability()" -- 'crates/*/src/**' ':!*tests*'
# controller readers: enumerated by the compiler once D9 changes the field type
```

**Known decisions** (not exhaustive; the grep and the compiler are
authoritative):

| Site | Decision for `CombatDamage` |
|---|---|
| `game_state.rs:16281,16294` `ability()`/`ability_mut()` | `None` |
| `game_state.rs:16042` `StackResolutionEntryProvenance` | new provenance |
| `game_state.rs:16946` `stack_ability_kind` | replaced by D5 |
| `game_state.rs:25101` yield scopes | waits for it like any entry |
| `stack.rs:1372` / `:1424` | resolver arm / `unreachable!` |
| `stack.rs:5031,5086,5102` | never grouped |
| `resolved_commands.rs:1139-1142` (journaled stack-kind rewrite) | not rewritable; journaled (§3.10) |
| `zones.rs:43` | trace and decide |
| `effects/copy_spell.rs:172,200,856,943` | not a copy source |
| `derived_views.rs:2194,2810-2831,2849,2915` | not a spell; label; assignment lines |
| `triggers.rs:3017-3022` | `None` |
| `analysis/resource.rs:6648,7011,7189,7245`; `ai_support/targeted_exchange.rs:568` | no ability / not a target |
| `phase-ai`: stack_awareness, deck_knowledge, planner, search, tactical_gate, auto_play, `bin/resolve_bench.rs` | §6 |

### 3.7 "Choose a source" (CR 609.7a) — the Circle of Protection class

The most-played interaction the overlay creates is activating Circle of
Protection: Red *after* damage is on the stack, choosing a creature that may
already have been sacrificed.
- **2009 419.8a** allowed exactly this.
- **Current CR 609.7a** already covers it: "any object referred to by an object
  on the stack … (even if that object is no longer in the zone it used to be
  in)".

**D6:**
- `damage_source_options` returns `DamageSourceRef` candidates (§3.11) instead
  of `Vec<ObjectId>`. It chains in every `AssignedCombatDamage.source` of each
  `CombatDamage` entry on the stack.
- Filter matching ("a red source") goes through the §3.11 source-read authority,
  so an LKI source is judged by its frozen incarnation's snapshot.
- The chosen source is stored **with its incarnation** (§3.11 D10), so the
  shield matches that object and no later incarnation.
- D6 isn't gated by the format policy: it is a correct application of current
  CR 609.7a, and it does nothing under Modern.

### 3.8 First strike and double strike

`first_strike_participants` is snapshotted as the step begins
(`combat_damage.rs:145-153`). `deals_in_substep` (`:442`) admits to the regular
sub-step anything not in the snapshot or with live double strike. That yields:
- **502.2c:** removing first strike after its damage is on the stack → no regular
  damage. Granting first strike to a non-snapshot creature → still deals regular
  damage.
- **502.28d:** granting double strike after first-strike damage is on the stack →
  deals regular damage.

The one literal 2009 reading that differs is in the §1.4 table.

### 3.9 Policy module

```rust
// game/combat_damage_timing.rs — mirrors game::mana_burn
pub(crate) fn policy_of(format_config: &FormatConfig) -> CombatDamageTiming { … }
pub(crate) fn policy(state: &GameState) -> CombatDamageTiming { policy_of(&state.format_config) }
```

Callers `match` on the result. The module lands in Phase 3c, alongside its first
caller.

### 3.10 Journaling

Stack mutations go through journaled authorities: removal is by position so a
replay reproduces it (`elimination.rs:965-970`), and trigger claims need a live
journal cause (`combat_damage.rs:411`). `push_combat_damage_object` runs inside
a turn-based action, not a player-action handler. Phase 3c therefore:
- names the journaled push command and its cause, reusing
  `journal_stack_push` (`stack.rs:272`) rather than adding a parallel authority;
- decides `resolved_commands.rs:1139-1142` for this kind;
- adds a replay round-trip test.

### 3.11 Damage-source identity (D10)

**The problem.** D2 freezes source incarnations, but today every stage after
the damage context reads the source **by bare id, live**:
- Phase A protection (`deal_damage.rs:484-488`, `:504-507`);
- Phase B `damage_source_filter` (`replacement.rs:7298-7307`, where
  `ProposedEvent::Damage` carries only `source_id`);
- emitted events (`DamageDealt`, `CombatDamageDealtToPlayer.source_amounts`) and
  their trigger matchers (`trigger_matchers.rs:1339-1346`);
- the chosen source (`SpecificObject { id }`).

Under `OnStack`, a sacrificed red token's damage would bypass protection from
red (the object is gone). A sacrificed card would be judged by its graveyard
characteristics. A Circle of Protection shield couldn't distinguish
incarnations.

**D10 — one identity, one read authority:**

```rust
/// Which object dealt damage, as the damage pipeline must see it.
pub struct DamageSourceRef(pub ObjectIncarnationRef);

/// The single authority for reading a damage source's characteristics.
pub enum DamageSourceView<'a> {
    Live(&'a GameObject),        // same incarnation, still in a public zone it's read from today
    Lki(&'a LKISnapshot),        // lki_by_incarnation[id][incarnation]
}
fn damage_source_view(state: &GameState, src: DamageSourceRef) -> Option<DamageSourceView<'_>>;
```

- **Carry `DamageSourceRef`** through `DamageContext`, `ProposedEvent::Damage`,
  `GameEvent::DamageDealt`, `DamagePrevented` and
  `CombatDamageDealtToPlayer.source_amounts`, in place of bare source ids.
- **Route every source read through `damage_source_view`:**
  - protection (`protection_prevents_from`, `player_protection_from`);
  - replacement `damage_source_filter`;
  - trigger-matcher source filters;
  - `damage_source_options`.

  Filter evaluation over the `Lki` arm uses the existing
  `matches_target_filter_on_lki_snapshot` (`filter.rs:3865`), so no parallel
  matcher is written. Protection needs the snapshot's `colors` / `card_types`,
  which `LKISnapshot` already has.
- **Protection takes a source view, not a filter.** Protection doesn't go
  through `TargetFilter`. `protection_prevents_from(&GameObject, &GameObject)`
  (`keywords.rs:590`), `source_matches_protection_target`,
  `source_matches_protection_filter` and `player_protection_from_object` read
  the source's effective colors, core types, subtypes, controller or owner,
  mana value and P/T from a `&GameObject`.
  - Phase 3b changes those predicates' *source* parameter to a characteristics
    view that both `GameObject` and `LKISnapshot` provide, i.e.
    `DamageSourceView`. `LKISnapshot` already stores all of those fields.
  - Existing targeting, blocking and attach callers pass `Live`.
  - **No second, LKI-only copy of the protection logic.** Current CR 702.16a
    defines the quality; CR 702.16e the damage prevention.
- **Post-replacement source slots.** `TargetFilter::PostReplacementDamageSource`
  and `PostReplacementSourceController` (`targeting.rs:1629-1642`) turn the
  prevented event's source into a live object id or controller. They drive
  "reflect" riders such as "deals that much damage to that source's controller"
  (`rider_reflects_per_event_damage_source`, `replacement.rs:2024`). They also
  go through `damage_source_view`, so a sacrificed token's controller is read
  from its LKI.
- **Token flag.** `matches_target_filter_on_lki_snapshot` reads `is_token` from
  the live object (`filter.rs:3902-3905`). That reads `false` for a token that
  has ceased to exist. Add `LKISnapshot.is_token` next to `is_commander`, and
  have the LKI filter read it.
- **Deliberately NOT incarnation-keyed:**
  - **Commander damage totals.** `CommanderDamageEntry.commander` (`game_state.rs:2453`)
    stays keyed by id: CR 903.10a counts damage "by the same commander over the
    course of the game", which spans zone changes.
  - **Shield-host events.** `DamagePrevented { source_id: rid.source }` emitted
    by prevention riders (`combat_damage.rs:1878,1908`) names the shield's
    *host*, not a damage source.
  - **Excess-damage redirect** (CR 120.4a). `excess_recipient` is always `None`
    on the combat path (`deal_damage.rs:345`).
- **Chosen source with incarnation.** The concrete shape (e.g. parameterizing
  `SpecificObject` with an optional incarnation, or a distinct chosen-source
  filter) **must go through the `add-engine-variant` gate** in Phase 3b's plan,
  because `TargetFilter` is a shared, serialized surface. The requirement is
  fixed here: a shield created for incarnation *n* must not apply to *n+1*.
- **Behavior under Modern: unchanged, by an explicit stamping rule.**
  - **The scale.** D10 touches ~59 `DamageContext::from_source` callers and ~135
    `ProposedEvent::Damage` constructors, most of them noncombat. "The source
    can't change" holds only for combat.
  - **The rule.** Every site stamps **the incarnation of the object currently
    stored under that id**, exactly what `from_source` records today
    (`deal_damage.rs:311`). A missing object keeps today's fallback: the view
    returns `None`.
  - **What it means for noncombat damage.** A dies-trigger's damage whose
    source is now a newer incarnation in the graveyard is still read from that
    graveyard object, as today. No noncombat site starts reading LKI in 3b.
  - **Only `OnStack` stamps a frozen, possibly-departed incarnation.** Widening
    noncombat damage to LKI is a separate, later change with its own review.
- **Legacy payloads.** `DamageSourceRef` replaces bare ids inside persisted
  state (`GameEvent`, `ProposedEvent`, `PendingCombatLifelink.batch_events`).
  Accept legacy bare-id payloads through a compat deserializer, following the
  existing precedent `ObjectIncarnationRef`'s
  `#[serde(from = "ObjectIncarnationRefCompat")]` (`identifiers.rs:146,188`).
  That way an old persisted mid-choice game still loads.
- **Scope beyond combat is intentional.** Noncombat damage from a source that
  left mid-resolution already needs LKI, and this authority serves it, but 3b
  keeps noncombat call sites behavior-identical and doesn't widen them.

---

## 4. Serialization and protocol

Changes by phase:
- **3a:** the new `StackEntryKind` variant and `AssignedCombatDamage` /
  `AssignedDamageRecipient`. **No protocol bump** — see below.
- **3a-II:** `StackEntry.controller: Option<PlayerId>` and
  `StackEntryDisplay.controller`.
- **3b:** `DamageSourceRef` in damage events, `ProposedEvent` and
  `DamageContext`; the chosen-source filter shape; `LKISnapshot.is_commander`.
- **3c:** `PendingCombatLifelink.origin`.

**A phase bumps `lobby-broker` `PROTOCOL_VERSION`** (with its changelog line,
value pins and client mirror, precedent #8870) **when a peer can actually
receive the new shape** — not merely when a type gains a variant.

**Phase 3a is the exception, decided during its implementation and recorded
here so a missing bump reads as a decision rather than an oversight.** Adding
`StackEntryKind::CombatDamage` changes no existing variant's shape, and 3a adds
no push authority, so nothing can ever serialize one. Three supports:
1. **Precedent.** `StackEntryKind::KeywordAction` — the closest analogue, an
   engine-built entry with a typed payload — was added by `df34aa647`
   (2026-04-17) with no protocol bump; the only `PROTOCOL_VERSION` change in
   that file's history is `3bbf2a59e` (2026-06-18), unrelated.
2. **The hazard needs an emitter.** Every variant-driven changelog entry in
   `protocol.rs` (69's "one-way variant contract", 66's unknown-variant error,
   and the file's "no serde default can rescue an unknown variant") bumps
   because a peer can *receive* the tag.
3. **CI does not force it.** `scripts/check-protocol-version.mjs` enforces that
   the Rust and TS constants AGREE, not that they were incremented — stated in
   the v64 changelog entry.

Had 3a bumped, it would have had to move five surfaces — lobby broker,
server-core (whose pinning test embeds the numeral in its *function name*), the
client adapter, the P2P `WIRE_PROTOCOL_VERSION`, and the checker's expectations
— for a tag nothing can emit.

### Wire-emission matrix — each bump assigned exactly once

The rule above ("bump when a peer can receive the shape") decides ownership.
This matrix is the single authority for which phase performs which bump; no
phase section may claim a bump that is not listed here.

| Phase | Serialized shape it introduces | Can a peer receive it in this phase? | Bump |
|---|---|---|---|
| **3a** | `StackEntryKind::CombatDamage`, `AssignedCombatDamage`, `AssignedDamageRecipient` | **No** — no push authority exists, so nothing serializes one | **None** |
| **3a-II** | `StackEntry.controller: Option<PlayerId>`, `StackEntryDisplay.controller` | **Yes** — the controller field is on every existing entry, so every peer receives the changed shape immediately | **Bump #1** |
| **3b** | `DamageSourceRef` in damage events / `ProposedEvent` / `DamageContext`; chosen-source filter; `LKISnapshot.is_commander` + `is_token` | **Yes** — damage events are emitted by every game | **Bump #2** |
| **3c** | `PendingCombatLifelink.origin`; the first pushes of the 3a variant | **Yes** — both | **Bump #3** |
| **3d** | none (gate flip + preset data) | — | **None** |

Each phase performs exactly the bump on its own row, with that bump's changelog
line, value pins and client mirror. A phase whose row says **None** must not
touch `PROTOCOL_VERSION`, and its section says so explicitly.

Rules that hold for every bump above:
- Additive fields take `#[serde(default)]`.
- `LOBBY_PROTOCOL_VERSION` doesn't move.
- `size_of::<StackEntry>() <= 768` (`game_state_size.rs:66`) is re-checked in 3a.

## 5. Frontend — display only

- **Types:** hand-written in `client/src/adapter/types.ts`. Update:
  - the `StackEntryKind` union (`:1855`) (3a);
  - `StackEntry.controller` and `StackEntryDisplay.controller` (3a-II);
  - the damage-event mirrors whose source shape changes (3b). These are consumed
    by animation components (`AnimationOverlay`, `CardSlamAnimation`), which
    must read the id from the new shape and derive nothing.
- **Labels:** `components/stack/StackEntry.tsx:103-166` prefers
  `details?.kind_label`. The engine supplies the label and the assignment lines
  (`derived_views.rs:2806,2915`), and `StackTargetArcs.tsx` renders them.
- **Nothing new to handle:** no new `WaitingFor` or `GameAction`.
- Follow the `add-frontend-component` skill.

## 6. AI

- **Must not break:**
  - `stack_awareness.rs:244` needs an arm (not a counter target; value 0).
  - `deck_knowledge.rs:122`: no source card.
  - Planner, search, tactical_gate and auto_play readers found by §3.6.
  - Controller readers flagged by D9.
- **Not required to exploit.** Tricks with damage on the stack are a follow-up
  policy (`add-ai-feature-policy`).
- **`cargo ai-gate`:** run once in Phase 3d to confirm no change. No baseline
  refresh.

## 7. Risks to trace in the phase plans

| # | Risk | Where |
|---|---|---|
| R1 | **Double trigger collection.** Dealing runs inside `resolve_next`, under a `PassPriority` pipeline that scans `events[event_start..]`. Name the dedup (`triggers.rs:11103` `filter_already_collected_trigger_events_from` and its `engine_priority.rs` callers) and prove it with a probe: a "whenever a creature deals combat damage" trigger fires **exactly once**. | `triggers.rs:11103`, `engine_priority.rs` |
| R2 | **Lifelink pause during resolution.** When `finish_resolving_stack_entry` runs relative to the park; whether `settle_resolving_stack_entry_after_continuation_resume` expects a continuation. | `stack.rs:37,53,1304-1330`; `combat_damage.rs:332` |
| R3 | **CR 724.1 (end the turn) and CR 724.2 (end combat, Mandate of Peace)** with the object on the stack: it ceases to exist, and the completeness gate isn't left armed. | `end_phase.rs:51`; `turns.rs:269-296` |
| R4 | **`entry.source_id` readers** that assume a real object. | grep `\.source_id` over stack readers |
| R5 | **A chosen-source shield** matches by incarnation, via the D10 filter shape. | `prevent_damage.rs`, `create_damage_replacement.rs` |
| R6 | **`DamageResult::NeedsChoice => 0`** (`combat_damage.rs:1641`): the comment rests on a modern-only premise; reword it, behavior unchanged. | `combat_damage.rs:1637-1642` |
| R7 | **Auto-pass and phase stops**: a new priority window per sub-step. | `priority.rs`; `issue_1969_combat_damage_auto_pass.rs` |
| R8 | **Event-shape blast radius** of D10: ~28 non-test files name `DamageDealt`, ~16 name `CombatDamageDealtToPlayer`. | `git grep` |
| R9 | **LKI capture when a player leaves.** `elimination.rs` has no capture of its own. Trace whether leave-game removal goes through the zone-move capture (`zones.rs:268-273`); if not, add it there. | `elimination.rs`, `zones.rs:268-273` |
| R10 | **Priority after a departed active player** (current CR 800.4j) once the object resolves. | `priority.rs:154-165`, `elimination.rs` |

## 8. CR annotation convention for this axis

Current-CR annotations stay mandatory and grep-verified
(`validate-cr-annotations`).

**Historical rules** are annotated by citing the current rule the code departs
from, naming the historical source in prose. Example:
`// CR 510.2: modern combat damage doesn't use the stack; under
CombatDamageTiming::OnStack the pre-M10 rule (1999–2009 CR, combat damage step)
puts it on the stack instead.` Precedent: `game/mana_burn.rs:1-19`.

**Current rules, each used only for what it says** (grep-verified 2026-09-15):

| Rule | What it's cited for |
|---|---|
| 510.2 | Simultaneity |
| 510.3 / 510.4 | Priority after each combat damage step |
| 500.6 | Step-start triggers wait for priority |
| 704.3 | SBAs before priority |
| 609.7a | Choosing a source |
| 616.1 | Only an actual replacement-ordering choice |
| 702.4b / 702.7b | First/double-strike participation |
| 702.2b / 702.2c / 702.2f | Deathtouch |
| 702.19b | Trample assignment |
| 702.15b / 702.15f | Lifelink |
| 702.16a | Protection quality (D10 source view) |
| 702.16e | Protection damage prevention |
| 724.1b / 724.2b | Stack exile |
| 800.4a / 800.4e / 800.4j | Leaving player |
| 903.10a + 704.6c | Commander damage |
| 400.7 | New object on zone change |
| 120.3 | Damage results |

**Don't cite** CR 117.4/405.5 as governing this object: they speak only of spells
and abilities.

**Never write** `CR 310.x` (current 310 = Battles), `CR 408.x`, `CR 419.x`,
`CR 420.x`, `CR 502.x` or `CR 600.x` for historical rules.

## 9. Implementation phases

**Five phases** — 3a, 3a-II, 3b, 3c, 3d. Each is its own PR: plan → plan
review → implement → impl review. 3a-II was split out of 3a during 3a's plan
review; see its own section for why.
`LegacyAxis::CombatDamageTiming` stays out of `IMPLEMENTED_LEGACY_AXES`
(`types/custom_format.rs:698`) until Phase 3d. Before then, `OnStack` is
reachable only through `FormatConfig::for_custom_rules` in tests.

Tests are paired with an accepted/Modern control on the same board, and proven
sharp by mutating the fix and pasting the failure.

### Phase 3a — Stack object + classification (no behavior)

- **Scope:**
  - `StackEntryKind::CombatDamage` and `AssignedCombatDamage` /
    `AssignedDamageRecipient`;
  - `StackObjectClass` (D5) — **without** the controller, which moves to 3a-II;
  - every §3.6 decision, the display label, TS types;
  - fix the `CombatDamageTiming` doc comment;
  - **no protocol bump** — 3a's row in §4's wire-emission matrix is **None**,
    and this phase must not touch `PROTOCOL_VERSION`.
- **Tests:**
  1. No ability-filter effect can target it — decided by `class()`, so the
     `class()`-revert mutation must turn it red. Control: real activated and
     triggered abilities are offered in the same legal set.
  2. No spell-filter effect can target it. **Structural**, not class-decided
     (a combat-damage entry has no `GameObject`), so this row claims no
     mutation and carries a reach-guard instead.
  3. Serde round trip, including a legacy bare-`ObjectId` incarnation payload.
  4. `kind_label` comes from the engine, for both sub-steps.
  5. Entries never coalesce in the stack display; identical triggers still do.
  6. The resolution fence captures the new kind.
  7. Never priority-yielded (CR 117.3d), with a stored yield proving the
     instrument fires on the control.
  8. Storm count ignores it.

### Phase 3a-II — Controller privatization

Split out of 3a during 3a's plan review. **Why it is a separate phase:** 3a adds
no push authority, so no entry can lack a controller until 3c; the two changes
have disjoint break sets (the variant breaks ~26 exhaustive `match` arms, while
privatizing the field breaks every `StackEntry { … }` literal — E0451 — plus
every controller read, ~495 literals across ~137 files); and bundling them makes
one reviewable diff out of two unrelated mechanical sweeps. The plan review
confirmed no foreclosure: `class()`'s consumer surface after 3a is three sites,
so adding the controller to the projection later is a three-site edit.

- **Scope:**
  - `StackEntry.controller` becomes a private `Option<PlayerId>`;
  - the D9 enforcement decision — `class()` as the only public controller
    reader, versus a plain accessor with that claim dropped;
  - the constructor signature that keeps ~495 literals from each spelling
    `Some(..)`;
  - `stack_object_controller`'s signature and its four non-test callers;
  - `apply_resolved_stack_push`'s `UnknownController` invariant, **kind-gated**
    so only `CombatDamage` may be controllerless;
  - the CR 901.10b planechase write, the CR 800.4a elimination sweep;
  - `StackEntryDisplay.controller` optionality, its TS mirror, and
    `StackEntry.tsx`'s fallback chain — `PlayerId::default()` is a real seat, so
    a missing value must not render as seat 0;
  - the AI planner's transposition hash and the manabrew encode;
  - **Bump #1** per §4's wire-emission matrix — the controller field rides every
    existing entry, so every peer receives the changed shape at once.
- **Tests:**
  1. **The phase's rules claim:** a controllerless entry survives a player
     leaving the game (2009 CR 600.4a: "A player leaving the game doesn't affect
     combat damage on the stack"; current CR 800.4a). Control: that player's own
     triggered ability *is* removed by the same sweep.
  2. No seat is attributed to a controllerless entry in `DerivedViews`.
  3. Journal replay accepts a controllerless push and still rejects an unseated
     `Some(..)`.

### Phase 3b — Damage-source identity (no behavior)

- **Scope:**
  - D10: `DamageSourceRef` through context, proposed events, damage events and
    `source_amounts`;
  - the `damage_source_view` authority routing protection, the replacement
    source filter, matcher source filters and `damage_source_options`;
  - the chosen-source-with-incarnation filter shape (through the
    `add-engine-variant` gate);
  - protection predicates over `DamageSourceView`; the post-replacement
    source slots;
  - `LKISnapshot.is_commander` and `is_token`;
  - the stamping rule, and the legacy-payload compat deserializer;
  - the client event-type mirrors;
  - **Bump #2** per §4's wire-emission matrix (damage events are emitted by
    every game).
- **Tests (building block):**
  1. `damage_source_view` returns `Live` for the same incarnation and `Lki` for
     a departed one.
  2. Protection from red prevents damage from a red source read through `Lki`.
     Control: a non-red LKI source isn't prevented.
  3. A "red sources" prevention filter matches an LKI red source.
  4. A chosen-source shield for incarnation *n* doesn't apply to *n+1*.
  5. **Pinning test for the stamping rule:** a dies-trigger's noncombat damage,
     whose source has since changed incarnation into the graveyard, is judged
     against the graveyard object exactly as before, e.g. by a protection
     quality the graveyard card has. It fails if a site stamps the captured
     battlefield incarnation instead.
  6. `matches_target_filter_on_lki_snapshot` treats a ceased token as a token
     ("nontoken source" doesn't match). Control: a nontoken card does match.
  7. A legacy bare-id damage event deserializes.

### Phase 3c — `OnStack` engine behavior

- **Scope:**
  - the seam (§3.2): push (always one object per step, zero-amount
    assignments recorded) → step-start triggers → prompt → Priority, the D3
    guard, the decision-maker seat;
  - the resolver arm and `ResolvedCombatDamage` (§3.3);
  - D7;
  - 310.4a–c dealing (§3.4);
  - the D6 widening;
  - journaling (§3.10);
  - `combat_damage_timing.rs`;
  - **Bump #3** per §4's wire-emission matrix — `PendingCombatLifelink.origin`
    and the first pushes of the 3a variant, which land together here;
  - R1–R10 traced.
- **Tests:**
  1. *Window exists:* Priority with exactly one `CombatDamage` entry. Modern
     control: damage already dealt, no entry.
  2. *Source sacrificed:* dealt in full, with the LKI controller.
  3. *LKI keywords by incarnation:* a sacrificed lifelink source still gains
     life; a bounced deathtouch source still destroys its blocker; a source
     bounced, recast and gone again uses the first incarnation's snapshot.
  4. *Current characteristics:* lifelink granted after assignment gains life.
  5. *Power change after assignment:* the amount is unchanged.
  6. *Recipients:* bounced or flickered → not dealt, no prevention event;
     removed from combat → dealt; no longer a creature → not dealt.
  7. *Commander damage from an LKI source* is counted.
  8. *After assignment:*
     - protection from red granted to the blocker prevents a sacrificed red
       token's damage;
     - a Circle-of-Protection-class shield chooses the already-sacrificed
       source.

     Control: the other incarnation isn't shielded.
  9. *First strike:* two objects, a window after each; 502.2c and 502.28d.
  10. *Empty first-strike step* (every first striker's blocker removed, no
      trample): an **inert** first-strike object appears with an empty
      assignment list and resolves, a Priority window follows, then the regular
      object is pushed. **Also asserts** the first strikers deal no damage in
      the regular sub-step, which is the D0 departure in §1.4's table.
  11. *First strike + lifelink + two life-gain replacements (D7):* a Priority
      window exists before the second object. Modern control: the resume still
      flows straight into the regular sub-step.
  12. *Beginning-of-combat-damage-step trigger:*
      - The stack order is trigger **above** the object, and it resolves before
        damage.
      - Variant: two same-controller triggers raise `OrderTriggers`; after
        ordering, exactly one damage object exists and the marker fired once.
      - Variant: the same with an **inert** step (empty assignment list). The
        marker fires once, the trigger is above the inert object, and exactly one
        object exists.
      - No card has this trigger text, so the test uses a **synthetic
        `TriggerDefinition`**, and the `/card-test` verbatim-Oracle requirement
        doesn't apply.
  13. *CR 724.1 end the turn and CR 724.2 end combat* with the object on the
      stack: no damage, clean teardown, and the gate doesn't re-fire.
  14. *Observer trigger on a sacrificed source:* "whenever a creature you
      control deals combat damage to a player" fires **exactly once** (the R1
      probe).
  15. *Multiplayer:*
      - APNAP priority with the object on the stack;
      - a recipient player eliminated (their assignments dropped, others dealt);
      - **the active player eliminated:**
        - the object remains;
        - damage *by* creatures they owned or controlled is dealt from LKI
          (2009 600.4a);
        - damage *to* their creatures is dropped;
        - priority after resolution goes to the next player (CR 800.4j);
        - a creature they **stole** (control-changing effect) returns to its
          owner and deals and receives its damage **live**;
        - an LKI lifelink source they controlled gains them no life.
  16. *Counter/Stifle-class* effects have no legal target (reachable flow).
  17. *Journal replay* of a push and resolve reproduces an identical state.
  18. *Reflect rider on a sacrificed token source:* a "prevent that damage; it
      deals that much damage to that source's controller" shield created after
      assignment finds the token's controller from LKI. Control: under Modern
      the same shield against a live token behaves as today.
  19. *Zero and negative power (310.2a):* a 0/1 attacker and a creature at
      power −2 attack an open board.
      - `OnStack`: exactly one `CombatDamage` entry appears, listing a
        **0-amount assignment** for each. It resolves with no `DamageDealt`
        event, no life change and no "deals combat damage" trigger, then
        Priority follows.
      - Modern control: no entry, and neither creature assigns (CR 510.1a).
      - Mutation proof: restoring the `power == 0 → continue` skip under
        `OnStack` must fail the entry assertion.

### Phase 3d — Release: gate, presets, client and AI polish

- **Gate:** add `LegacyAxis::CombatDamageTiming` to `IMPLEMENTED_LEGACY_AXES`,
  and re-check every reachability claim against the widened gate.
- **Preset:** **write and register** `middle_school()` only. It doesn't exist
  at the pin. Use PLAN.md §2 and RESEARCH.md §1; verify set codes against
  `set_catalog`; assert rosters by name.
- **Classic Magic: deferred** (Q4). Its registration follows in its own PR,
  once the Time Vault / Illusionary Mask format-scoped card text exists. That
  PR must also add a registration-gate test proving `classic_magic()` can't
  become selectable without those overrides.
- **Polish:** AI arms (§6); `cargo ai-gate` once with no refresh;
  `StackTargetArcs`; README / IMPLEMENTATION_PLAN status.

**Size:**
- **3a:** medium, compiler-guided; the D9 field change produces most of the
  errors.
- **3b:** medium-large, a wide mechanical signature change with a small core.
- **3c:** large; D7, R1, R2 and R9 are the likeliest multi-round items.
- **3d:** medium.

## 10. Open questions

- **Q1 — resolved (maintainer review on #8898):** every combat damage step
  pushes exactly one object, including an inert one. Zero-amount assignments
  are recorded under `OnStack` (2009 310.2a). A 0 amount deals no damage
  (current CR 120.8). See §3.2 and test 3c-19.
- **Q2 — resolved:** no controller (D9, 2009 600.4a).
- **Q3 — scope beyond the presets:** "Lost Legacy 606" (CONTEXT.md) is
  expressible with this axis, but it isn't a bundled preset.
- **Q4 — resolved (2026-09-15):** Classic Magic's ruleset is its enumerated
  four-item list, so D0 is faithful to it. The only gap is the Time Vault /
  Illusionary Mask card text. Decision: **Middle School first; Classic
  deferred** to a separate follow-up PR that adds those overrides.
- **Q5 — resolved:** damage from a departed player's sources is dealt from LKI.
  2009 600.4a: "A player leaving the game doesn't affect combat damage on the
  stack"; combined with 310.4a/b. Implementation dependency: R9.

## 11. Review log

### Round 1

**Architecture (REVISE):**

| Finding | Change |
|---|---|
| H1 lifelink resume skips the post-resolution window | D7; test 3c-11 |
| H2 CR 500.6 triggers after the window | triggers handled at push; marker as a parameter; test 3c-12 |
| H3 an `Option` context override is incomplete; `LKISnapshot` lacks `is_commander` | `ResolvedCombatDamage`; `is_commander`; test 3c-7 |
| H4 LKI read by id | `lki_by_incarnation` |
| M1 CR 724.2 | R3, test 3c-13 |
| M2 elimination by controller | no controller |
| M3 D5 rationale and site count | grep rule |
| M4 journaling | §3.10 |
| M5 R1 evidence | R1 probe |
| M6 observer triggers on LKI sources | subsumed by D10 |
| L1 dead code | policy module moved |
| L2 empty first strike | §3.2 |
| L3 priority seat | `turn_decision_maker` |
| L4 cites | fixed |

**Rules:**

| Finding | Change |
|---|---|
| Classic's framing | §1.4, D0 as a stated decision, Q4 |
| Assignment conflicts | departures table |
| 6ED date | fixed |
| Active player as controller is a CR 800.4a hazard | no controller |
| Q1 | rewritten |
| CR over-citation | §8 |
| Literal first-strike reading | table row |

### Round 2

**Architecture (REVISE):**

| Finding | Change |
|---|---|
| N1 step-start triggers collected before the push land below the object; a prompt is clobbered; re-entry double-collects | §3.2 order is push → triggers → prompt → Priority; test 3c-12 asserts stack order plus an `OrderTriggers` variant |
| N2 source identity doesn't reach Phase A protection, the Phase B source filter, or the chosen-source shield | §3.11 D10 (`DamageSourceRef` + `damage_source_view` + chosen source with incarnation via the variant gate); Phase 3b; tests 3b-2..4, 3c-8 |
| N3 controller option (a) unenforceable | D9: `Option<PlayerId>` field, compiler-enumerated readers |
| N4 3b fields shipped without a bump | per-phase bumps (§4); event mirrors in §5 |
| N5 LKI capture on player leave unverified | R9; test 3c-15 asserts LKI damage |
| N6 3b too large | split into 3b (identity, no behavior) and 3c (behavior) |
| L-a three resume entry points | §2, D7 |
| L-b resume marker value; marker wording | derived from origin; §3.2 wording |
| L-c priority after a departed active player | R10; test 3c-15 |

**Rules:**

| Finding | Change |
|---|---|
| Q5 decided by 2009 600.4a's last sentence | Q5 closed as dealt-from-LKI; D9 cites it |
| An empty first-strike sub-step would skip CR 510.3's window | §3.2 grants the window; the modern-path gap recorded as a separate issue; test 3c-10 |
| Departures table missed triggered deathtouch/lifelink's main effects; banding 502.10h | rows expanded; banding row added |
| Illusionary Mask quote not verbatim | full sentence quoted; Time Vault `[sic]` |
| Owned vs controlled | §3.4 ownership paragraph; test 3c-15 |
| Step-start marker in the second step | §3.2 note (no observable card) |

### Round 3 (final)

**Architecture (APPROVE WITH CHANGES):**

| Finding | Change |
|---|---|
| C1 empty-branch prompt returns before the sub-step is marked dealt; the marker fires twice | §3.2 marks the sub-step dealt before triggers and prompt; empty-sub-step variant in test 3c-12 |
| C2 protection can't use the LKI filter matcher | protection predicates over `DamageSourceView` (§3.11); no LKI-only copy |
| C3 post-replacement source slots and the live `is_token` read | added to D10; `LKISnapshot.is_token`; test 3c-18, test 3b-6; commander keys, shield-host events and excess damage stay id-keyed, stated explicitly |
| C4 3b's no-behavior claim needs a noncombat stamping rule | stamping rule (§3.11); pinning test 3b-5 replaces the unfalsifiable suite-run check |
| C5 a bare `Option` controller invites unwraps | D9: private field; controller carried in `StackObjectClass`; serde correction |
| L1 one helper for `include_phase_event` | §3.2 |
| L2 legacy event payloads | compat deserializer (§3.11); test 3b-7 |
| L3 untested D0 first-strike departure | test 3c-10 |
| L4 synthetic trigger in test 3c-12 | stated |

**Rules:**

| Finding | Change |
|---|---|
| Control-changed permanents aren't exiled when their controller leaves | §3.4 three-case table; test 3c-15 |
| The banding 502.10h row was wrong | row removed |
| CR 702.16b is targeting, not damage | §8 cites 702.16a / 702.16e |
| The elided 310.1 quote is about assignment triggers | §3.2 cites 2009 408.1f + current CR 500.6 |
| Lifelink to a departed controller | §3.4 paragraph; test 3c-15 |

### PR review (phase-rs/phase#8898)

| Finding | Change |
|---|---|
| **MED** (matthewevans): an empty or zero assignment still needs the historical stack object; a 0- or negative-power creature assigns under 310.2a | Always one object per step (§3.2); zero-amount assignments recorded under `OnStack`; §1.4 row; Q1 resolved; tests 3c-10, 3c-12 and new 3c-19. This also removes round 3's C1 empty-branch hazard: there is no longer an empty branch. |

**Review loop closed** at the three-round cap. The final architecture verdict is
APPROVE WITH CHANGES, and all changes are applied above. Each implementation
phase still gets its own plan review and implementation review.

---

*Research inputs:*
- the archived CR texts and EC pages in §1.1 (fetched 2026-09-15);
- Scryfall API queries (§1.4, §3.2);
- traces of `combat_damage.rs`, `combat.rs`, `stack.rs`, `priority.rs`,
  `turns.rs`, `elimination.rs`, `deal_damage.rs`, `replacement.rs`,
  `choose_damage_source.rs`, `create_damage_replacement.rs`, `end_phase.rs`,
  `trigger_matchers.rs`, `zones.rs` and `types/custom_format.rs` at `05c27e0d5`.
