# Implementation Plan (REVISED, round 2): MSH Wave 6 — Defender-Scoped "Can't Attack You or [Permanents/Planeswalkers] You Control"

Produced by `/engine-planner`. Round-2 revision addressing reviewer findings B1–B4 + parser/verification re-statement. **Every file:line anchor below was re-verified by reading the cited file in this session.** Anchors that drifted from round 1 are corrected and flagged.

---

## Cards & exact gap (re-verified against `client/public/card-data.json`)

| Card | Oracle clause (the gap) | Current parse state (verified) |
|---|---|---|
| **Willie Lumpkin, Postman** | "…you draw a card and that player may draw a card. **If they do, that player can't attack you or permanents you control during their next turn.**" | Trigger `DamageDone` → sub-chain `Draw(Controller)` → `Draw(TriggeringPlayer, optional)` → **`Effect::Unimplemented { name:"can't", description:"can't attack you or permanents you control during their next turn" }`** gated by `condition: EffectOutcome{OptionalEffectPerformed}`. Verified in card-data. |
| **Promise of Loyalty** | "Each player puts a vow counter on a creature they control and sacrifices the rest. **Each of those creatures can't attack you or planeswalkers you control for as long as it has a vow counter on it.**" | Sorcery (`core_types:["Sorcery"]`). Misparse: top-level `static_abilities:[{mode:CantAttack, affected:SelfRef, condition:null}]` on the **sorcery** (inert — sorceries never persist on the battlefield), defender scope DROPPED, plus `parse_warnings:[{SwallowedClause, detector:"Condition_AsLongAs"}]`. Verified in card-data. |

Both share ONE construct: a **defender-scoped attack restriction** ("can't attack you or [permanents | planeswalkers] you control"). They diverge on TWO orthogonal axes:
- **Subject scope**: Willie = **player-scoped** ("that player" → every creature that player controls); Promise = **per-object** ("each of those creatures" → the specific vow-counter recipients).
- **Duration**: Willie = "during their next turn" (the restricted player's next turn; CR 514.2 + CR 500.7); Promise = "for as long as it has a vow counter on it" (CR 611.2b).

---

## B1 REBUTTAL — keep `AttackTargetFilter::PlayerOrPermanents`; do NOT collapse onto `PlayerOrPlaneswalker`

**Reviewer B1 said:** drop the new variant; map both cards to the existing `PlayerOrPlaneswalker`.

**Verdict: REJECTED. B1 is wrong and is self-contradictory with B2.** Evidence:

- `AttackTarget` (`combat.rs:33`, re-verified) has exactly three inhabitants: `Player(PlayerId)`, `Planeswalker(ObjectId)`, `Battle(ObjectId)`. Battles ARE attackable (CR 506.3: "Only a player, a planeswalker, or a battle can be attacked"; verified) and the engine declares them (`combat.rs:2134` `AttackTarget::Battle` validation, re-verified).
- "you or **permanents** you control" = you + every attackable permanent you control = Player + Planeswalker + **Battle** (a battle is a permanent — CR 506.3 enumerates it as an attack target). "you or **planeswalkers** you control" = Player + Planeswalker only, **excluding battles**.
- The concrete distinguishing `(filter, AttackTarget)` pair the reviewer demanded:
  - **`(PlayerOrPlaneswalker, AttackTarget::Battle) ⇒ false`** (a planeswalker-only restriction must NOT block attacking your battle).
  - **`(PlayerOrPermanents, AttackTarget::Battle) ⇒ true`** (a permanents restriction MUST block attacking your battle).
- Collapsing them forces one of two bugs: Willie UNDER-restricts (a restricted opponent could attack the protected player's battle, violating "permanents you control"), OR `PlayerOrPlaneswalker` over-restricts battles for every planeswalker-only card (Eriette, Ghostly Prison-family). Both are rules-incorrect.

**This is leaf-level parameterization within ONE CR section (CR 508 declare-attackers legality), not a sibling-cluster smell**, per the CLAUDE.md "Parameterize, don't proliferate" / "categorical boundary" rule: the parameterization axis (which `AttackTarget` inhabitants are defended) lies entirely inside CR 508.1c (restrictions) + CR 508.5 (defending-player identity for planeswalker/battle). It is behaviorally distinct at the gate (the Battle arm). The existing enum already parameterizes this exact axis (`Player` vs `PlayerOrPlaneswalker` vs `Owner` vs `OwnerOrPlaneswalker`); `PlayerOrPermanents` is the next leaf on the same axis. `cargo engine-inventory` / `/add-engine-variant` gate: this is an additive leaf variant on `AttackTargetFilter` justified by a distinct `(filter, AttackTarget::Battle)` truth value — it passes the parameterization filter (cannot be expressed by any existing variant) and the categorical-boundary check (single CR section).

**No existing variant means "you + all permanents".** Verified: `restrictions.rs:1914-1939` matcher has no Battle arm for `PlayerOrPlaneswalker` and no "all permanents" inhabitant.

### Scope-assignment table (confirmed)

| Card | Phrase | `AttackTargetFilter` | Defends (AttackTarget inhabitants) |
|---|---|---|---|
| **Promise of Loyalty** | "you or planeswalkers you control" | `PlayerOrPlaneswalker` (existing) | Player + Planeswalker (NOT Battle) |
| **Willie Lumpkin** | "you or permanents you control" | `PlayerOrPermanents` (NEW leaf) | Player + Planeswalker + **Battle** |

---

## Central architectural decision

Reuse the EXISTING defender-scope primitive `StaticDefinition.attack_defended: Option<AttackTargetFilter>` (`types/ability.rs:14865`, builder `attack_defended()` at `:14912`, both re-verified). The Eriette analog confirms the shape: its static carries `attack_defended:"PlayerOrPlaneswalker"` and is enforced at declare-attackers (verified in card-data + `static_abilities.rs:683` scoped gate → `restrictions::attack_target_matches_defended_scope` at `restrictions.rs:1900`).

**Decision: NO field on `StaticMode::CantAttack`; NO new `StaticMode` variant; do NOT touch `StaticMode` (concurrently edited by #4010/#3958).** Scope rides on the sibling field `attack_defended`. The only new type-level work is one leaf variant `AttackTargetFilter::PlayerOrPermanents`.

Two install/enforcement seams (each justified below by a traced analog):

- **Promise (per-object)** → object-scoped continuous grant. The chosen creatures host a scoped `CantAttack` static (source = recipient), gated `for as long as it has a vow counter`. Enforced by the EXISTING `combat.rs:2162` scoped-`attacks`-loop gate. **No combat-gate change.** Analog: **Aurification** (verified card-data: continuous static `affected: Typed(Creature) + property Counters{gold,GE,1}`, re-evaluated continuously per CR 611.3a).
- **Willie (player-scoped, next-turn-expiring)** → `GameRestriction::ProhibitActivity` with a NEW `ProhibitedActivity::Attack { defended }`. Enforced by a NEW gate in declare-attackers that scans `state.restrictions` for the active player. Analog: **Kang** (verified: `RestrictionExpiry::UntilEndOfNextTurnOf{player}` armed at `turns.rs:766`, pruned at cleanup).

---

## B3b RESOLUTION — Willie's player-scoped seam: the EXACT home, container, insertion point, and expiry anchor

**The reviewer confirmed (and I re-verified):** `game/effects/add_restriction.rs` has NO attack-restriction kind (`ProhibitedActivity` has only `CastOnlyFromZones`, `CastSpells`, `ActivateAbilities` — `types/ability.rs:1613-1630`, re-verified), and there is NO existing player-scoped "can't attack you" combat query. A NEW seam is genuinely required either way.

I read BOTH candidate homes and decide on **(ii) the `GameRestriction`/`RestrictionExpiry` system**, NOT (i) the player-bound TCE path. The deciding factor is the **temporal anchor**, which round 1 got wrong:

### Why NOT (i) the `SpecificPlayer` TCE / `transient_grants_static_mode_to_player` path
- The existing player-scoped TCE query `transient_grants_static_mode_to_player` (`static_abilities.rs:736`, re-verified) matches `ContinuousModification::AddStaticMode { mode }` — which carries **only a `StaticMode`, no `attack_defended` scope** (`types/ability.rs:15726`). It cannot express the defender scope without modifying `AddStaticMode` (forbidden blast radius).
- **Fatal duration mismatch (the round-1 bug):** `Duration::UntilEndOfNextTurnOf` TCEs arm/prune in `prune_until_next_turn_effects` (`layers.rs:409-426`, re-verified) keyed on **`e.controller == active_player`** with `PlayerScope::Controller` ONLY. The TCE's `controller` is the GRANT's controller (Willie's controller = the protected player). Willie's restriction must expire at the end of the **restricted opponent's** next turn ("during their next turn") — a DIFFERENT player. A plain `UntilEndOfNextTurnOf{Controller}` TCE would arm on the wrong player's turn. No prune handles a non-`Controller` `UntilEndOfNextTurnOf` TCE (verified: the only non-Controller `UntilEndOfNextTurnOf` handling is in the `GameRestriction` path, `casting.rs:487`/`turns.rs:767`/`derived_views.rs:398`).

### Why (ii) the `GameRestriction::ProhibitActivity` path is correct
- `GameRestriction::ProhibitActivity` (`types/ability.rs:1603-1608`, re-verified) carries `affected_players: RestrictionPlayerScope` + `expiry: RestrictionExpiry` + `activity: ProhibitedActivity`. Its `RestrictionExpiry::UntilEndOfNextTurnOf { player: PlayerId }` (`:1648`) takes an **explicit `PlayerId`** — the correct anchor for "the restricted player's next turn". It is armed at `turns.rs:766-772` (verified: `if *player == active { *expiry = EndOfTurn }`) and pruned at cleanup.
- `RestrictionPlayerScope::ParentTargetedPlayer` (`types/ability.rs`, re-verified: "CR 608.2c: Anaphoric 'that player' in a sub-ability reuses a player target already chosen") is EXACTLY Willie's "that player". `add_restriction.rs:51-55` resolves `ParentTargetedPlayer`/`TargetedPlayer` → `SpecificPlayer(resolved_target_player)` at resolution time (re-verified).

**Named storage container:** the grant lives in **`state.restrictions: Vec<GameRestriction>`** as `GameRestriction::ProhibitActivity { source: Willie_id, affected_players: SpecificPlayer(restricted_pid), expiry: UntilEndOfNextTurnOf{player: restricted_pid}, activity: ProhibitedActivity::Attack { defended: PlayerOrPermanents } }`.

**Exact declare-attackers insertion point that reads it:** a NEW gate loop in `combat.rs` immediately after the existing scoped `attacks` loop at **`combat.rs:2160-2184`** (the `for (attacker_id, target) in attacks` block, re-verified). The new loop iterates `attacks`, and for each `(attacker_id, target)` where `state.objects[attacker_id].controller` is an `affected_players`-matched player, scans `state.restrictions` for an active `ProhibitActivity{ activity: Attack { defended } }` whose `affected_players` contains that controller, and rejects if `restrictions::attack_target_matches_defended_scope(state, Some(target), defended, protected_player, protected_player)` is true. **The `attack_defended` scope travels with the restriction** as the `defended` field of `ProhibitedActivity::Attack` — read directly by the gate, passed verbatim into the shared `attack_target_matches_defended_scope` matcher (the SAME matcher Promise uses, so both seams share one scope authority).
  - *Source-controller argument:* the matcher compares attacked planeswalker/battle controllers against `source_controller`. For Willie the "you" being defended is the protected player (= Willie's controller = the grant `source`'s controller). Resolve it as `state.objects[restriction.source].controller` (CR 109.5: "you" = the static/permanent's controller). Pass it as both `source_controller` and `source_owner` args (battles/planeswalkers compare on controller; the Player arm compares on it directly).

**Expiry-anchor correction (round-1 gap, must fix):** `add_restriction.rs:101-107` currently lowers `Duration::UntilEndOfNextTurnOf{Controller}` → `RestrictionExpiry::UntilEndOfNextTurnOf{ player: ability.controller }` (re-verified). For Willie the expiry player must be the **restricted (parent-targeted) player**, NOT `ability.controller`. **Add an arm** in `add_restriction.rs` that, when `affected_players` resolved to `SpecificPlayer(p)` from a `ParentTargetedPlayer`/`TargetedPlayer` scope AND the duration is the next-turn shape, sets `expiry = UntilEndOfNextTurnOf{ player: p }`. Since the `affected_players` resolution (`:51-55`) runs before the expiry block (`:82-110`), read the already-resolved `SpecificPlayer(p)` in the expiry arm. **CR 514.2 + CR 500.7** (verified: "until end of turn / this turn effects end" + extra-turn ordering) — the restriction survives the creation turn and ends at the restricted player's next-turn cleanup.

**Arming confirmation for this case:** `turns.rs:763-773` (re-verified) arms `GameRestriction::ProhibitActivity` whose `expiry == UntilEndOfNextTurnOf{player}` when `player == active`. With `player = restricted_pid`, it arms on the restricted player's next untap step and ends at that turn's cleanup — exactly "during their next turn". Confirmed this path fires for `ProhibitActivity` (the only variant the arming loop matches).

---

## B3a — CR 109.4 vs 109.5 (corrected, grep-verified)

Grep-verified against `docs/MagicCompRules.txt`:
- **CR 109.4** (verified): "Only objects on the stack or on the battlefield have a controller… There are six exceptions…" → this is the **control/ownership identity** rule.
- **CR 109.5** (verified): "The words 'you' and 'your' on an object refer to the object's controller… For a static ability, this is the current controller of the object it's on." → this is the **"you/your" self-reference** rule.

**Correction applied throughout the plan and new code:**
- The **identity of the protected player** ("you" in "can't attack **you**") resolves via **CR 109.5** ("you" → the static's/permanent's controller) — this is correct as-is.
- The **control comparison for the defended planeswalker/battle/permanent** ("planeswalkers/permanents **you control**") — i.e., `permanent_controller(pw_id) == Some(source_controller)` — is governed by **CR 109.4** (control), NOT 109.5.

Every NEW annotation I add to the `PlayerOrPermanents` matcher arms and the Willie gate must cite:
- `// CR 508.1c + CR 109.5: "you" is the static's controller (the protected player).`
- `// CR 109.4 + CR 508.5: planeswalker/battle defended-scope compares the attacked permanent's controller against the protected player.`

I do NOT rewrite the pre-existing `CR 109.5` comments on unrelated intrinsic-CantAttack paths (`combat.rs:431,548,2562,3258`) — those correctly use 109.5 for "you" resolution and are another agent's surface; surgical/additive only.

---

## B2 — confirmed Battle arm

The matcher `attack_target_matches_defended_scope` (`restrictions.rs:1914-1939`, re-verified) currently has NO `(PlayerOrPlaneswalker, Battle)` arm — correct, and it must stay that way (planeswalker-only cards must not restrict battles). **Step 2 adds three `PlayerOrPermanents` arms including the Battle arm.** CR 508.5 verified ("…the controller of the planeswalker that creature is attacking, or the protector of the battle that creature is attacking").

---

## B4 — Promise "those creatures" anaphor: TRACED (round-1 plan was BROKEN here)

**Traced end-to-end (subagent + my own reads, file:line confirmed):**

1. **First sentence** "Each player puts a vow counter on a creature they control and sacrifices the rest" → lowers toward `Effect::ChooseAndSacrificeRest` (resolver `game/effects/choose_and_sacrifice_rest.rs`; IR `oracle_ir/ast.rs:1084 CategoryAndSacrificeRest`; chain-split recognizes "sacrifices the rest" at `sequence.rs:699`). **The resolver accumulates kept creatures in a local `all_kept` vector that is DISCARDED when the effect completes** — it does NOT populate `ability.targets` or any tracked set.
2. **Second sentence** "Each of those creatures" / "those creatures" → `TargetFilter::ParentTarget` (`subject.rs:1299`, and "each of those creatures" → `ParentTarget` at `subject.rs:1228-1235`, test at `:4798`). At runtime `ParentTarget` resolves to `object_targets(&ability.targets)` (`targeting.rs:681`, `filter.rs:2823`).
3. **THE BREAK:** `ChooseAndSacrificeRest` leaves `ability.targets` empty, so `ParentTarget` resolves to the EMPTY set. The round-1 plan asserted "those creatures binds to the vow recipients" — **it does not**; as designed it would bind to nothing (restriction applies to zero creatures), or if the parser re-targeted, it would broadcast to all creatures. Reviewer B4 is VALID.

### Scoping fix (specified exactly)
The vow recipients must be published as a **tracked set**, and the anaphor must bind to that set, NOT `ParentTarget`:

1. **`ChooseAndSacrificeRest` must publish the kept set.** Before sacrificing the unchosen permanents, call `publish_fresh_tracked_set(state, all_kept)` (`game/effects/mod.rs:3516`, re-verified — allocates a fresh `TrackedSetId`, stores in `state.tracked_object_sets`, sets `state.chain_tracked_set_id`). Analog: `Effect::ChooseFromZone` already does this (`choose_from_zone.rs:292`). This is additive (a new publish call), not a behavior change to the sacrifice itself.
2. **The second-sentence anaphor binds to `TargetFilter::TrackedSet { id: TrackedSetId(0) }`** (the "most recent published set" sentinel, resolved at `filter.rs:1582`/`:1601`, re-verified) instead of `ParentTarget`. The parser must, when "those creatures" follows a `ChooseAndSacrificeRest` (tracked-set-publishing) head in the same chain, emit `TrackedSet` rather than `ParentTarget`. This is the SAME bridge `ChooseFromZone` "those cards" consumers already use.
3. **Per-recipient duration gate.** Each recipient hosts a scoped `CantAttack` static via `ContinuousModification::GrantStaticAbility { definition: Box<StaticDefinition{ mode:CantAttack, affected:SelfRef, attack_defended:Some(PlayerOrPlaneswalker), condition:Some(HasCounters{vow,GE,1}) }> }`. At layer-6 expansion (`layers.rs:2543` `expand_granted_static_effects`, re-verified: "The recipient is the granted-static's *source*"), the inner static's `source_id = recipient`, so `StaticCondition::HasCounters{vow}` (evaluated against `source_id` at `layers.rs:916`, verified via subagent) correctly gates on that recipient's OWN vow counter — per-object, exactly CR 611.2b. **Decision: use `HasCounters` (not `RecipientHasCounters`)** because the static is installed AS the recipient's own static (source = recipient), not as an attached-object static; `RecipientHasCounters` (`ability.rs:5148`) is for Aura/Equipment "it" = enchanted/equipped object, which does not apply here.
4. **Duration of the grant itself** = `Duration::ForAsLongAs { condition: HasCounters{vow,GE,1} }` (CR 611.2b, verified) so the grant lapses when the counter is removed; the inner static's own `condition` mirrors it for per-object continuity (CR 611.3a "applies at any moment to whatever its text indicates").

**Discriminating test requirement (B4):** a board with the vow recipient AND a NON-recipient creature the affected player controls — the non-recipient (no vow counter, not in the tracked set) must be UNRESTRICTED; only the recipient is blocked from attacking you/your planeswalkers.

**⚠️ NEEDS MANUAL VERIFICATION at implementation time:** Promise's first sentence is "puts a vow **counter** on a creature they control and sacrifices the rest" — this is a *put-counter-on-the-kept-creature* variant, NOT the multi-category keep (Tragic Arrogance) that `ChooseAndSacrificeRest` was built for. I verified `ChooseAndSacrificeRest` exists and handles "choose … sacrifice the rest", but I did NOT confirm it currently (a) parses Promise's "puts a vow counter on a creature they control and sacrifices the rest" framing, or (b) places the vow counter on the kept creature. **Implementation must first confirm the first sentence parses to a counter-placing-then-sacrifice effect; if not, that first-sentence parse is a prerequisite sub-task** (place a vow counter on each player's chosen creature + sacrifice the rest + publish the kept set). If that infrastructure is genuinely missing and large, Promise's second-sentence restriction is correctly gated behind it — keep the whole card honest via `Effect::unimplemented` until the first sentence is real (do NOT ship a green restriction over a broken counter/sacrifice head).

---

## Architectural sections (mandatory)

### Pattern Coverage
- `AttackTargetFilter::PlayerOrPermanents` + Battle arm: every "can't attack you or permanents you control" card (Willie + future "permanents you control" defender scopes). Combined with existing `PlayerOrPlaneswalker`, covers the full defender-scope family.
- `ProhibitedActivity::Attack { defended }`: the player-scoped, next-turn-expiring "that player can't attack [scope]" class — Willie and any "target/that player can't attack you (or your permanents/planeswalkers) [until/during their next turn]" effect. Composes with the existing `RestrictionPlayerScope` (Targeted/ParentTargeted/Defending/AllPlayers) and `RestrictionExpiry` family.
- `ChooseAndSacrificeRest` tracked-set publish + "those creatures"→`TrackedSet`: every "each player keeps/chooses X, then those [kept] do Y" chain (Promise, and future edict-of-choice → those-survivors riders). Estimated class: 10+ cards across the two seams.

### Building Blocks (compose, don't reinvent)
- `attack_target_matches_defended_scope` (`restrictions.rs:1900`) — single scope authority shared by BOTH seams.
- `StaticDefinition::attack_defended()` builder (`ability.rs:14912`); `ContinuousModification::GrantStaticAbility` (`ability.rs:15741`) + its layer expansion `expand_granted_static_effects` (`layers.rs:2617`).
- `GameRestriction::ProhibitActivity` + `RestrictionPlayerScope::ParentTargetedPlayer` + `RestrictionExpiry::UntilEndOfNextTurnOf{player}` + `add_restriction.rs` resolution; `turns.rs:766` arming.
- `publish_fresh_tracked_set` (`mod.rs:3516`) + `TargetFilter::TrackedSet{TrackedSetId(0)}` (`filter.rs:1582`).
- `StaticCondition::HasCounters` (`ability.rs:5130`, eval `layers.rs:916`).
- Parser: nom combinators in `oracle_static/evasion.rs` (`parse_cant_attack_defended_scope_nom` region) and `oracle_effect/subject.rs` (`try_parse_subject_restriction_clause`).

### Logic Placement
- New `AttackTargetFilter` leaf + matcher arms → `types/triggers.rs` + `game/restrictions.rs` (types + rules).
- New `ProhibitedActivity::Attack` variant + resolution/expiry → `types/ability.rs` + `game/effects/add_restriction.rs`.
- New player-scoped declare-attackers gate → `game/combat.rs` (combat legality, CR 508).
- Tracked-set publish → `game/effects/choose_and_sacrifice_rest.rs`. Anaphor→TrackedSet binding + scope-phrase + duration-phrase detection → `parser/`.

### Rust Idioms
- Typed `AttackTargetFilter` leaf, no bool. Exhaustive `match` on `(filter, target)` (the matcher already has a final `_ => false` for the cross-product; new arms are explicit). `ProhibitedActivity::Attack { defended: AttackTargetFilter }` reuses the existing typed scope enum — no new bool, no parallel scope type.

### Nom Compliance (mandatory — parser files change)
All detection/dispatch via nom combinators; NO `contains`/`find`/`split`/`starts_with` for parsing dispatch. Concrete combinators:
- **Scope phrase** (extend the existing `opt(alt((...)))` at `oracle_static/evasion.rs:2124-2134`, and the sibling at `evasion.rs:929` region): add `value(AttackTargetFilter::PlayerOrPermanents, tag_no_case(" you or permanents you control"))` ordered **before** ` you or planeswalkers you control` and ` you` so the longest phrase wins (nom `alt` is leftmost-match). Promise's " you or planeswalkers you control" already maps to `PlayerOrPlaneswalker` here — verified present at `:2127`.
- **Willie player-scoped clause** (in the trigger sub-chain lowering): replace the `Effect::Unimplemented{"can't"}` leaf. Recognize `"that player can't attack" + <scope> + " during their next turn"` with chained `tag`/`value`/`opt`: subject `tag("that player can't attack")` → reuse the scope-phrase combinator above (PlayerOrPermanents/PlayerOrPlaneswalker/Player) → duration `value(Duration::UntilEndOfNextTurnOf{ScopedPlayer-or-Target}, alt((tag(" during their next turn"), tag(" during that player's next turn"))))`. Emit `Effect::AddRestriction{ ProhibitActivity{ affected_players: ParentTargetedPlayer, activity: Attack{defended}, expiry: <placeholder> } }` with `ability.duration = Some(UntilEndOfNextTurnOf{...})` so `add_restriction.rs` lowers the expiry to the restricted player. (Duration combinators already exist for "until the end of their next turn" at `subject.rs:2482`; add the "during their next turn" leaf as a sibling `tag` arm — compose, don't duplicate.)
- **Promise "those creatures" → TrackedSet**: in `subject.rs` (`:1299` region) the "those " subject currently emits `ParentTarget`; add a context-gated branch so when the chain head is a tracked-set publisher (`ChooseAndSacrificeRest`), "those creatures"/"each of those creatures" emits `TargetFilter::TrackedSet{TrackedSetId(0)}`. Use `ParseContext` (`oracle_nom/context.rs`) to carry the "prior clause published a tracked set" flag rather than string-sniffing.
- The parser IS the detector: gate Willie's lowering on `parse_cant_attack_defended_scope_nom(rest).is_ok()` succeeding, not on `contains("can't attack")`.

### Extension vs Creation
Extends three existing patterns: `AttackTargetFilter` (leaf), `ProhibitedActivity` (sibling variant within the established `GameRestriction::ProhibitActivity` family — justified because attacking is a distinct activity axis from casting/activating, exactly the axis `ProhibitedActivity` enumerates), and the `ChooseAndSacrificeRest`→tracked-set bridge (reuses `ChooseFromZone`'s existing publish→TrackedSet mechanism). No new state machine, no new effect for the restriction itself.

### Analogous Trace
- **Defender-scope static (Promise seam):** traced **Eriette** end-to-end: `types/triggers.rs:181 AttackTargetFilter` → parser `oracle_static/evasion.rs` (`attack_defended` capture) → `types/ability.rs:14865 attack_defended` → `game/static_abilities.rs:683` scoped gate → `game/restrictions.rs:1900 attack_target_matches_defended_scope` → `game/combat.rs:2162` declare-attackers. Plus **Aurification** for counter-gated continuous application (`affected` Counters property, CR 611.3a).
- **Player-scoped next-turn restriction (Willie seam):** traced **Kang**: `types/ability.rs:1603 GameRestriction::ProhibitActivity` + `:1648 RestrictionExpiry::UntilEndOfNextTurnOf` → `game/effects/add_restriction.rs:82-110` (duration→expiry lowering) → `game/turns.rs:763-783` (arm + prune) → `game/casting.rs:467` (enforcement reader pattern; Willie adds the analogous combat reader at `combat.rs:2184`).
- **Tracked-set anaphor (Promise B4):** traced **ChooseFromZone**: `choose_from_zone.rs:292 publish_fresh_tracked_set` → `filter.rs:1582 TrackedSet` resolution → "those cards" consumer.

### Variant Discoverability
`AttackTargetFilter::PlayerOrPermanents` and `ProhibitedActivity::Attack` are both additive. Run `cargo engine-inventory` and the `/add-engine-variant` checklist for each before extension (parameterization filter: neither is expressible by an existing variant — `PlayerOrPermanents` has a unique Battle truth value; `Attack` is a new activity axis. Categorical boundary: `PlayerOrPermanents` is single-section CR 508; `Attack` is the activity-axis enumeration CR 508.1c/601.2a parallel).

---

## Step-by-step

- **Step 1** — `types/triggers.rs:193` (after `OwnerOrPlaneswalker`): add `AttackTargetFilter::PlayerOrPermanents` with annotation `// CR 508.1c + CR 508.5 + CR 109.4: "you or permanents you control" defends you + planeswalkers + battles you control (control compared per CR 109.4).` Run `/add-engine-variant`.
- **Step 2** — `game/restrictions.rs:1937` (before final `_ => false`): add three arms:
  - `(PlayerOrPermanents, AttackTarget::Player(p)) => *p == source_controller,` `// CR 508.1c + CR 109.5`
  - `(PlayerOrPermanents, AttackTarget::Planeswalker(pw_id)) => permanent_controller(*pw_id) == Some(source_controller),` `// CR 109.4 + CR 508.5`
  - `(PlayerOrPermanents, AttackTarget::Battle(b_id)) => permanent_controller(*b_id) == Some(source_controller),` `// CR 109.4 + CR 508.5 + CR 310 (battles are attackable permanents)`
  Leave `PlayerOrPlaneswalker` WITHOUT a Battle arm (B2: must remain false for battles).
- **Step 3** — `parser/oracle_static/evasion.rs` (`:2124` and `:929` scope combinators): prepend `value(PlayerOrPermanents, tag_no_case(" you or permanents you control"))` to the `opt(alt((...)))` (longest-match first). Verify no other `parse_cant_attack_defended_scope_nom` site is missed (grep the symbol).
- **Step 4** — `types/ability.rs:1613` (`ProhibitedActivity`): add `Attack { defended: crate::types::triggers::AttackTargetFilter }` with `// CR 508.1c + CR 601.2a-parallel: a temporary effect prohibits a player from declaring attacks against the defended scope.` Update the matches in `add_restriction.rs:36-41,45-78,82-110` (they currently destructure `ProhibitActivity{..}` — confirm exhaustive `ProhibitedActivity` matches, if any, get the new arm; `add_restriction.rs` matches on `GameRestriction` not `ProhibitedActivity`, so likely no change beyond the expiry arm).
- **Step 5** — `game/effects/add_restriction.rs:82-110`: add the expiry arm so a next-turn duration with a resolved `SpecificPlayer(p)` from `ParentTargetedPlayer`/`TargetedPlayer` lowers to `RestrictionExpiry::UntilEndOfNextTurnOf{ player: p }` (the restricted player), not `ability.controller`. Read the already-resolved `affected_players` (resolution runs first at `:45-78`).
- **Step 6** — `game/combat.rs` after the scoped `attacks` loop at `:2184`: add the new gate loop reading `state.restrictions` for `ProhibitActivity{ activity: Attack{defended}, affected_players, .. }`, matching the attacker's controller against `affected_players`, and rejecting via `attack_target_matches_defended_scope(state, Some(target), defended, protected, protected)` where `protected = state.objects[source].controller`. Also add the parallel reader in any derived "legal attack targets" view if one exists for this restriction class (grep for where `ProhibitActivity` is read for casting and mirror only if a combat-eligibility view needs it; do NOT over-apply — eligibility queries with no target should skip, mirroring `static_abilities.rs:677`).
- **Step 7** — Willie parser: in the `Damagedone` sub-chain lowering, replace the `Effect::Unimplemented{"can't"}` leaf with the `Effect::AddRestriction` lowering (Nom Compliance section). Preserve the existing `condition: EffectOutcome{OptionalEffectPerformed}` gate on that sub-ability (the restriction only applies "if they do" draw).
- **Step 8** — Promise: (a) `choose_and_sacrifice_rest.rs` — `publish_fresh_tracked_set(state, all_kept)` before sacrificing the rest; (b) `subject.rs` — context-gated "those creatures"→`TrackedSet`; (c) emit the `GenericEffect{ static_abilities:[continuous().modifications([GrantStaticAbility{StaticDefinition{CantAttack, affected:SelfRef, attack_defended:Some(PlayerOrPlaneswalker), condition:Some(HasCounters{vow,GE,1})}}])], duration: ForAsLongAs{HasCounters{vow,GE,1}}, target: TrackedSet }`. **Gate on the first-sentence prerequisite** (B4 manual-verification note).
- **Step 9** — coverage honesty: remove the inert sorcery `CantAttack/SelfRef` static; clear the `SwallowedClause/Condition_AsLongAs` warning; any residue (e.g. unverified first-sentence counter/sacrifice) stays `Effect::unimplemented(...)` so coverage stays honest.

## CR annotations (round 2, grep-verified against `docs/MagicCompRules.txt`)
- **CR 508.1c** ✓ "active player checks each creature… restrictions… declaration is illegal" — the gate's rule.
- **CR 508.1d** ✓ requirements (not used for prohibitions; the existing matcher comment cites it — leave, additive).
- **CR 508.5 / 508.5a** ✓ defending-player identity for planeswalker/battle — control comparison.
- **CR 506.2 / 506.3** ✓ who/what can be attacked (battle is an attack target).
- **CR 109.4** ✓ control identity (planeswalker/battle "you control").
- **CR 109.5** ✓ "you/your" → controller (protected player identity).
- **CR 611.2b** ✓ "for as long as" duration (Promise).
- **CR 611.2c / 611.3a** ✓ static-from-resolution vs static-ability set re-evaluation (recipient set is continuous).
- **CR 514.2 + CR 500.7** ✓ next-turn expiry arming (Willie).
- **CR 122.1** ✓ counter definition (vow counter).
- **CR 121.1** ✓ draw (the trigger's preceding draws).
All present in `docs/MagicCompRules.txt` (verified this session). No subpart hallucination — every number printed above was grep-confirmed.

---

## Verification Matrix (every test names: changed seam → production entry point → revert-failing assertion → sibling/negative)

Runtime tests use `add_real_card` + rehydrate and drive the REAL declare-attackers pipeline (`can_declare_attackers`/`declare_attackers_with_bands`), not hand-built state.

1. **Willie scope parse** — seam: Willie parser (Step 7). Entry: parse Willie. Assert: trigger sub-chain leaf is `Effect::AddRestriction{ ProhibitActivity{ activity: Attack{ defended: PlayerOrPermanents }, affected_players: ParentTargetedPlayer } }`, NOT `Unimplemented`. Revert-fail: reverting Step 7 leaves `Unimplemented`. Negative: assert `defended != PlayerOrPlaneswalker` (proves the permanents/planeswalker distinction).
2. **Willie restricted-player rejection** — seam: combat gate (Step 6) + matcher (Step 2). Entry: Willie deals combat damage to opponent A, A draws; on A's next turn A declares an attack vs Willie's controller. Assert: declare-attackers REJECTED. Revert-fail: reverting Step 6 lets the attack through. Sibling: a THIRD player B (not restricted) attacking the protected player is ALLOWED.
3. **Willie Battle coverage (PlayerOrPermanents distinctive arm)** — seam: Step 2 Battle arm. Entry: protected player controls a battle; restricted opponent attempts to attack that battle. Assert: REJECTED. Revert-fail: removing the `(PlayerOrPermanents, Battle)` arm lets the battle attack through. **Discriminator vs B1**: a parallel `PlayerOrPlaneswalker`-scoped restriction (Eriette-style) must NOT reject the battle attack — proves the variants are behaviorally distinct.
4. **Willie next-turn expiry** — seam: `add_restriction.rs` expiry arm (Step 5) + `turns.rs:766` arming. Entry: advance past the restricted player's next turn. Assert: on the turn AFTER, the same attack is now LEGAL. Revert-fail: if the expiry anchors on `ability.controller` (round-1 bug) the restriction expires on the wrong turn — assert it expires on the RESTRICTED player's turn specifically.
5. **Promise vow-recipient rejection** — seam: tracked-set publish (Step 8a) + anaphor→TrackedSet (Step 8b) + grant (Step 8c). Entry: resolve Promise; the vow recipient attempts to attack Promise's controller / their planeswalker. Assert: REJECTED. Revert-fail: removing the publish/anaphor bridge binds to empty set → attack wrongly ALLOWED.
6. **Promise non-recipient NOT restricted (B4 discriminator)** — seam: tracked-set scoping. Entry: the affected player controls BOTH the vow recipient AND a non-recipient creature; non-recipient attacks the protected player. Assert: non-recipient attack ALLOWED, recipient REJECTED. Revert-fail: a broadcast (ParentTarget-to-all) binding restricts the non-recipient too.
7. **Promise counter-removal lapse** — seam: `ForAsLongAs{HasCounters vow}` + `HasCounters` eval (`layers.rs:916`). Entry: remove the vow counter from the recipient; recipient attacks the protected player. Assert: now ALLOWED. Revert-fail: a permanent (non-conditional) grant keeps it restricted.
8. **Promise battle NOT restricted (PlayerOrPlaneswalker excludes battle)** — seam: Step 2 (no Battle arm for PlayerOrPlaneswalker). Entry: protected player controls a battle; vow recipient attacks that battle. Assert: ALLOWED (Promise is planeswalker-scoped, not permanents). Revert-fail: wrongly adding a Battle arm to PlayerOrPlaneswalker rejects it.
9. **Propaganda no-over-application regression (MANDATORY)** — seam: the unscoped fast path at `combat.rs:441` (`sd.attack_defended.is_none()`) and the scoped gate. Entry: an UNSCOPED `CantAttack` static (Propaganda-family `attack_defended: None`) on a creature. Assert: that creature is rejected from attacking ANY target (player, planeswalker, battle) — proving the new scoped paths did NOT narrow the `attack_defended.is_none()` fast path. Revert-fail: if a refactor accidentally routes unscoped statics through the scoped matcher, an unscoped creature could wrongly attack — this test catches it.

**Coverage honesty:** Promise stays red/honest via `Effect::unimplemented` if the first-sentence counter/sacrifice prerequisite (B4 manual-verification note) is not satisfied. No Oracle text is accepted with deferred semantics silently — either the whole card resolves correctly or the unimplemented marker keeps coverage accurate.

---

## Constraints honored
- Do NOT touch `StaticMode` (#4010/#3958) — scope rides `attack_defended`; ✓.
- mtgish dormant — not referenced; ✓.
- Surgical/additive; no whole-file rewrites; commit by pathspec; Tilt-first (no raw cargo); ✓.
- All parser dispatch via nom combinators; ✓ (Nom Compliance section).
- No bool flags — `AttackTargetFilter` + `RestrictionPlayerScope` + `RestrictionExpiry` typed enums; ✓.

## Items flagged "needs manual verification"
1. Whether Promise's first sentence ("puts a vow counter on a creature they control and sacrifices the rest") currently parses to a counter-placing `ChooseAndSacrificeRest`-family effect and places the counter on the KEPT creature (B4 note). If absent, that is a prerequisite sub-task; gate Promise's restriction behind it via `Effect::unimplemented`.
2. The exact `PlayerScope`/anchor used in the Willie duration combinator ("during their next turn") — confirm at implementation that the lowered `Duration::UntilEndOfNextTurnOf` carries a scope that `add_restriction.rs` (Step 5) resolves to the RESTRICTED player, not the controller. The plan specifies reading the resolved `SpecificPlayer(p)`; verify no other `add_restriction` caller depends on the controller-anchored path.

---

## Reviewer round-2 verdict: APPROVED (zero gaps). Non-blocking implementer notes:

1. **Scope-combinator single authority = `oracle_static/shared.rs:2996` `parse_cant_attack_defended_scope_nom`** (NOT evasion.rs:929, which delegates to it). Add the `PlayerOrPermanents` arm in shared.rs:2996 (covers all delegating sites) PLUS the separate inline `alt()` copy at shared.rs:2124 (Propaganda "unless cost" family). Grep the symbol to land in the right place.
2. **Do NOT edit the pre-existing `CR 508.1d` annotations** at restrictions.rs:1898 and combat.rs:2160/2181 (other agents' surface; technically mis-cite requirements-subpart for a restriction, but out of scope). Use `CR 508.1c` for all NEW code.
3. **Step 5 expiry arm**: existing next-turn arms hardcode `PlayerScope::Controller`; the new arm must read the already-resolved `SpecificPlayer(p)` from the affected_players block (which runs first, add_restriction.rs:45-78, before the expiry lowering at :82-110). Anchor `UntilEndOfNextTurnOf{player: p}` on that resolved player, not `ability.controller`.
