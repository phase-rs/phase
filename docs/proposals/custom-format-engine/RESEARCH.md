---
phase: 58-custom-format-engine
doc: RESEARCH
subsystem: engine-formats
status: research-and-design (no implementation)
---

# Phase 58 — RESEARCH

All findings below are from reading source this session. File:line citations
are given; where a search found nothing relevant that is stated explicitly.

## 1. EC ruleset (re-verified 2026-07-07, raw GitHub fetch)

Fetched `raw.githubusercontent.com/northern-information/lordsofthepit.com/main/src/pages/formats.md`.
Matches the 2026-07-07 background with two clarifications: (a) 93-94's and 95's
**legal-set lists explicitly include Collector's Edition + International
Collector's Edition** as reprint sources; (b) Eternal Chaos's mechanic is
implemented via three "boostie" cards (Booster Tutor ×4, Opening Ceremony ×4,
Summon the Pack restricted to 1) plus errata allowing tutoring from packs opened
during the match.

### Old School 93-94
- **Source:** `raw.githubusercontent.com/northern-information/lordsofthepit.com/main/src/pages/formats.md`, fetched 2026-07-07 (same fetch as the section header above — repeated here so this format's data is independently checkable without scrolling to a shared citation).
- **Legal sets:** Alpha, Beta, Unlimited, Collector's Edition, International
  Collector's Edition, Arabian Nights, Antiquities, Revised, Legends, The Dark,
  Fallen Empires.
- **Restricted (1 copy):** Ancestral Recall, Balance, Black Lotus, Braingeyser,
  Chaos Orb, Channel, Demonic Tutor, Library of Alexandria, Mana Drain, Mind
  Twist, Mox Emerald, Mox Jet, Mox Pearl, Mox Ruby, Mox Sapphire, Recall,
  Regrowth, Sol Ring, Time Vault, Time Walk, Timetwister, Wheel of Fortune.
- **Banned:** Bronze Tablet, Contract from Below, Darkpact, Demonic Attorney,
  Jeweled Bird, Rebirth, Tempest Efreet.
- **Reprint policy:** non-foil reprints with original frame + art, any language;
  no proxies.
- **Legacy rules:** mana burn only. (Plus Chaos Orb / Falling Star flip Oracle,
  and a "no draws" 50-minute Chaos-Orb tiebreaker — tournament-ops, not engine.)

### Old School 95
- **Source:** same fetch as Old School 93-94, above — 95 is published on the
  same page as an incremental extension of 93-94's own lists.
- **Legal sets:** all of 93-94 **plus** Fourth Edition, Ice Age, Chronicles,
  Renaissance, Homelands.
- **Restricted:** 93-94's list **plus** Demonic Consultation, Mana Crypt.
- **Banned:** 93-94's list **plus** Amulet of Quoz, Timmerian Fiends.
- **Legacy rules:** mana burn only.

### Middle School
- **Source:** same fetch as Old School 93-94, above.
- **Legal sets:** Fourth Edition through Scourge (1995–2003).
- **Restricted:** NONE. Instead, 25 cards fully **banned:** Amulet of Quoz,
  Balance, Brainstorm, Bronze Tablet, Channel, Dark Ritual, Demonic
  Consultation, Flash, Goblin Recruiter, Imperial Seal, Jeweled Bird, Mana
  Crypt, Mana Vault, Memory Jar, Mind's Desire, Mind Twist, Rebirth, Strip Mine,
  Tempest Efreet, Timmerian Fiends, Tolarian Academy, Vampiric Tutor, Windfall,
  Yawgmoth's Bargain, Yawgmoth's Will.
- **Reprint policy:** permissive — CE/ICE, world-championship, artist proofs,
  and even modern-bordered reprints "begrudgingly" allowed.
- **Legacy rules:** mana burn **AND** damage-uses-the-stack **AND** pre-M10 wish
  templating (all three).

### Classic Magic
- **Source:** same fetch as Old School 93-94, above.
- **Legal sets:** Alpha through Scourge (1993–2003) — the full pre-Mirrodin pool.
- **Restricted (44 — corrected this round; the prior "(37)" label
  disagreed with the enumerated list below, flagged by maintainer review
  round 2 and recounted directly against this list):** Ancestral Recall,
  Balance, Black Lotus, Black Vise,
  Braingeyser, Burning Wish, Channel, Demonic Consultation, Demonic Tutor, Fact
  or Fiction, Fastbond, Flash, Gush, Imperial Seal, Library of Alexandria,
  Lion's Eye Diamond, Lotus Petal, Mana Crypt, Mana Vault, Memory Jar, Maze of
  Ith, Merchant Scroll, Mind Twist, Mind's Desire, Mox Emerald, Mox Jet, Mox
  Pearl, Mox Ruby, Mox Sapphire, Mystical Tutor, Necropotence, Regrowth,
  Shahrazad, Sol Ring, Strip Mine, Stroke of Genius, Time Walk, Timetwister,
  Tolarian Academy, Vampiric Tutor, Wheel of Fortune, Windfall, Yawgmoth's
  Bargain, Yawgmoth's Will (44 names — the stated count and the enumerated
  list must agree; see PLAN.md §6's new preset-integrity test requiring this).
- **Banned:** Amulet of Quoz, Bronze Tablet, Chaos Orb, Contract from Below,
  Darkpact, Demonic Attorney, Falling Star, Jeweled Bird, Rebirth, Tempest
  Efreet, Timmerian Fiends.
- **Reprint policy:** old-bordered cards only; new-border of any kind illegal
  (proxy or not).
- **Legacy rules:** mana burn AND damage-uses-the-stack AND wish-cycle
  restoration. B&R twice yearly (Jan 1 / Jul 1).

### Eternal Chaos (stretch — not designed in depth)
Built on 93-94 base; adds Booster Tutor ×4, Opening Ceremony ×4, Summon the Pack
×1; errata to tutor from packs opened during the match; sideboard = packs;
one new pack cracked between games; all pack cards removed after match; optional
per-match "Gentleman's Agreement" banlist.

### Swedish Old School 93/94 (the phase-1 target preset — see CONTEXT.md)

**A different, real ruleset from the four EC formats above — not the same
source, not a duplicate.** Given its own section here, matching the EC
formats' treatment, because it's the actual first preset this proposal ships
(§2/§8) and deserves the same direct, checkable citation.

- **Source:** `http://oldschool-mtg.blogspot.com/p/banrestriction.html`,
  fetched directly this session (2026-07-15) via `WebFetch`, not from
  memory or a secondary summary. Distinct from the EC ruleset above — Sweden
  is the historical origin community for "Old School 93/94" as a movement,
  and its published rules differ from EC's in real, substantive ways (see
  below), not just presentation.
- **Legal sets:** Alpha, Beta, Unlimited, Arabian Nights, Antiquities,
  Legends, The Dark, "Summer Magic" (a distinct, small 1994 print run — its
  MTGJSON set code is unconfirmed pending implementation-time verification,
  same caveat PLAN.md §2 already carries for every set code in this
  proposal).
  - Also stated by this source: **"Only English versions are allowed in
    Oldschool"** — a language restriction with no current engine
    representation; not modeled by this proposal (out of scope, same as the
    already-noted physical-tournament logistics in the Classic
    Legacy/"Lost Legacy" section below).
- **Banned:** NONE — the source states no card is fully banned under the
  Swedish ruleset, a genuinely empty list (distinct from EC's 93-94, which
  bans 7 named cards).
- **Restricted (25, one-copy maximum):** Ancestral Recall, Balance, Black
  Lotus, Braingeyser, Channel, Chaos Orb, Contract from Below, Darkpact,
  Demonic Tutor, Library of Alexandria, Mana Drain, Mind Twist, Mishra's
  Workshop, Mox Emerald, Mox Jet, Mox Pearl, Mox Ruby, Mox Sapphire,
  Regrowth, Sol Ring, Strip Mine, Tempest Efreet, Time Walk, Timetwister,
  Wheel of Fortune — a **different 25-name list from EC's 93-94's 22-name
  list above** (compare directly: EC includes Recall and Time Vault, absent
  here; Swedish includes Contract from Below, Darkpact, Demonic Tutor,
  Library of Alexandria, and Mishra's Workshop, absent from EC's list) —
  confirms this is a genuinely independent ruleset, not a re-presentation of
  EC's.
- **Ante cards** (a THIRD list-shaped rule, distinct from banned/restricted —
  see CONTEXT.md Open item 5): "must be removed before play unless the
  tournament is specifically played for ante" — Bronze Tablet, Contract
  from Below, Darkpact, Demonic Attorney, Jeweled Bird, Rebirth, Tempest
  Efreet.
- **Reprint policy:** NOT stated by this source beyond "Only English
  versions are allowed" — see CONTEXT.md Open item 6. A secondary source
  (`mtgoldframe.com`) claims no Revised-or-later reprints are allowed, but
  that claim was not independently verified against this primary source and
  must not be encoded without doing so.
- **Legacy rules:** NO mention of mana burn, damage-on-the-stack, old Wish
  templating, or a modified legend rule anywhere in this source — the plain
  reading is fully modern rules on a restricted-era card pool, confirmed by
  the ABSENCE of any of the legacy-rule language the EC sections above all
  state explicitly for their own formats. This is why `swedish_old_school()`
  (PLAN.md §2) sets every `LegacyRuleSet` axis to its `Modern` default and
  needs none of §4's engine wiring.

## 2. Legality-pipeline gap — CONFIRMED absent

- `card_db.rs:109` — `let normalized = normalize_legalities(&entry.legalities);`
  is the single ingest site. Source is MTGJSON `AtomicCard.legalities`
  (`oracle_gen.rs:778/814/883`).
- `LegalityFormat` (`legality.rs:12-28`) has 15 variants; **none** are
  oldschool/middleschool/classic. `from_key` (`legality.rs:69-88`) returns
  `None` for any unlisted key, and `normalize_legalities` (`legality.rs:125-137`)
  `continue`s past `None` — the key is dropped.
- The test at `legality.rs:286-313` uses `"oldschool"`-shaped names as the
  canonical example of "unknown keys are dropped".
- **Conclusion:** the four EC formats have zero per-card legality signal in the
  pipeline, and even if MTGJSON published one for `oldschool` it wouldn't
  survive normalization today. The custom-format engine must define its own
  legal-set-code + banned/restricted-name mechanism and must NOT depend on
  external per-card legality data. This is *correct* — Scryfall/MTGJSON cannot
  be relied on to track player-authored custom formats.

## 3. Set-membership + reprint policy — building block confirmed, one data gap

- `CardDatabase::printings_for(name) -> Option<&[String]>` (`card_db.rs:227-230`)
  returns set codes; backed by `printings_index` populated from the export
  `printings` field (`card_db.rs:101-103`).
- **This is the enforceable legality key**: a card is pool-legal for a custom
  format iff at least one of its printings' set codes is in the format's
  `legal_sets` list.
- **Data gap:** printings are *set codes only* — there is **no per-printing
  frame/border/art metadata** in the runtime DB. So "original frame/art only"
  (93-94) vs "old-border only" (Classic) vs "modern border begrudgingly allowed"
  (Middle School) are **not fully distinguishable** from current data. However,
  set-code membership is a *good approximation*: a modern reprint of an Alpha
  card lives in a modern set code, which is simply not in `legal_sets`, so it is
  excluded automatically. The reprint-policy nuance mainly affects special
  reprint set codes (CE/ICE/world-championship/artist-proof), which are
  themselves distinct set codes and can be added/omitted from `legal_sets` per
  format. **Recommendation:** model reprint policy as *which set codes are in the
  legal list*, and flag frame/art-level fidelity as a known limitation (needs a
  new per-printing data field if we ever want to enforce it precisely).

## 4. `DeckCopyLimit::UpTo(1)` reuse — feasible for the ceiling, WRONG layer for the source

Read `deck_validation.rs` copy-limit + restricted paths in full.

- `DeckCopyLimit` (`format.rs:100-105`) is a **card-intrinsic** override: it
  models what an individual card's *own rules text* says ("A deck can have up to
  N cards named ~", "only one copy"). It is resolved per-card from
  `face.deck_copy_limit` / Oracle text in `copy_limit_violations`
  (`deck_validation.rs:2412-2449`, esp. the `UpTo(n)` arm at 2434-2435).
- A format **restricted list** is a *format-level policy*, not a property of the
  card. Overloading `DeckCopyLimit::UpTo(1)` to carry it would conflate two
  abstraction layers — exactly the smell CLAUDE.md's "separate abstraction
  layers in enum design" warns against. (It would also break for a card that has
  BOTH a real intrinsic limit and a format restriction.)
- **The right existing building block already exists**:
  `restricted_copy_violations` (`deck_validation.rs:2462-2485`) enforces the CR
  100.2b `<= 1` ceiling **format-generally**, driven by a `restricted_canonical:
  HashSet<String>` of names. Today that set is populated from
  `LegalityStatus::Restricted` (`deck_validation.rs:388-390`). For a custom
  format we simply populate the *same* set from the format's restricted-name
  list, and reuse the *same* enforcer. Zero new copy-limit concept needed.
- **Finding:** reuse `restricted_copy_violations` (the enforcement path), NOT
  `DeckCopyLimit::UpTo`. Correct the task's framing accordingly.

## 5. Mana burn — hook point identified (REVISED TWICE — maintainer review rounds 2 and 3)

The first pass got two things wrong (round 2 fixed both in the *description*
but not fully in the *mechanism*); round 2's fix itself was then found
incomplete (round 3). Both rounds' findings, current state only:

- **It's life loss, not damage** (round 2, still correct). Modern CR: **mana
  burn is obsolete.** Glossary "Mana Burn (Obsolete)": "Older versions of the
  rules stated that unspent mana caused a player to **lose life**… That rule
  no longer exists." (Removed by the 2010 "M10" rules update.) Life loss and
  damage are behaviorally distinct in this engine (damage can be
  prevented/redirected and fires "dealt damage" triggers; life loss does
  neither).
- **It's per real MTG phase, not per engine `Phase` transition — and this
  requires the mana POOL to persist across intra-phase-group steps, not just
  the burn CHECK to skip them (round 2 only did the latter; round 3 caught
  that this is insufficient).** The engine's `Phase` enum (`types/phase.rs`)
  is a flat 11-variant list representing BOTH MTG's 5 real phases and their
  internal steps as siblings (e.g. `DeclareAttackers`, `DeclareBlockers`,
  `CombatDamage` are three separate `Phase` variants, all inside the single
  real "Combat" phase). Modern CR 500.5 empties the mana pool at **every**
  one of these transitions unconditionally — round 2 left this unconditional
  draining untouched and only gated the life-loss side-effect to phase-group
  boundaries, which means by the time a phase-group boundary was reached the
  pool had already been silently emptied at the prior intra-phase-group step,
  with nothing left to burn.

**Corrected mechanism (round 3) — reuses an existing pattern instead of
gating a side-effect on an unconditionally-firing event:**

- The engine already has a generic mana-persistence mechanism for exactly
  this shape: `ManaExpiry` (`types/mana.rs:1509`) has `EndOfTurn` and
  `EndOfCombat` variants. `EndOfCombat`'s own doc comment: "Mana persists
  through combat steps but drains at EndCombat → PostCombatMain," used by
  Firebending — i.e. "persist through this phase's internal steps, drain at
  the real phase-group boundary," already built and tested, just
  special-cased to the Combat phase-group. Units carrying an active expiry
  are excluded from the `Drop` disposition entirely
  (`turns.rs`, the `u.expiry.is_none()` filter feeding
  `UnitDecision` construction) — they never reach the empty-pool pipeline
  while their duration is live.
- **The fix generalizes this**, rather than gating the life-loss check on an
  event that already ran unconditionally: add `ManaExpiry::EndOfPhaseGroup`
  (one new variant, not five per-phase-group ones — it resolves contextually
  against whichever phase-group is active, exactly as `EndOfCombat`/
  `EndOfTurn` already do without parameterizing which combat/turn). Mana
  added while `LegacyRuleSet.mana_burn` is set is tagged with this expiry
  instead of `None` at construction. The existing expiry-clearing logic
  (`turns.rs`'s generalization of
  `clear_expired_end_of_combat_retention_markers`, which already runs where
  the pre-transition phase is still available — `state.phase = next` doesn't
  overwrite it until later in `enter_phase`) converts these units to
  ordinary (`None`-expiry) units only when a real phase-group crossing is
  detected, at which point they flow into that SAME transition's
  already-firing `EmptyManaPool` event as `Drop` decisions.
- Because units never reach `Drop` except at a real phase-group crossing (by
  construction, mirroring exactly how live `EndOfCombat` units are excluded
  mid-combat today), the drop count at any transition where a `mana_burn`
  player's units DO drop *is* the burn amount — apply it as life loss at the
  same aggregation point `apply_empty_mana_pool_event` already uses for the
  existing `player_unspent_mana_loss_causes_life_loss` (Yurlok-class,
  `static_abilities.rs:1237`, `StaticMode::UnspentManaLossCausesLifeLoss`)
  check, independent of it (a format flag and a card-granted static ability
  are different triggers that could both apply to the same event), and after
  any pause the drain takes for a player choice (Kruphix/Horizon Stone `Keep`
  dispositions) — reusing the pipeline's existing pause/resume point rather
  than computing before the choice is known.
- **Size: still small, slightly larger than round 2's estimate.** One new
  `ManaExpiry` variant, its construction-site tagging, and its
  clearing-logic wiring — plus the life-loss application at the existing
  aggregation point, distinguishable via `GameEvent::ManaBurn { player_id,
  amount }`. No new state machine; reuses the per-unit disposition, APNAP
  drain, and replacement-pipeline infra that already exists for
  `EndOfCombat`/`EndOfTurn` and the Yurlok-class check.

## 6. "Damage uses the stack" — LARGE / deep; honest assessment

- Modern combat damage is **simultaneous and explicitly does NOT use the
  stack**: "CR 510.2 … This turn-based action doesn't use the stack. No player
  has the chance to cast spells or activate abilities between the time combat
  damage is assigned and the time it's dealt."
- The engine implements exactly this modern model in
  `game/combat_damage.rs` (**3,658 lines**) as a simultaneous batch
  (`resolve_combat_damage`, `combat_damage.rs:102`; batch/replacement machinery
  throughout — see the "batch" references at 814/832/862/926). `combat.rs` is a
  further 10,821 lines. `engine_combat.rs::handle_assign_combat_damage`
  (`engine_combat.rs:538`) drives assignment then calls
  `resolve_combat_damage` immediately — there is **no intermediate
  priority/stack round** between assignment and dealing.
- Pre-6th-edition "damage on the stack" put assigned combat damage **on the
  stack as a stack object** that players could respond to before it resolved.
  That is a *reversal of CR 510.2's control flow* and touches the combat step
  state machine, the stack, and priority passing — not a leaf value.
- **Size: large, and I will not guess a smaller number to look complete.** There
  is no existing hook-point for interposing priority between damage assignment
  and dealing; the immediate-dealing path is load-bearing across thousands of
  lines and hundreds of tests. This is plausibly a multi-week combat rework and
  should be treated as its own sub-project, gated behind
  `LegacyRuleSet.damage_timing: CombatDamageTiming::OnStack`, and very likely
  **out of the initial MVP**. **Per PLAN.md §7's preset-readiness gate
  (tightened in maintainer review round 4): Middle School and Classic Magic
  may NOT be registered as selectable formats until this fully lands — there
  is no "playable without it, with a caveat" interim state.** This retracts
  the framing this section originally had.

## 7. Analogous Trace — `GameFormat::Limited` (phase 53 prior art)

From `git show 80404a98b:.planning/phases/53-limited-draft-core/53-01-SUMMARY.md`:

Adding `GameFormat::Limited` was a single, tightly-scoped TDD change:
- Added `Limited` to `FormatGroup` and `GameFormat`.
- Added `FormatConfig::limited()` (40-card, 20 life, 2-player).
- Extended **all** exhaustive match arms: `legality_format` (`None`),
  `sideboard_policy` (`Unlimited`), `label`, `for_format`, plus the registry.
- The one "surprise" was a second exhaustive match in
  `deck_validation.rs::format_compatibility_check` that also required an arm —
  the compiler caught it (non-exhaustive match error), and it was added as a
  pass-through.
- RED→GREEN→(no REFACTOR); 8 new tests; full engine suite stayed green.

**Lessons this phase inherits:**
1. An additive `GameFormat` variant is a well-trodden, compiler-guided path —
   every exhaustive match is a checklist the compiler enforces.
2. There are **two** exhaustive-match sites to remember: `format.rs` and
   `deck_validation.rs`. Grep for `match .*format` / `GameFormat::` before
   assuming five arms.
3. Structural params (life/deck/players) live in `FormatConfig`; legality is a
   separate axis (`legality_format` → `LegalityFormat`). Our custom layer must
   respect that separation but *replace* the `LegalityFormat` axis with a
   set-list + name-list payload, because custom formats have no `LegalityFormat`.

## 8. Idiomatic-Rust note on the legacy-rules axis

Mana burn, damage-uses-stack, and pre-M10 wish templating are **independent**
binary rule-modules (93-94 = burn only; Middle School/Classic = burn + stack +
wish). They are not mutually exclusive and not orderable, so an `enum` is the
*wrong* model (it cannot express "burn + wish, no stack" without enumerating
2^N combinations). The idiomatic model is a small struct of documented `bool`
fields (or `bitflags`), each gating one independent rules module — this is the
one place a bool cluster is correct precisely because each axis is a separate
CR-era toggle. This satisfies CLAUDE.md's intent ("mana burn and
damage-uses-stack do NOT travel together… never a single old-rules boolean")
while staying honest that the members are independent switches, not a
parameterization of one axis.

## 9. Pre-M10 Wish templating — SMALL (building block already exists); REAL functional difference

This flag was named in §8 and the `LegacyRuleSet` struct but never investigated
in the first pass. This section closes that gap with the same rigor as §5/§6.

### 9a. What the EC ruleset actually requires (re-fetched 2026-07-07)

Re-fetched `raw.githubusercontent.com/northern-information/lordsofthepit.com/main/src/pages/formats.md`.
Both Middle School and Classic Magic restore the Judgment Wish cycle to
pre-M10 function, worded verbatim as:

> "…Cunning Wish, Burning Wish, Living Wish, Death Wish, and Golden Wish were
> originally able to find an appropriate card that had either been removed from
> the game, or was located in your sideboard. The Wish cycle functionality has
> been restored to allow this."

So the required behavior is explicit: a Wish may retrieve a matching card the
player owns **that has been removed from the game** (modern: exile), in
addition to the sideboard.

### 9b. What the M10 (Magic 2010, July 2009) update actually changed — REAL, not wording-only

The M10 rules update renamed the **"removed from the game" zone to "exile"** and
made exile a *defined in-game zone*. Sources:

- MTG Wiki "Wish" / Magic Judges rules tips (`blogs.magicjudges.org/rulestips/2013/01/you-cant-burning-wish-for-exiled-cards/`):
  "the exile zone is a zone in the game, those cards aren't outside of the game,
  so you can no longer Wish for those cards." Before M10, Wishes could acquire
  a card from the sideboard **or** a card that had been "removed from the game";
  after M10, only the sideboard qualifies.
- Current CR confirms the *modern* boundary this flag reverts:
  - **CR 400.11**: "An object is outside the game if it isn't in any of the
    game's zones. **Outside the game is not a zone.**"
  - **CR 400.11a**: "Cards in a player's sideboard are outside the game."
  - **CR 701.23j**: "If an effect instructs a player to search outside the
    game for a card, that player may choose an appropriate card they own from
    outside the game."
  - Exile (CR 406) is a normal in-game zone, so exiled cards are *not* "outside
    the game" and are ineligible for a modern Wish.

**Verdict: this is a genuine functional / gameplay difference, NOT a
wording-only templating change.** It changes the *set of legal choices offered
during resolution*: a card exiled during the game (e.g. by the Wish's own
"Exile ~" clause, by cycling/foretell-era effects, by an opponent's exile
removal, etc.) is a legal Wish target pre-M10 and an illegal one post-M10. That
is directly observable at the table, so the engine must model it — it is not a
no-op flag. (The label "templating" in the flag name is therefore a slight
misnomer; see §9e and the PLAN naming note.)

### 9c. phase.rs already implements the Wish cycle — in modern (post-M10) form

The engine has a full, general outside-the-game search effect, and the M10
distinction is already a first-class typed axis:

- **`Effect::SearchOutsideGame { filter, count, reveal, destination, source_pool }`**
  (`types/ability.rs:10347-10356`).
- **`OutsideGameSourcePool`** (`types/ability.rs:246-260`) is exactly the M10
  boundary as a typed enum:
  - `Sideboard` (default) — "CR 400.11a: Tournament sideboard / casual
    outside-the-game collection." This is the **post-M10 Wish** pool.
  - `SideboardAndFaceUpExile` — "CR 400.11a + CR 406.3: Sideboard plus matching
    owned face-up exile." Used today for the modern **Karn, the Great Creator /
    Coax from the Blind Eternities** class, whose Oracle text has an explicit
    "…or choose a face-up … card you own in exile" disjunction.
  - Helper `includes_face_up_exile()` (`ability.rs:257-259`).
- **Parser** (`parser/oracle_effect/imperative.rs:2711-2818`,
  `parse_search_and_creation_ast`): "reveal/play/cast a … card you own from
  outside the game" lowers to `SearchOutsideGame`. A single-branch wording gets
  `source_pool: Sideboard`; only the explicit second "…or choose a face-up …
  in exile" branch (`parse_face_up_exile_branch`, `imperative.rs:2763-2768`)
  produces `SideboardAndFaceUpExile`. Wish-cycle cards therefore parse to the
  **sideboard-only (post-M10)** pool today, verified by tests:
  - `parse_outside_game_wish_reveal_to_hand` (`imperative.rs:12312`),
    `parse_outside_game_legacy_single_branch_still_works` (`:12461`),
    `parse_outside_game_wish_play_from_sideboard` (`:12489`, the M19 "Wish"
    card end-to-end through `parse_oracle_text`).
  - `swallow_check.rs:4331` `optional_you_may_accepts_wishboard_creature_or_land…`
    parses **Living Wish** by name; `swallow_check.rs:4143` parses a Burning-Wish-
    shaped sorcery fetch. Karn is contrasted at `swallow_check.rs:4349`.
- **Resolver** (`game/effects/search_outside_game.rs`): `resolve` builds the
  sideboard candidate list (`:36-67`, CR 400.11a) and — **only when
  `source_pool.includes_face_up_exile()`** (`:72`) — appends
  `collect_face_up_exile_candidates` (`:105-135`), which already selects exile
  objects the controller **owns** and that are **face-up** (`face_down` filtered
  out at `:122`) and match the filter. Movement into hand/destination is handled
  by `put_face_up_exile_into` (`:141-186`) through the standard `ChangeZone`
  pipeline. This is the entire pre-M10 retrieval mechanism, already built and
  tested (`karn_minus_two_pulls_face_up_exile_artifact_to_hand`,
  `search_outside_game.rs:654`).

Searched `crates/engine` for `burning wish|cunning wish|living wish|golden
wish|death wish|glittering wish` — matches only in `search_outside_game.rs`
(test scaffolding names) and `swallow_check.rs` (Living Wish parse test); no
per-card special-casing anywhere. The class is handled generically by the
`SearchOutsideGame` parser pattern, so the real named cards
(Burning/Cunning/Living/Golden/Death Wish) parse via the same path when present
in the generated `card-data.json`.

### 9d. Why the modern face-up-exile pool is a rules-faithful model of pre-M10 RFG

Pre-M10 "removed from the game" cards that a Wish could fetch were, in practice,
exactly *cards the player owns that are visible* — you cannot choose a card you
don't own, and hidden (face-down) removed cards were never eligible Wish
targets. That is precisely what `collect_face_up_exile_candidates` already
enforces (`owner == controller` + `!face_down`). So the pre-M10 rule is
faithfully expressed by *widening a Wish-class search's effective pool to the
already-existing `SideboardAndFaceUpExile` behavior* — no new candidate-
collection logic, no new zone, no new movement path.

### 9e. Hook point and size — SMALL (revise PLAN's "Medium")

`GameState` already carries the resolved format config:
`GameState.format_config: FormatConfig` (`types/game_state.rs:6787`), and PLAN
§1 places `LegacyRuleSet` under `FormatConfig.custom_rules`. The resolver has
direct access to it.

**The entire change is one flag check at one existing hook.** In
`search_outside_game::resolve`, the exile-append condition at
`search_outside_game.rs:72` becomes, in effect:

```text
if source_pool.includes_face_up_exile()
    || state.format_config.custom_rules's legacy.wish_scope == WishOutsideGameScope::PreM10ReachesExile
{
    choices.extend(collect_face_up_exile_candidates(state, ability, filter));
}
```

That is: when the legacy flag is on, treat a Wish-class (`Sideboard`) search as
if it were `SideboardAndFaceUpExile`. No parser change (the parser can't know
the format anyway — the flag is a *runtime resolution* concern, not a parse
concern), no new effect, no new state, no new WaitingFor variant, and full reuse
of the tested face-up-exile collector and mover.

**Size: SMALL — and materially smaller than PLAN §4's current "Medium"
estimate.** The Medium framing predated discovering that
`SideboardAndFaceUpExile` + `collect_face_up_exile_candidates` already implement
the retrieval end-to-end. This should be re-classified alongside mana burn
(§5) as a small, well-contained addition; unlike damage-on-stack (§6) it touches
no state machine.

**One CR-annotation note for implementation:** the pre-M10 behavior predates the
current CR 400.11 zone model, so annotate the flag as a *legacy rule reverting
the M10 change* — cite CR 400.11 / 400.11a (the modern boundary being relaxed)
and CR 701.23j, exactly as mana burn cites the obsolete-glossary entry. The flag
does **not** implement current CR; it deliberately re-enables removed behavior.

**Naming history (resolved — canonical field is `wish_scope:
WishOutsideGameScope`, its `PreM10ReachesExile` variant is what this section
means throughout, including the pseudocode above).** Two renames, both
tracked here so a reader hitting an old name in a stray comment isn't
confused: the first-pass placeholder `pre_m10_wish_templating` (a bool)
mislabeled a *functional pool-scope* toggle as a *wording* one, so it became
`pre_m10_wish_reaches_exile` (still a bool) during round 1. That name was
itself superseded during the round-4 typed-enum conversion (CONTEXT.md
"Maintainer review round 4," point 3) — the field is `wish_scope` and the
type is `WishOutsideGameScope` now. An intermediate maintainer-review pass
fixed this section's references to the ROUND-1 rename but missed that the
ROUND-4 rename needed the same reconciliation — caught by an automated
review pass on the following commit and fixed here in the same pass. This is
not a no-op / wording-only flag; it is a real, testable pool-widening.

## 10. Legend rule — historical scope change is REAL; engine hardcodes modern; flag is SMALL

The "legend rule" (colloquially "the legend rule") was introduced by the
*Legends* set (1994) — a set legal in EC's Old School 93-94 and 95 — and its
governing rule has a documented history of change. Investigated with the same
rigor as §5/§6/§9. All dates/wordings below are web-verified (not from memory);
the current CR wording is verified directly against
`docs/MagicCompRules.txt` in this checkout.

### 10a. What the rule said at each point in history (web-verified)

Sources: WotC "Legendary Rule Change" (magic.wizards.com, 2013-05-23, via the
2013-07 M14 rules-tips summary); MTG Wiki "Legendary/Legend rule"; Magic Judges
rules-tips "M14 Rules changes! Legendary permanents"
(blogs.magicjudges.org/rulestips/2013/07/m14-rules-changes-legendary-permanents/).

- **Legends (1994) → pre-M14 (through 2013): NOT per-controller — global.** If
  two or more legendary permanents with the *same name* were on the battlefield,
  they were affected **regardless of which player(s) controlled them**. The exact
  mechanic shifted across eras (early "first legend in play trumps / newest is
  denied"; Sixth Edition 1999 unified it across all legendary permanent types
  with the "both go to the graveyard" form; *Champions of Kamigawa* 2004 changed
  it to a "nullification" variant), but the invariant across every pre-M14 form
  is that **same-named legends interacted across controllers** — two different
  players could not each keep their own copy of the same legendary permanent.
- **Magic 2014 (effective 2013-07-13, the M14 prerelease): today's
  per-controller rule.** Rewritten so that only when *a single player* controls
  two or more legendary permanents with the same name does that player choose one
  to keep and put the rest into their owners' graveyards; it **does not** affect
  same-named legendaries controlled by *different* players. This is the version
  the engine implements (see §10c).
- **Ixalan (effective 2017-09-28, day before release; NOT Dominaria 2018):
  planeswalker uniqueness folded into the legend rule.** Before Ixalan,
  planeswalkers used a separate **"planeswalker uniqueness rule"** keyed on
  planeswalker *type* (a player couldn't control two planeswalkers sharing a
  type, e.g. two different "Jace" cards). Ixalan removed that rule, gave all
  past/present/future planeswalkers the `legendary` supertype via Oracle errata,
  and made them subject to the *name*-based legend rule instead. (The task's
  "around 2018's Dominaria" guess is **incorrect** — the correct set/date is
  Ixalan, 2017-09-28. Dominaria 2018 is unrelated to this change.)

### 10b. Current CR wording — verified against `docs/MagicCompRules.txt`

The legend rule is a **state-based action**, CR **704.5j** (verified by grep;
the number is not guessed):

- **CR 704.5j**: "If two or more legendary permanents with the same name are
  controlled by the same player, that player chooses one of them, and the rest
  are put into their owners' graveyards. This is called the 'legend rule.'"
  (Note "**by the same player**" — the modern per-controller scope, in the rule
  text itself.)
- Glossary "Legend Rule": "…causes a player who controls two or more legendary
  permanents with the same name to put all but one into their owners'
  graveyards."
- **CR 205.4d** cross-references 704.5j.
- **Planeswalker uniqueness is confirmed obsolete in the current CR:**
  **CR 306.4**: "Previously, planeswalkers were subject to a 'planeswalker
  uniqueness rule'… This rule has been removed and planeswalker cards printed
  before this change have received errata… to have the legendary supertype… they
  are subject to the 'legend rule' (see rule 704.5j)." And the glossary entry
  "Planeswalker Uniqueness Rule (Obsolete)".
- (Contrast: the world rule, CR 704.5k, is a distinct global/choiceless SBA —
  a useful structural precedent, see §10d.)

### 10c. Is it a REAL functional difference? — YES

**Verdict: a genuine functional / gameplay difference, not wording-only** — same
class as mana burn (§5) and pre-M10 Wish (§9). Under the pre-M14 rule, two
*different* players could **not** simultaneously keep two copies of the same
legendary permanent (the second to resolve, or both, would be put into the
graveyard as an SBA — no controller kept one). Under the modern rule each player
independently keeps one. This changes the *set of legal board states*, directly
observable at the table (e.g. a mirror-match where both players resolve the same
legend: pre-M14, both die; modern, each keeps one). It is not a no-op flag.

**Relevance to the four EC presets — HONEST CAVEAT.** EC's published rulesets
(§1, re-fetched 2026-07-07) list their legacy-rule exceptions explicitly per
format (93-94/95 = mana burn only; Middle School / Classic = mana burn +
damage-on-stack + wish). **None of the four lists a legend-rule reversion**, so
the four EC presets play the *modern* legend rule and set this flag to its
default. The legend-rule scope is therefore a **general historical-rules axis**
the custom-format engine should be able to express (exactly the orthogonality
argument Block Constructed makes for `mana_burn`, PLAN note), **not** a behavior
any of the four current presets turns on. Do not silently flip it on for the EC
presets. Two further points confirm the EC-scope impact is minimal today:
(i) *planeswalker uniqueness is entirely moot for these formats* — all four pools
top out at Scourge (2003), and planeswalkers did not exist until Lorwyn (2007),
so no EC-legal card is a planeswalker and no separate planeswalker-uniqueness
flag is needed for this project; (ii) the legend-rule scope difference only
manifests in cross-controller same-name mirrors, which are rare in singleton-ish
retro metagames but *are* legal board states, so modeling it is correct-for-the-
class even if the four presets default it off.

### 10d. Engine implementation — HARDCODED to the modern (per-controller) rule

Searched `crates/engine/src/game/` for "legend"; the SBA lives in `sba.rs`.

- **`check_legend_rule` (`sba.rs:902-956`) is unambiguously per-controller.** It
  loops **per player** (`for player_idx in 0..state.players.len()`, `:910`),
  filters candidates to `obj.controller == player_id` (`:920`), groups by name
  **within that one controller's permanents** (`by_name`, `:935-940`), and for
  any name with ≥2 pauses with `WaitingFor::ChooseLegend { player, legend_name,
  candidates }` (`:950-954`) so the controller *chooses* one to keep. Both the
  cross-controller-independence and the player-choice are the modern M14 rule.
  Annotated CR 704.5j at `:167`, `:899`, `:924`.
- The exemption path is separate and reusable: `legend_rule_exempt` /
  `legend_rule_exempt_with_gate` (`sba.rs:864-897`) consult the
  `LegendRuleDoesntApply` static (Mirror Gallery / Sakashima / Mirror Box); it is
  the single authority and already gates the candidate filter (`:926`).
- **No legacy scope toggle exists anywhere** — there is no flag, enum, or config
  read in `check_legend_rule`; the per-controller behavior is structural.

### 10e. Size of re-adding the pre-M14 (any-controller) rule as a legacy flag — SMALL

The pre-M14 form is actually *structurally simpler* than the modern one: it is
**global** (one group per name across all controllers) and **choiceless** (all
same-named legendaries in a ≥2 group go to their owners' graveyards, no player
selection, no `WaitingFor`). The engine **already has this exact shape** in
`check_world_rule` (`sba.rs:1348`, CR 704.5k — global, choiceless) and
`check_role_uniqueness` (`sba.rs:1190`), and it already has the SBA-departure
mover used by every SBA (`move_sba_departing_permanent`, CR 704.5 zone-pipeline
mover at `sba.rs:618`). So the change is:

1. Add a scope field to `LegacyRuleSet` (PLAN §1) — a **typed enum**, not a bool
   (per CLAUDE.md "typed enum over raw bool"), because the historical space has
   more than a clean binary and a typed enum leaves room without a later refactor:
   `LegendRuleScope { Modern, PreM14AnyController }` (default `Modern`).
2. Branch once at the top of `check_legend_rule`: when the resolved
   `GameState.format_config` legacy scope is `PreM14AnyController`, build the
   name groups **without** the `obj.controller == player_id` filter (all
   controllers) and, for any ≥2 group, route every member through the existing
   `move_sba_departing_permanent` — mirroring `check_world_rule`'s
   global-choiceless structure — instead of pausing with `WaitingFor::ChooseLegend`.
   The `legend_rule_exempt_with_gate` filter is reused unchanged.

No new WaitingFor variant, no new mover, no new state machine, full reuse of the
world-rule choiceless-global precedent and the shared SBA departure pipeline.
**Size: SMALL** — comparable to mana burn (§5) and pre-M10 Wish (§9); materially
smaller than damage-on-stack (§6). The only design nuance is scoping the enum:
model just `Modern` vs a single consolidated `PreM14AnyController` "all
same-named copies across all controllers go to the graveyard" form (the Sixth-
Edition "both die" version, which is the one that changes cross-controller legal
board states); do **not** attempt to reproduce the era-specific "first-in
trumps" / Kamigawa "nullification" micro-variants — they are out of scope and a
future enum variant can carry them if a format ever needs one. Because the flag
re-enables a rule the current CR replaced (M14), annotate it as a *legacy rule
reverting the M14 change* and cite CR 704.5j (the modern boundary being relaxed),
exactly as mana burn cites the obsolete-glossary entry and the Wish flag cites
CR 400.11.
