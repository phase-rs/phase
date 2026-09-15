# `CombatDamageTiming::OnStack` — Design

**Status:** design only, no code. This is the dedicated design pass that
`PLAN.md` §6/§7/§8 and `IMPLEMENTATION_PLAN.md` require before this axis is
built. It is the last `LegacyRuleSet` axis, and it gates the two remaining
Eternal Central presets, `middle_school()` and `classic_magic()`.

**As of:** `upstream/main` @ `05c27e0d5`. Every `file:line` below is pinned to
that commit (paths relative to `crates/engine/src/` unless stated). Re-verify
before implementing — `combat_damage.rs` and `stack.rs` move quickly.

---

## 0. Summary

- Before M10, combat damage was **assigned**, put on the stack as **one object
  per combat damage step**, and **dealt when that object resolved** — after a
  full priority round. That object was not a spell or an ability, so it could
  not be countered or targeted.
- The engine already separates assignment from dealing internally
  (`CombatState.pending_damage`). The design puts a priority round at that
  seam. It does not rewrite combat: assignment, the batch-replacement
  pipeline, lifelink batching, and the trigger/SBA loop are all reused
  unchanged.
- New surface:
  - one `StackEntryKind::CombatDamage` variant;
  - a three-way stack-object classification, replacing the current
    `Option`-means-spell;
  - a resolver arm, and the CR 310.4a–c dealing semantics (live source if it
    is still the same object, otherwise last-known information; recipients
    that are gone are skipped);
  - one widening of the CR 609.7a "choose a source" options;
  - a policy module like `game::mana_burn`.
- Three implementation phases (§9). The gate flips, and the two presets
  register, only in the last.
- **Correction to the charter:** `PLAN.md:644`, `RESEARCH.md:331`, and the
  `CombatDamageTiming` doc comment (`types/custom_format.rs:79-84`) all call this
  rule "pre-6th-edition". That is backwards. Damage on the stack was
  **introduced** by the Classic Sixth Edition rules (1999) and **removed** by
  M10 (July 2009). See §1.3. Implementation Phase 1 fixes the doc comment.
  The merged design documents stay as reviewed, following
  `IMPLEMENTATION_PLAN.md`'s convention, and this document records the
  correction.

---

## 1. The rule being implemented

### 1.1 Sources

The current CR (`docs/MagicCompRules.txt`) has no damage-on-the-stack rule.
The historical text below comes from archived Comprehensive Rules files.
Petr Hudeček's rulebook archive is a fan mirror of the Wizards text; the files
carry the original Wizards headers.

| Document | URL |
|---|---|
| CR current as of **May 1, 2009** (last pre-M10) | <https://hudecekpetr.cz/other/rulebooks/comprehensive-2009-05-01.txt> |
| CR dated **April 23, 1999** (Classic Sixth Edition) | <https://hudecekpetr.cz/other/rulebooks/comprehensive-1999-04-23.txt> |
| CR effective **July 11, 2009** (M10) | <https://hudecekpetr.cz/other/rulebooks/comprehensive-2009-07-08.txt> |
| "Magic 2010 Rules Changes" (Forsythe/Gottlieb, 2009-06-10) | <https://magic.wizards.com/en/news/feature/rules-changes-2009-06-10> |
| Eternal Central — Middle School rules | <https://www.eternalcentral.com/middleschoolrules/> |
| Eternal Central — Classic Magic rules | <https://www.eternalcentral.com/classicmagicrules/> |

**Numbering trap:** in the pre-M10 CR, the combat damage step is **rule 310**,
not 510; rule 510 was "Status" back then. Everything cited as `CR 310.x` below
is a *historical* number. **Such numbers must never appear in engine CR
annotations**, because the annotation gate checks against the current CR. §8
gives the annotation convention.

### 1.2 The 2009 text (verbatim)

> **310.1.** As the combat damage step begins, the active player announces how
> each attacking creature will assign its combat damage. Then the defending
> player announces how each blocking creature will assign its combat damage.
> All assignments of combat damage go on the stack as a single object. Then any
> abilities that triggered on damage being assigned go on the stack. […] Then
> the active player gets priority and players may play spells and abilities.
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
> Glossary, **Removed from Combat:** […] if combat damage assigned to or by that
> permanent is already on the stack, it will resolve normally.

The M10 CR's 510.2 confirms what changed: "This turn-based action doesn't use
the stack. No player has the chance to cast spells or activate abilities
between the time combat damage is assigned and the time it's dealt. **This is a
change from previous rules.**"

### 1.3 Era: Sixth Edition through M10

- The 1999 CR (310.2–310.4, 408.1g) already has the full model: "All
  announcements of combat damage go on the stack as a single entry", it "can't
  be countered", and it is "dealt as originally assigned", using the source "as
  it currently exists, or as it most recently existed".
- Between 1999 and 2009 the model did not change materially. The 2009 text
  only adds the word "object", triggers on assignment, double strike,
  planeswalker recipients, and an explicit priority pass after resolution.
- Before Sixth Edition (Fourth/Fifth Edition) there was no stack at all:
  batches, interrupts, and a damage-prevention step. **Nothing in this project
  targets that model.** The Old School 93/94 and 95 presets use modern rules
  plus mana burn (RESEARCH.md §1), and no preset needs batches.
- Exact effective date of the Sixth Edition change: the archive's earliest CR
  is dated 1999-04-23, and the set released 1999-04-28. The precise day is
  **unverified** and does not matter for the design.

### 1.4 What the presets actually require: modern rules plus one overlay

Both Eternal Central formats describe themselves as current rules plus named
exceptions, not as "play by the 2009 CR":

- Middle School: "Damage Uses the Stack (as it did with Sixth Edition rules for
  example, so combat tricks such as Morphling, Triskelion, and Mogg Fanatic
  work)". Its C.1.Ruling.3 gives the sequence: "6. Assign combat damage (but
  don't deal it yet) 7. Chance for instants and abilities. 8. Deal combat
  damage. 9. Triggers on damage being dealt".
- Classic Magic: "DAMAGE USES THE STACK (as in, combat tricks such as Morphling,
  Triskelion, and Mogg Fanatic work)".

**Design decision D0: only the *timing* is historical.** Assignment legality
stays exactly current CR 510.1a–e: free division among blockers (510.1c/d),
trample lethal thresholds (702.19), banding (702.22j/k). The same holds for
every other rule — current Oracle text, current layer system, current lifelink
as a static ability (702.15b), and so on. The overlay changes one thing: what
happens between assignment and dealing, and how dealing treats a source or
recipient that changed in between. By a stroke of luck, current 510.1c/d once
again say "divided as its controller chooses", as 310.2c/d did. The M10 damage
assignment order no longer exists, so there is no assignment-rule conflict to
decide.

**Triggers on damage being assigned** (310.1): out of scope, because no current
Oracle text has one. Scryfall `o:"is assigned combat damage"` returns zero cards
(2026-09-15). `o:"assigns combat damage"` with when/whenever returns five cards,
all static "assigns combat damage equal to its toughness" or "as though it
weren't blocked". If such a trigger ever appears, the seam in §3.2 is where it
would fire.

---

## 2. How combat damage works today

| Concern | Location | Note |
|---|---|---|
| Step entry | `game/turns.rs:3554` (`Phase::CombatDamage` arm of `auto_advance_once`) | `flush_layers`, then `resolve_combat_damage`; `Some(wf)` → wait, else Priority. |
| Main resolver (re-entrant) | `game/combat_damage.rs:107` `resolve_combat_damage` | Lifelink-resume guard (`:115`), `regular_damage_done` guard (`:141`), first-strike snapshot (`:145-153`). |
| First-strike sub-step | `combat_damage.rs:157-190` | `collect_damage_assignments` → **`take_pending_damage` → `apply_combat_damage`** (`:162-163`). |
| Regular sub-step | `combat_damage.rs:193-218` | Same pair at `:197-198`. |
| Sub-step bookkeeping | `combat_damage.rs:226` `finish_combat_damage_sub_step` | Sets flags, runs `process_combat_damage_triggers` (`:46`), returns Priority after first strike when the stack is non-empty. |
| Assignment collection | `combat_damage.rs:745` `collect_damage_assignments` | Resumable through `damage_step_index`; `WaitingFor::AssignCombatDamage` / `AssignBlockerDamage`; handlers in `game/engine_combat.rs:528,750`. |
| Assigned-but-not-dealt data | `game/combat.rs:299` `CombatState.pending_damage: Vec<(ObjectId, DamageAssignment)>` | `DamageAssignment { target: DamageTarget, amount: u32 }` (`combat.rs:392-402`). |
| Dealing | `combat_damage.rs:1535` `apply_combat_damage` | Phase A gate (`effects/deal_damage.rs:445`), Phase B `replacement::replace_combat_damage_batch` (`replacement.rs:10624`), Phase C apply + commander damage + per-source lifelink (`drain_combat_lifelink`, `:1778`). |
| Source characteristics at dealing | `combat_damage.rs:1570-1578` | `DamageContext::from_source` (`effects/deal_damage.rs:308`) reads `state.objects[id]` **with no zone or incarnation check**. If the object is missing, `DamageContext::fallback` (`:355`) keeps only the LKI controller — **no LKI keywords**. |
| Completeness gate | `game/priority.rs:81-91` | Empty stack, all pass, `CombatDamage && !regular_damage_done` → re-enter `auto_advance` (runs the regular sub-step). |
| Leaving the step | `priority.rs` (else branch) → `turns::advance_phase_once` (`turns.rs:126`) | End of combat teardown at `turns.rs:269`. |

**The seam:** in both sub-steps, `take_pending_damage` is immediately followed
by `apply_combat_damage`. Everything before that pair is *assignment*;
everything from `apply_combat_damage` onward is *dealing*. Modern rules keep
the pair together. `OnStack` splits it across a stack object.

**Today's legacy-axis pattern** (the template to follow):
`game/mana_burn.rs:31-42` has `policy_of(&FormatConfig)` and
`applies(&GameState)`; `game/legend_scope.rs:46,61` has the same shape.

---

## 3. Design

### 3.1 The stack object — `StackEntryKind::CombatDamage`

```rust
// types/game_state.rs — new StackEntryKind variant (sketch)
/// Pre-M10 combat damage on the stack: one object per combat damage sub-step,
/// holding every assignment made in it. Not a spell or an ability.
CombatDamage {
    sub_step: CombatDamageSubStep,          // existing enum, game_state.rs:14785
    assignments: Vec<AssignedCombatDamage>,
}

/// One assignment frozen when damage went on the stack.
pub struct AssignedCombatDamage {
    pub source: ObjectIncarnationRef,       // source id + incarnation at assignment
    pub target: AssignedDamageRecipient,
    pub amount: u32,
}
pub enum AssignedDamageRecipient {
    Object(ObjectIncarnationRef),
    Player(PlayerId),
}
```

**D1 — one object per sub-step, not one per source.** 310.1 says "All
assignments … go on the stack as a single object", and the 1999 text says "a
single entry". One entry per source would be wrong in two ways:
1. It would open a priority window *between* two creatures' damage.
2. It would break simultaneity, which `replace_combat_damage_batch` and
   per-source lifelink batching both depend on (CR 510.2 / 616.1).

**D2 — carry incarnations, not bare ids.** 310.4b and 310.4c turn on whether
the source or recipient is *still the same object in play*. The engine keeps
an `ObjectId` across zone changes and bumps an incarnation (CR 400.7). A bare
id cannot tell apart "the creature is still here" and "it died and a
different incarnation now sits in the graveyard with the same id". The
`ObjectIncarnationRef` type already exists (`combat.rs`,
`attacking_incarnations_this_combat`) and is the established identity. **Do
not reuse `(ObjectId, DamageAssignment)` as the entry's payload.** That shape
is right for the modern path, where nothing can change between assignment and
dealing, and wrong here. The modern path keeps `pending_damage` unchanged;
`OnStack` converts it at the seam (§3.2).

**Why not the alternatives:**

| Alternative | Why rejected |
|---|---|
| A synthesized `TriggeredAbility` | It would be a legal target for "target triggered ability" (Stifle) and counterable, violating 310.3. The charter's own earlier draft made this mistake (CONTEXT.md:537). |
| A `KeywordAction` variant | `stack_ability_kind()` classifies `KeywordAction` as an *activated ability* (`game_state.rs:16943`) so Squelch can target Equip. That is the wrong layer: combat damage is not an ability. |
| Leave damage off the stack; add a special "damage window" priority state | Players would see no stack object, yet responses would have to resolve *above* pending damage. Every stack-based reasoning path (resolve order, split second, "counter target spell", AI stack awareness) would need to know about an invisible pseudo-object. It rebuilds the stack badly. |

**Entry fields:**
- `id`: fresh `ObjectId(next_object_id++)` with no `GameObject`, exactly like
  `push_keyword_action` (`game/engine.rs:17196`).
- `controller`: the active player. Nothing uses this for rules purposes; it is
  there for display and stack-controller lookups.
- `source_id`: the entry's own id. The object has no single source, and a
  sentinel `ObjectId(0)` would collide with the monarch-trigger convention
  (`triggers.rs:5968`). Implementation Phase 1 must audit readers of
  `entry.source_id` for this kind. See §7, R4.

### 3.2 Pushing: the seam in `resolve_combat_damage`

```text
collect_damage_assignments(state, sub_step)  -> Some(wf) => return Some(wf)   // unchanged
match combat_damage_timing::policy(state):
  Modern  => take_pending_damage; apply_combat_damage; finish_…               // unchanged
  OnStack => let pending = take_pending_damage(state);
             push_combat_damage_object(state, sub_step, freeze(pending), events);
             reset_priority(state);
             return Some(WaitingFor::Priority { player: active })              // CR 310.1
```

- **Exhaustive `match` on `CombatDamageTiming`**, never `if uses_stack`. A
  future third timing must be a compile error at this site.
- `freeze(pending)` records each source's and recipient's current incarnation.
- SBAs run before the priority grant by the usual CR 704.3 path. No
  assignment triggers exist (§1.4).
- An empty `pending` (no creature assigns damage, e.g. all power 0) pushes
  **nothing**: the object would deal no damage, and the pre-M10 practice was
  not to create one. The sub-step then finishes the modern way. **Needs
  confirmation (open question Q1):** the 2009 text does not say this
  outright.

**D3 — "damage is already on the stack" is derived from the stack, not stored
as a flag.** `resolve_combat_damage` is re-entrant: from the turns arm, the
assignment handlers, the lifelink resume, and the priority completeness gate.
It must not re-collect assignments for a sub-step whose object is already on
the stack. Use a helper in the style of `crew_pending_on_stack`
(`engine.rs:17040`), `combat_damage_on_stack(state) ->
Option<CombatDamageSubStep>`, and put its guard next to the
`regular_damage_done` guard (`:141`). A stored `CombatState` flag would be a
second authority that can drift from the stack, for example after CR 724.1b
"exile every object on the stack".

**Collection state hygiene at push:**
- Clear `damage_step_index` and `damage_assignments` for the sub-step, as
  `finish_combat_damage_sub_step`'s first-strike arm does, so the
  next sub-step's collection starts clean.
- **Leave the sub-step completion flags unset** (`first_strike_done`,
  `regular_damage_done`). They mean "damage for this sub-step has been
  *dealt*". Setting them at push would break the priority completeness gate's
  meaning and the CR 500.6 `include_phase_event` computation in that function.

### 3.3 Resolving: `stack.rs` arm

The template is the `KeywordAction` early return at `game/stack.rs:1372`. Add a
sibling early return before `bind_resolution_scope`:

```text
StackEntryKind::CombatDamage { sub_step, assignments } =>
    combat_damage::resolve_combat_damage_object(state, sub_step, &assignments, events)
    // then StackResolved + finish_resolving_stack_entry(Resolved), as KeywordAction does
```

`resolve_combat_damage_object` must:
1. **Resolve each assignment against the current game** (310.4a–c, §3.4) into
   the existing `(ObjectId, DamageAssignment)` plus a `DamageContext` per
   source.
2. Call the **existing** `apply_combat_damage` batch unchanged, so Phases A–C,
   batch replacement, commander damage, and lifelink batching all stay one
   implementation. Implementation Phase 2 widens `apply_combat_damage` to take
   an optional per-assignment `DamageContext` override; the modern path passes
   none.
3. Handle `CombatDamageBatch::Complete` by calling the **existing**
   `finish_combat_damage_sub_step`, which sets the sub-step flags and runs the
   trigger/SBA loop. Handle `Paused` through the existing
   `pending_combat_lifelink` park-and-resume.

After it resolves:
- The active player gets priority (310.4; engine: `priority.rs:154-165`).
- **After the first-strike object:** when all players pass with an empty
  stack, the existing completeness gate (`priority.rs:81-91`) sees
  `CombatDamage && !regular_damage_done` and re-enters `auto_advance` →
  `resolve_combat_damage`. That collects the regular assignments and pushes
  the second object. This is 310.5's "second combat damage step", with its
  own priority window, for free.
- **After the regular object:** `regular_damage_done` is set, so the next
  all-pass on an empty stack advances to end of combat through the normal
  branch.

**D4 — no counter or target interaction is needed at resolution.** Nothing can
counter or retarget the object (310.3, §3.5). So there is no fizzle check, no
`bind_resolution_scope`, and no zone routing.

### 3.4 Dealing semantics (310.4a–c)

The one rules-bearing difference from the modern dealing path, which assumes
nothing changed since assignment:

| Case | Rule | Behavior |
|---|---|---|
| Source is the same incarnation, on the battlefield | 310.4b | `DamageContext::from_source`: **current** characteristics. Lifelink granted after assignment counts; deathtouch lost after assignment doesn't. |
| Source left the battlefield (sacrificed Mogg Fanatic, bounced Morphling), or its incarnation changed | 310.4a + 310.4b | Damage is still dealt with the assigned amount. The source is its **last-known information**: controller, lifelink, deathtouch, wither, infect, toxic, and commander flag from `LKISnapshot` (`game_state.rs:372`; `keywords` at `:406`). **New:** `DamageContext::from_lki(&LKISnapshot)`. The current `fallback` (`deal_damage.rs:355`) drops every keyword, which is wrong for this path. |
| Source power changed after assignment | 310.4a | Amount stays as assigned. It is already frozen in `amount`. |
| Recipient creature, planeswalker, or battle: same incarnation, still that type, on the battlefield | 310.4a | Dealt, **even if removed from combat**. |
| Recipient left the battlefield, changed incarnation (flickered), or is no longer a creature/planeswalker/battle | 310.4c | That assignment is **not dealt**. It is dropped before Phase A, so nothing is prevented and no prevention event fires. |
| Recipient player left the game | current CR 800.4a | Not dealt. |
| Protection, prevention, redirection created after assignment | 310.4 + current CR 615 | Applies, because it is evaluated when the damage would be dealt. The existing Phase A gate and Phase B batch already do this. |

- **Battles.** 310.4c predates battles. D0 says current object types apply, so
  a battle recipient is handled like a planeswalker. Per §8, the code
  annotates the current rules (CR 510.2 departure, CR 120.3 damage results)
  and names the historical 310.4c only in prose.
- **LKI lifetime.** `lki_cache` clears on phase/step transitions
  (`game_state.rs:19853`). The step cannot end while the object is on the
  stack (current CR 117.4 / 405.5), so the snapshot outlives the object. Add a
  debug assertion that an off-battlefield source always has an LKI entry, so a
  missing capture fails loudly.
- **Existing modern-path gap (out of scope):** `from_source` does not check zone
  or incarnation. Under modern rules nothing can change between assignment and
  dealing, so the gap is unreachable there. Record it only; do not change the
  modern path in this project.

### 3.5 Classification: not a spell, not an ability

`StackEntry::stack_ability_kind()` (`game_state.rs:16943`) returns
`Option<StackAbilityKind>`, where `None` means **spell**. A third kind of object
cannot be expressed that way. Returning `None` for `CombatDamage` would make it
"spell-like" at every reader that treats `None` as spell.

**D5 — replace the `Option` with an exhaustive classification:**

```rust
pub enum StackObjectClass {
    Spell,
    Ability(StackAbilityKind),
    /// CR 310.3 (2009): combat damage on the stack is neither.
    CombatDamage,
}
```

- `matches_stack_ability_kind` becomes `matches!(class, Ability(k) if …)`.
- Each current caller of `stack_ability_kind()` must choose explicitly. That is
  the point of making it exhaustive, and the reason the current doc comment
  already asks for it ("a future `StackEntryKind` variant must make this
  classification decision explicitly").

This parameterizes an existing axis instead of adding a sibling predicate
(CLAUDE.md "Parameterize, don't proliferate").

**Consequences:**
- **Counterspells and Stifle can't target it.** `effects/counter.rs:141,412`
  already use `matches!(… Spell)`, and `TargetFilter::StackAbility` goes
  through `matches_stack_ability_kind`, so both exclude it correctly once the
  class exists.
- **Split second** (current CR 702.61a): `keywords::stack_has_split_second`
  (`keywords.rs:97`) reads `objects[entry.id]`, and there is no object, so it
  is safe.
- **Storm** counts casts (`derived_views.rs:2205`), not stack entries, so it is
  unaffected.
- **Copy effects** (`effects/copy_spell.rs`): exhaustive matches get a
  `CombatDamage` arm that is not copyable.
- **Ending the turn** (current CR 724.1b, "Exile every object on the stack"):
  `effects/end_phase.rs:51-62` already makes "non-card stack entries cease to
  exist". Confirm in Phase 2 that the entry disappears and that combat
  teardown leaves no stranded `regular_damage_done` gate (§7, R3).

### 3.6 Exhaustive-match sites that need an arm

Grep proxy: every non-test site naming `StackEntryKind::KeywordAction`. The
compiler enforces the list, so this table sizes the work; it does not replace
the compiler.

| Site | Decision for `CombatDamage` |
|---|---|
| `types/game_state.rs:16281,16294` `ability()`/`ability_mut()` | `None` |
| `game_state.rs:16042` `StackResolutionEntryProvenance` | new `CombatDamage` provenance |
| `game_state.rs:16946` `stack_ability_kind` | replaced by D5 |
| `game_state.rs:25101` yield-scope matching | not an ability, spell, or trigger; a yield "until stack empty" still waits for it |
| `game/stack.rs:1372` / `:1424` | resolver arm (§3.3) / `unreachable!` |
| `stack.rs:5031,5086,5102` display grouping | never grouped; a distinct group key |
| `game/targeting.rs:2383-2388` | never matches a stack target filter |
| `effects/copy_spell.rs:172,200,856,943` | not a copy source |
| `game/derived_views.rs:2194` storm; `:2810-2831` `kind_label`; `:2849` provenance; `:2915` targets | not a spell; label "Combat damage"; assignment lines as the "targets" display (§5) |
| `game/triggers.rs:3017-3022` spell id | `None` |
| `analysis/resource.rs:6648,7011,7189,7245` | no ability / no choice freedom |
| `ai_support/targeted_exchange.rs:568` | not a target |
| `phase-ai/src/policies/stack_awareness.rs:244-294`; `phase-ai/src/deck_knowledge.rs:122`; `phase-ai/src/bin/resolve_bench.rs:202` | §6 |

**Non-exhaustive readers.** These use `matches!`/`let … else` and treat an
unknown kind as "not X". They are correct by default, but still need a
reviewer's read-through: `effects/counter.rs`, `filter.rs:2979,2987`,
`targeting.rs:2624,2640`, `effects/change_targets.rs:592`,
`casting_costs.rs:102`, `sba.rs:1962,2275`, `triggers.rs`, `engine.rs`.
Outside the engine:
- `manabrew-compat/src/lib.rs:3502`: `matches!(Spell)`, safe.
- `engine-wasm/src/lib.rs`: constructors only.

### 3.7 "Choose a source" (CR 609.7a) — the Circle of Protection class

In Middle School and Classic Magic this is the most-played interaction the
overlay creates. A player activates Circle of Protection: Red *after* damage
is on the stack, choosing a creature that may already have been sacrificed.

- The historical 419.8a allowed "a creature that assigned combat damage on the
  stack, even if the creature is no longer in play".
- **Current CR 609.7a already covers it:** "any object referred to by an
  object on the stack … (even if that object is no longer in the zone it used
  to be in)".
- **Gap today:** `effects/choose_damage_source.rs:44`
  `damage_source_options` enumerates objects *in* the battlefield and stack
  zones plus the command zone. A `CombatDamage` entry has no `GameObject`, and
  the sources it refers to may be in a graveyard.

**D6:**
- Widen `damage_source_options` to chain in the sources referred to by every
  `CombatDamage` entry on the stack.
- Filter matches for an off-battlefield source ("a red source") use the
  existing LKI filter evaluation (`matches_target_filter_on_lki_snapshot`).
- Also verify that a prevention shield keyed on the chosen source matches the
  `DamageContext.source_id` the resolver uses for an LKI source (§7, R5).

This widening is a correct application of *current* CR 609.7a, so it is not
gated by the format policy. Under modern rules no `CombatDamage` entry exists,
so it adds nothing.

### 3.8 First strike and double strike

No new logic is needed:
- `first_strike_participants` is snapshotted "as the combat damage step
  begins" (`combat_damage.rs:145-153`), which matches 310.5.
- `deals_in_substep` (`:442`) lets the regular sub-step include creatures not
  in the snapshot *or* with live double strike.

That already yields both historical rules:
- **502.2c:** removing first strike after first-strike damage is on the stack
  still leaves the creature in the snapshot without double strike, so it
  doesn't deal regular damage. Granting first strike to a creature outside
  the snapshot still lets it deal regular damage.
- **502.28d:** granting double strike to a first striker after its
  first-strike damage is on the stack makes it deal regular damage.

Implementation Phase 2 adds tests for both. They are the cheapest proof that
the snapshot survives the new priority window.

### 3.9 Policy module

```rust
// game/combat_damage_timing.rs — mirrors game::mana_burn
pub(crate) fn policy_of(format_config: &FormatConfig) -> CombatDamageTiming { … }
pub(crate) fn policy(state: &GameState) -> CombatDamageTiming { policy_of(&state.format_config) }
```

Unlike `mana_burn::applies`, no `bool` wrapper: callers `match` on the enum
(D3's exhaustive-match rule).

---

## 4. Serialization and protocol

- `StackEntryKind` and `GameState` are on the wire, so a new variant changes
  the serialized full-game state.
- **Bump `lobby-broker` `PROTOCOL_VERSION`** (71 at pin,
  `crates/lobby-broker/src/protocol.rs:519`), with its changelog line, value
  pins, and the client mirror. This follows the #8870 precedent and changelog
  entries 18/19.
- `LOBBY_PROTOCOL_VERSION` does **not** move: format selection is unchanged,
  since `damage_timing` already exists in the schema.
- `size_of::<StackEntry>() <= 768` (`types/game_state_size.rs:66`): a `Vec` plus
  a small enum fits. Re-check after the edit.
- `AssignedCombatDamage` derives `Serialize`/`Deserialize`/`PartialEq`/`Eq`.
  No `HashMap`, so no deterministic-serde wrapper is needed.

## 5. Frontend — display only

The TS types are hand-written (`client/src/adapter/types.ts:1855`
`StackEntryKind` union). Add the variant there.

- `components/stack/StackEntry.tsx:103-166` switches on `entry.kind.type` but
  prefers `details?.kind_label`. The engine supplies `kind_label` ("Combat
  damage", plus "first strike" for that sub-step) and the assignment lines
  through `stack_entry_detail` / `stack_entry_targets`
  (`derived_views.rs:2806,2915`).
- **No client-side derivation of who deals what to whom.** That is engine
  data (CLAUDE.md, frontend is a display layer).
- Target arcs (`StackTargetArcs.tsx`) render from the engine-provided lines.
- Follow the `add-frontend-component` skill for the component change.
- No new `WaitingFor` or `GameAction`: assignment prompts are unchanged and the
  window is plain `Priority`.

## 6. AI

- **Must not break:**
  - `stack_awareness.rs:244` `assess_spell_impact` needs an arm: not a counter
    target, value 0.
  - `deck_knowledge.rs:122`: no source card.
  - The AI gets ordinary `Priority` with the object on the stack and picks
    from existing legal actions.
- **Not required to exploit.** Tricks like "sacrifice Mogg Fanatic after damage
  is on the stack" are a strength improvement, not rules fidelity. They are
  noted as a follow-up policy (`add-ai-feature-policy`), outside this project's
  gate.
- **`cargo ai-gate`:** default AI baselines run built-in formats, where the new
  path is unreachable. Run the gate once in Phase 3 to confirm no change. Do
  not refresh baselines.

## 7. Risks to trace during implementation planning

| # | Risk | Where to trace |
|---|---|---|
| R1 | **Double trigger collection.** `finish_combat_damage_sub_step` runs its own collect/SBA loop (`process_combat_damage_triggers`, `:46`). Stack resolution is followed by the reducer's generic post-action trigger collection. On the modern path, damage events reach the reducer too, and `claim_combat_lifelink_batch_events_for_ordinary_collection` (`:389`) shows events are claimed to prevent double collection. The stack-resolution caller must get the same guarantee. | the reducer's post-action trigger pass; `triggers::process_collected_triggers_with_delayed_events` |
| R2 | **A lifelink CR 616.1 pause during stack resolution.** `Paused` today parks under the turn-based action. While a stack entry resolves, the `resolving_stack_entry` / `settle_resolving_stack_entry_after_continuation_resume` lifecycle (`stack.rs:1304-1330`) may expect a continuation. Decide whether `finish_resolving_stack_entry` runs before the park or at the resume. | `stack.rs:37,53`; `combat_damage.rs:332` |
| R3 | **Ending the turn** (CR 724.1b) with the object on the stack: it must cease to exist, and combat teardown must not leave the completeness gate armed. | `effects/end_phase.rs:51`; `turns.rs:269` |
| R4 | **Readers of `entry.source_id`** that assume it names a real object: display, `stack_object_controller` (`stack.rs:1033`), incarnation capture in `push_to_stack_with_firing` (`stack.rs:131`, Activated/Triggered only, so it skips this kind). | grep `\.source_id` over stack readers |
| R5 | **A prevention shield keyed on a source chosen through D6** must match the resolver's `source_id` when the source is LKI-only. | `effects/prevent_damage.rs`, `effects/create_damage_replacement.rs` |
| R6 | **`DamageResult::NeedsChoice => 0`** (`combat_damage.rs:1641`) cites "no player gets priority between combat damage being assigned and dealt". Under `OnStack`, a player *does* get priority, but not during resolution. A replacement choice made during resolution is a choice, not priority, so dropping it stays as today. Re-word the comment so it no longer rests on the modern-only premise. | `combat_damage.rs:1637-1642` |
| R7 | **Auto-pass and phase stops**: a new priority window every combat, each sub-step. `AutoPassMode::UntilStackEmpty` baselines count stack entries. Confirm "pass until end of turn" passes through it, and that the damage object counts like any other stack object for yield scopes (`game_state.rs:25101`). | `priority.rs`, `tests/integration/issue_1969_combat_damage_auto_pass.rs` |

## 8. CR annotation convention for this axis

Current-CR annotations stay mandatory and grep-verified (`validate-cr-annotations`
skill). Historical rules are **not** CR numbers of the current document, so:

- Annotate with the **current** rule the code departs from, and name the
  historical source in prose, e.g.
  `// CR 510.2: modern combat damage doesn't use the stack; under
  CombatDamageTiming::OnStack the pre-M10 rule (1999–2009 CR, rule 310.4)
  puts it on the stack instead.`
  This follows the precedent in `game/mana_burn.rs:1-19`, which cites the
  current glossary entry "Mana Burn (Obsolete)" rather than a retired number.
- Current rules that apply unchanged are annotated normally: CR 117.4, 405.5,
  609.7a, 616.1, 702.4b/702.7b, 724.1b, 400.7, 120.3, 702.15b. All were
  grep-verified on 2026-09-15.
- **Never write `CR 310.x`** in engine code. In the current CR, rule 310 is
  Battles, and the annotation gate would treat the number as real.

## 9. Implementation phases

Each phase is its own PR through plan → plan review → implement → impl review,
like 1a–2cd. `OnStack` stays **unreachable from any selectable format** until
Phase 3: `LegacyAxis::CombatDamageTiming` stays out of
`IMPLEMENTED_LEGACY_AXES` (`types/custom_format.rs:698`) until then. Phases 1–2
are exercised only through `FormatConfig::for_custom_rules` in tests, the
pattern at `legend_scope.rs:78-89`.

### Phase 3a — Stack object and classification (no behavior)

- `StackEntryKind::CombatDamage`, `AssignedCombatDamage`,
  `AssignedDamageRecipient`.
- `StackObjectClass` (D5), replacing `stack_ability_kind`'s `Option`, and every
  §3.6 arm.
- Derived views label and lines, the TS union, the `StackEntry.tsx` label path.
- Protocol bump (§4).
- `game/combat_damage_timing.rs` (§3.9).
- Fix the `CombatDamageTiming` doc comment's "pre-6th-edition" (§0).
- **Tests** (by construction; nothing pushes the object yet):
  - A hand-pushed `CombatDamage` entry is not a legal target for "target
    spell", "target activated ability", "target triggered ability", or
    "target spell or ability". Pair each with an accepted control of the same
    filter against a real spell or ability.
  - It survives a serde round trip.
  - Its `kind_label` is engine-provided.

### Phase 3b — Engine behavior

- The seam (§3.2, exhaustive `match`) and the `combat_damage_on_stack` guard
  (D3).
- The resolver arm and `resolve_combat_damage_object` (§3.3).
- `DamageContext::from_lki` and incarnation-checked dealing (§3.4).
- The D6 source-choice widening.
- R1–R7 traced and resolved in the phase plan.
- **Tests**, class-level. Each asserts `OnStack` behavior **and** a `Modern`
  control on the same board, and each is proven sharp by mutating the fix and
  pasting the failure (see memory: tests here are reviewed on whether they
  discriminate):
  1. *Window exists:* after assignment, `Priority { active }` with exactly one
     `CombatDamage` entry. The modern control reaches Priority with damage
     already dealt and no such entry.
  2. *Source leaves (sacrifice):* damage dealt in full with the LKI controller.
  3. *LKI keywords:* a lifelink source sacrificed with damage on the stack
     still gains its controller life. A deathtouch source bounced with damage
     on the stack still destroys its blocker. Control: `fallback` without the
     fix gains nothing.
  4. *Current characteristics:* lifelink granted after assignment gains life
     (310.4b).
  5. *Power change after assignment:* the amount is unchanged (pump and
     shrink, Morphling-style).
  6. *Recipient bounced or flickered:* not dealt, and no prevention event.
     *Recipient removed from combat but still on the battlefield:* dealt.
  7. *Recipient stops being a creature:* not dealt.
  8. *Prevention created after assignment:* applies, including a
     Circle-of-Protection-class shield choosing an already-sacrificed source
     (D6).
  9. *First strike:* two objects, a priority window after each, no regular
     damage before the first-strike object resolves. 502.2c and 502.28d cases
     (§3.8).
  10. *Ending the turn* with the object on the stack: no damage, clean
      teardown, and the step gate does not re-fire.
  11. *Counter/Stifle-class* effects have no legal target (reachable-flow
      version of the 3a test).
  12. *Multiplayer:* two defending players, APNAP priority round with the
      object on the stack; recipient player eliminated before resolution.
- New tests go in `crates/engine/tests/integration/` (not a top-level test
  binary), in a module next to `mana_burn.rs` and `wish_outside_game_scope.rs`.

### Phase 3c — Release: gate, presets, client and AI polish

- Add `LegacyAxis::CombatDamageTiming` to `IMPLEMENTED_LEGACY_AXES`, and
  re-check every reachability claim against the widened gate (memory:
  widening that list changes what is reachable).
- **Write** `middle_school()` and `classic_magic()`. **Neither constructor
  exists at the pinned commit.** PLAN.md §2 specifies them, and RESEARCH.md
  §1 holds the sourced lists. Register them in `bundled_presets()`. Set codes
  are verified against `set_catalog`, and the banned/restricted rosters are
  asserted by name, not count.
- AI arms from §6 if Phase 3a stubbed them. Run `cargo ai-gate` once, no
  baseline refresh.
- `StackTargetArcs` display of the assignment lines.
- Update `README.md` and `IMPLEMENTATION_PLAN.md` status.

**Size estimate.**
- 3a: medium, and mostly compiler-guided. About 20 exhaustive sites plus the
  classification change's callers.
- 3b: large, and where the rules risk lives. R1 and R2 are the parts most
  likely to take more than one review round.
- 3c: medium, mostly data and tests.

## 10. Open questions

- **Q1 — empty assignment.** When a sub-step has no damage to assign (every
  participant assigns 0), does an object go on the stack? The design says no
  (§3.2). The 2009 rules neither require nor forbid an empty object. If it
  matters for a ruling, it matters only for triggers on putting damage on the
  stack, and none exist. Confirm with a period ruling if one surfaces.
  Otherwise accept "no object" as the least surprising choice.
- **Q2 — who controls the object.** The 2009 CR assigns the combat damage
  object no controller. The design uses the active player for display only.
  Confirm no rules reader (for example "spells and abilities your opponents
  control") ever sees it.
- **Q3 — scope beyond the two presets.** CONTEXT.md's "Lost Legacy 606" also
  lists "Damage uses the stack". It is expressible with this axis and needs no
  extra work, but it is not a bundled preset.

---

*Research inputs for this document:*
- the archived CR texts in §1.1, downloaded 2026-09-15;
- Scryfall API queries (§1.4);
- traces of `combat_damage.rs`, `combat.rs`, `stack.rs`, `priority.rs`,
  `turns.rs`, `choose_damage_source.rs`, `end_phase.rs`, and
  `types/custom_format.rs` at `05c27e0d5`.
