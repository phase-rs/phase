---
phase: 58-custom-format-engine
doc: PLAN
subsystem: engine-formats
status: research-and-design (no implementation)
tags: [format, custom-format, legacy-rules, mana-burn]
---

# Phase 58 — PLAN

Design-only. No code is written in this phase. Below is the proposed
architecture, the custom-vs-built-in coexistence design, the delivery-surface
recommendation, and the sequencing.

## Mandatory architectural sections

### Pattern Coverage — build for the class, not the four formats

Every element below handles the *category* "data-driven format", not the four
EC formats specifically:

- **`CustomFormatDef`** is one schema instantiated N times. The four EC formats
  are four values; a future "my playgroup's Old-School-plus-Portal" is the
  N+1th value with zero engine change.
- **Legal-set membership** is `printings ∩ legal_sets ≠ ∅` — works for any set
  list, not four hardcoded pools.
- **Restricted/banned enforcement** reuses the *format-general* CR 100.2b
  enforcer (`restricted_copy_violations`) and the illegal-card path, sourced
  from name lists — works for any list.
- **`LegacyRuleSet`** is three independent rule-module toggles — works for any
  future era-rule combination, not "old rules on/off".

No arm anywhere may read `if format == OldSchool93_94`. Behavior is driven by
the resolved `CustomFormatDef` data only.

**Block Constructed — validation that the two axes are genuinely decoupled.**
"Block Constructed" formats (legality restricted to the sets of one block/era)
are a clean proof that the design's two axes — the legal-set-code list vs. the
`LegacyRuleSet` era-rule flags — are properly orthogonal, not entangled:

- A **modern-era block** needs *only* the legal-set-code axis with **all
  `LegacyRuleSet` flags false / defaulted** (`mana_burn: false`,
  `damage_uses_stack: false`, `pre_m10_wish_reaches_exile: false`,
  `legend_rule_scope: Modern`). It is a `CustomFormatDef` that is purely a
  set-list restriction.
- A **pre-M10-era block** (a block from before the 2010 "M10" rules update)
  would set `mana_burn: true` (and potentially `pre_m10_wish_reaches_exile` /
  `legend_rule_scope` depending on the exact era) via the **same** mechanism —
  proving `LegacyRuleSet` is *not* special-cased to the four EC presets but is a
  genuinely general axis any format opts into based on which era's rules it
  represents. The four EC formats are simply the first four consumers of an axis
  that is open to Block Constructed and any future era format alike.

**Legal-set list is a plain arbitrary `Vec<SetCode>`, not a "block" enum.**
Real-world Block Constructed formats have sometimes been legal only for a
*subset* of a block's sets (not always the full canonical block), so the legal-
set mechanism is modeled as a plain arbitrary list of set codes (`legal_sets:
Vec<SetCode>`, §1) rather than any higher-level "block" abstraction tied to
WotC's own block groupings. Arbitrary subsets fall out of a plain list for free
— no "block" type, no special-casing, and a format author can include exactly
the set codes they want. (This also matches the EC formats, whose pools are
hand-curated set lists that include special reprint sets like CE/ICE and do not
map onto any single WotC block.)

### Building Blocks — reused, not reinvented

| Need | Existing block reused | Location |
|------|----------------------|----------|
| Structural game params (life, deck size, players, sideboard) | `FormatConfig` | `types/format.rs` |
| Set codes per card | `CardDatabase::printings_for` | `card_db.rs:227` |
| Format-level 1-copy ceiling (CR 100.2b) | `restricted_copy_violations` + `restricted_canonical` set | `deck_validation.rs:2462` / `:373` |
| 4-copy default + card-intrinsic overrides (CR 100.2a) | `copy_limit_violations` | `deck_validation.rs:2412` |
| Illegal-card reporting | existing `illegal_cards` accumulation | `deck_validation.rs:372-406` |
| Step-end unspent-mana drop (mana-burn hook) | `apply_empty_mana_pool_decisions` Drop arm | `types/mana.rs:1707` |
| Format registry → frontend | `GameFormat::registry()` / `FormatMetadata` | `format.rs:326` |

New building blocks introduced (all general): `CustomFormatDef`,
`CustomFormatId`, `ReprintPolicy`, `LegacyRuleSet`, and a
`custom_format_registry()` of bundled presets.

### Logic Placement — engine owns everything

All of it lives in the `engine` crate. Deck legality, set-membership checks,
banned/restricted enforcement, and legacy-rule behavior are engine logic. The
frontend receives the resolved `CustomFormatDef` (or a display projection of it)
via the existing registry/WASM export and *renders* it — it never re-derives
legality. This matches "the engine owns all logic / the frontend is a display
layer".

### Extension vs Creation

We **extend** the existing format system, we do not fork it:
- One additive `GameFormat::Custom(CustomFormatId)` variant (mirrors the
  `GameFormat::Limited` additive-variant precedent).
- `FormatConfig` gains an `Option<CustomFormatRules>` payload — additive field,
  `None` for all official formats, serde-default so existing serialized configs
  round-trip.
- Deck validation gains one match arm that routes `Custom` to a new
  `evaluate_custom_format` that *calls the existing* copy-limit and restricted
  enforcers with a data-sourced name set.

### Analogous Trace

`GameFormat::Limited` (phase 53) — see RESEARCH.md §7. We follow its
compiler-guided "extend every exhaustive match" method exactly, and we inherit
its lesson that there are two match sites (`format.rs` + `deck_validation.rs`).

## 1. Schema — the general custom-format layer

```text
CustomFormatId(u16)                     // Copy handle; keeps GameFormat: Copy

GameFormat::Custom(CustomFormatId)      // one additive, typed variant

CustomFormatRules {                     // the resolved ruleset payload
    id: CustomFormatId,
    structural: StructuralRules,        // Axis A — see below; first-class, not inherited
    legality: LegalityRules,            // Axis B — legal_sets/banned/restricted/legacy
}

// Axis A — "FFA that's super flexible" (maintainer framing, see CONTEXT.md
// "Maintainer input"). Every field here already exists on `FormatConfig`
// (`types/format.rs:174-212` — the full struct, re-read directly for this
// revision, not from memory) and several are already host-adjustable today in
// `client/src/components/lobby/HostSetup.tsx` for a chosen built-in
// `GameFormat` — this struct is a NAMED, SAVEABLE snapshot of that same
// knob set, not new game-rule surface. This is the axis the lobby "save as
// custom format" action captures.
//
// REVISED per maintainer review round 2 (CONTEXT.md point 2): the first-pass
// version of this struct dropped several behavior-bearing `FormatConfig`
// fields. Full accounting against the real struct:
//   - Included below: every FIELD that is independently meaningful and not
//     derivable from something else already in this struct.
//   - `uses_commander: bool` — NOT a stored field here. REVISED per
//     maintainer review round 3 (CONTEXT.md point 4): `GameFormat::
//     uses_commander()` (`format.rs:365-380`) is a hardcoded per-variant
//     match with no access to `FormatConfig`, but its doc comment states
//     the real invariant it encodes — true for every format whose
//     `FormatConfig` has BOTH `command_zone: true` AND a non-`None`
//     `commander_damage_threshold` (round 2 wrongly used the threshold
//     alone). `FormatConfig.uses_commander` for `GameFormat::Custom` is
//     computed at construction time (`from_lobby_config` / preset
//     constructors) as `structural.command_zone &&
//     structural.commander_damage_threshold.is_some()` — both conditions,
//     matching the stated invariant exactly. Never stored redundantly on
//     `StructuralRules` itself.
//   - `supplies_fixed_deck: bool` — NOT included. Always `false` for
//     `GameFormat::Custom`; no custom-format use case for an engine-supplied
//     fixed deck exists today. Flagged, not silently dropped, if that need
//     ever arises (it would need its own design, analogous to Momir's
//     Madness).
//   - `allow_debug_actions: bool` — deliberately EXCLUDED. Its own doc
//     comment states it is "orthogonal to format" — a session capability
//     flag, not format identity. Belongs on `FormatConfig` directly, set
//     independently of which format (built-in or custom) is chosen.
//   - `archenemy_player: Option<PlayerId>` — REMOVED per maintainer review
//     round 3 (CONTEXT.md point 3). It is a per-GAME seat index derived from
//     `topology()` and validated against THAT game's player count
//     (`FormatConfig::archenemy_player()` + `validate_for_player_count`,
//     `format.rs:658-670`) — persisting it in a reusable saved format could
//     reference a seat that doesn't exist, or a different player, next time
//     it's loaded. Axis A does not support the Archenemy one-vs-many
//     topology at all (a different `FormatTopology` shape, not a config knob
//     on top of `IndividualSeats`) — explicit scope exclusion, not a silent
//     drop; a topology-aware design is its own future project.
//   - `sideboard_policy` — this is NOT a `FormatConfig` field. It's a
//     `GameFormat` method (`format.rs:270`,
//     `fn sideboard_policy(self) -> SideboardPolicy`), computed from the
//     enum variant for built-in formats, with no `FormatConfig` access to
//     check anything for `Custom`. `StructuralRules` carries it explicitly;
//     see the new `FormatConfig::sideboard_policy()` accessor and consumer
//     migration below (maintainer review round 3, CONTEXT.md point 2) —
//     round 2 added this field but never specified how anything would
//     actually READ it for a Custom format, which was itself a gap.
StructuralRules {
    starting_life: i32,
    min_players: u8,
    max_players: u8,
    deck_size: u16,
    singleton: bool,
    command_zone: bool,
    commander_damage_threshold: Option<u8>,
    range_of_influence: Option<Box<RangeOfInfluenceConfig>>,  // mirrors FormatConfig's field exactly
    team_based: bool,
    sideboard_policy: SideboardPolicy,  // no derivation source for Custom, see note above
}

// --- sideboard_policy accessor and consumer migration (new — maintainer
// review round 3, CONTEXT.md point 2) ---
//
// `GameFormat::sideboard_policy(self) -> SideboardPolicy` (`format.rs:270`)
// is an exhaustive match over the built-in variants with NO access to
// `FormatConfig` — a `Custom(_)` arm there can only be
// `unreachable!("call FormatConfig::sideboard_policy() instead")`, since the
// real answer lives in `custom_rules`, which this method cannot see. The
// correct authority is one level up:
//
// impl FormatConfig {
//     pub fn sideboard_policy(&self) -> SideboardPolicy {
//         match self.format {
//             GameFormat::Custom(_) => self.custom_rules.as_ref()
//                 .expect("Custom format must carry custom_rules")
//                 .structural.sideboard_policy,
//             other => other.sideboard_policy(),
//         }
//     }
// }
//
// Every current call site of `state.format_config.format.sideboard_policy()`
// — confirmed this session at `game/deck_loading.rs:681` and
// `game/match_flow.rs:357`; a repo-wide grep for the same pattern at
// implementation time catches any others — migrates to
// `state.format_config.sideboard_policy()`. Round 2 added the field to
// `StructuralRules` but never specified this accessor or the migration,
// which is why a saved format's sideboard policy would have been silently
// ignored (both existing consumers would keep reading a meaningless
// built-in-format answer for `GameFormat::Custom`).

// Axis B — legality/era-rules. Genuinely new data; no existing UI surface.
// This is what makes the four EC formats rules-correct; kept exactly as
// originally designed, just now named as its own struct rather than sharing
// `CustomFormatRules`'s top level with Axis A undifferentiated.
//
// REVISED per maintainer review round 2 (CONTEXT.md point 1): `legal_sets`
// was a bare `Vec<SetCode>`, which cannot distinguish "no set restriction"
// (Axis A's default) from "restricted to the empty set" (which would reject
// every card) — the evaluator (§3) checked membership unconditionally, so an
// empty Vec silently rejected everything. `Option<Vec<SetCode>>` fixes this:
// `None` = unrestricted (every card passes the pool check), `Some(list)` =
// restricted to `list`. This is the same `Option<T>`-over-ambiguous-sentinel
// pattern CLAUDE.md already prescribes elsewhere in this codebase.
LegalityRules {
    legal_sets: Option<Vec<SetCode>>,   // None = unrestricted; Some(list) = pool membership
    reprint_policy: ReprintPolicy,
    banned: Vec<CardName>,              // fully illegal
    restricted: Vec<CardName>,          // legal, max 1 (CR 100.2b path)
    legacy: LegacyRuleSet,
}

// A format saved from the lobby ("FFA, but I bumped starting life to 30 and
// capped it at 4 players") sets `structural` and leaves `legality` at
// defaults (`legal_sets: None`, no legacy rules) — a fully valid
// `CustomFormatRules` value that legitimately places no restriction on the
// card pool. The four EC formats set `legality` to real data (`legal_sets:
// Some([...])`) and `structural` to sane multiplayer/duel defaults. Neither
// axis requires the other to be present; this is the orthogonality Block
// Constructed already proved for `LegacyRuleSet` (§ below), now proved a
// second time between Axis A and Axis B — and the `Option` fix above is what
// makes Axis A's "no restriction" case a real, correctly-representable value
// rather than an accidental illegal state.

// --- Identity, persistence, and transport (new — maintainer review round 2,
// CONTEXT.md point 3) ---
//
// The original design conflated two different identity concerns under one
// `CustomFormatId`. Separating them:
//
//   (a) AGREEMENT WITHIN ONE GAME — already solved. `FormatConfig.custom_rules`
//       (below) carries the full resolved `CustomFormatRules` VALUE, not a
//       lookup key. A peer receiving a host's `FormatConfig` gets the entire
//       ruleset directly — no registry lookup, no need to "already know" a
//       custom format by ID. This was already correct; it just wasn't stated
//       explicitly enough to distinguish from (b).
//   (b) A PLAYER'S REUSABLE SAVED FORMAT — was never designed. This is a
//       CLIENT-SIDE-ONLY concern:
//
// SavedCustomFormat {                 // client-local only — NOT an engine or WASM type
//     local_id: Uuid,                 // or any locally-unique key; never sent over the wire
//     name: String,                   // player-chosen display name
//     rules: CustomFormatRules,       // the exact payload a game-start packages into FormatConfig
// }
//
// A player's "my saved formats" list lives in local storage / a user
// profile, entirely outside the engine. The engine never learns about
// "saved format #3" — it only ever receives a fully-resolved
// `CustomFormatRules` value at game-start time, exactly as it already does
// for the four EC/Swedish presets. `CustomFormatId` therefore stays what it
// was always designed to be: a lightweight, `Copy`, per-`GameState`
// transport tag. Well-known, stable IDs matter only for the registry-backed
// presets (so both peers' registries can label the same preset the same
// way); an ad-hoc lobby-saved format can use a single reserved sentinel ID,
// since the ID itself carries no meaning once the full payload travels with
// it.
//
// VERSION SKEW (flagged, not fully resolved here): an older client that has
// never heard of the `GameFormat::Custom` enum variant at all cannot be
// rescued by `#[serde(default)]` on `custom_rules` — the failure happens at
// the enum-variant level during deserialization of `format: GameFormat`
// itself, before any field-level default ever applies. This is the same
// class of risk any new `GameFormat` variant already carries, but custom
// formats make it far more frequent in practice (a new custom format can be
// created at any time, not just at a client release). The lobby-join flow
// needs an explicit compatibility check — reject with a clear "this game
// needs a custom-format-capable client" message — rather than relying on
// deserialization to fail gracefully. Not designed in depth here; flagged as
// a real requirement for whoever implements the lobby-join handshake.

ReprintPolicy {                         // enum — enforceable today only at set-code granularity
    OriginalPrintingsOnly,              // 93-94 / Classic intent (see limitation)
    AllowSpecialReprintSets,            // CE/ICE/world-champ/proof set codes included in legal_sets
    AllowAnyPrinting,                   // Middle School "begrudgingly"
}

// NOT YET DESIGNED — flagged, not resolved, per CONTEXT.md open item #2.
// `ReprintPolicy` above gates LEGALITY (which printings are legal to play).
// A user follow-up asks for a sibling, independent axis: DISPLAY DEFAULT
// (which specific legal printing's frame/art renders by default when a card
// is added to a deck under this format) — e.g. an old-rules format should
// default old-frame Alpha/Beta art over a modern reprint's, without forcing
// the player to manually pick a printing every time.
//
// Per CONTEXT.md's corrected finding: this is NOT the same gap as
// ReprintPolicy's set-code-membership approximation. Legality (engine/MTGJSON,
// set-codes only) and frame/art data (frontend/Scryfall, already has
// released_at/border_color/frame_effects/full_art via `scryfall-printings.json`
// + `preferencesStore`'s `ArtChainEntry`) are two disjoint systems today that
// have never been cross-referenced. Building this means either (a) new
// engine-owned derived state — a WASM-exposed "preferred printing for name X
// given format Y" API, reusing `SetCatalog`/`SetMeta.release_date` (already
// loaded, already projected to `client/public/set-list.json` for an unrelated
// purpose) for chronological ordering — or (b) extending the frontend's
// existing `ArtChainEntry` cosmetic-preference chain with a new variant seeded
// by the format's reprint policy, treating this as display preference rather
// than engine-derived game state. (a) fits "engine owns all logic" more
// cleanly; (b) reuses a system that already does exactly this job for
// individual players today. This fork needs the same maintainer conversation
// as the rest of this design — do not build either without it.
//
// Whatever shape it takes, per CLAUDE.md's bool-vs-enum rule (and consistent
// with ReprintPolicy itself being a 3-way enum, not a bool), a bare
// `use_old_frame: bool` field would be the wrong shape. If/when designed, this
// should be a typed axis analogous to `ArtChainEntry` — sketch only, NOT a
// final design:
//   PrintingDefault {
//       Newest,                   // today's implicit behavior
//       OldestLegal,              // oldest printing within this format's legal_sets
//       SpecificSet(SetCode),     // pin to one set's frame (e.g. "Alpha only" preset)
//   }

LegacyRuleSet {                         // INDEPENDENT era-rule axes (RESEARCH §8, §10)
    mana_burn: bool,
    damage_uses_stack: bool,
    pre_m10_wish_reaches_exile: bool,   // RESEARCH §9: Wishes reach owned face-up
                                        // exile (pre-M10 "removed from the game").
                                        // NOTE: renamed from the first-pass
                                        // placeholder `pre_m10_wish_templating`
                                        // — it is a functional POOL-SCOPE toggle,
                                        // not a wording/templating change.
    legend_rule_scope: LegendRuleScope, // RESEARCH §10: modern per-controller
                                        // (default) vs pre-M14 any-controller.
                                        // A typed enum, NOT a bool (the historical
                                        // space is not a clean binary and this
                                        // leaves room without a later refactor).
}

LegendRuleScope {                       // RESEARCH §10: legend-rule controller scope
    Modern,                             // CR 704.5j: per-controller + choice (post-2013-07 M14).
                                        // DEFAULT — all four EC presets use this.
    PreM14AnyController,                // pre-M14: same-named legends across ALL
                                        // controllers all go to owners' graveyards,
                                        // choiceless (Sixth-Edition "both die" form).
}
```

**Why `legend_rule_scope` is a general axis, not an EC-preset behavior.** RESEARCH
§10 establishes the legend-rule scope change (Legends 1994 / pre-M14 = global,
any-controller; M14 2013-07 = per-controller + choice) is a REAL functional
difference — but **none of the four EC presets turn it on** (EC's published rules
list mana burn / damage-on-stack / wish as their only legacy exceptions, never a
legend-rule reversion), so all four set `LegendRuleScope::Modern`. It is included
as a general historical-rules axis the engine can express for other era/custom
formats — exactly the orthogonality Block Constructed proves for `mana_burn`
(see the note below). Planeswalker uniqueness needs **no** flag: the four EC
pools top out at Scourge (2003) and planeswalkers postdate that (Lorwyn 2007),
so no EC-legal card is a planeswalker (RESEARCH §10c).

**Where the payload lives.** `FormatConfig` gains
`custom_rules: Option<CustomFormatRules>` (serde `#[serde(default,
skip_serializing_if = "Option::is_none")]`). Because `FormatConfig` is already
the per-game config carried on `GameState` and serialized across the WASM/P2P
boundaries, embedding the resolved ruleset there means **no global mutable
registry** is needed at runtime — deck validation and the mana-burn hook both
read it from the config they already hold. `GameFormat` stays `Copy` (the heavy
`Vec`s live in `FormatConfig`, which is `Clone`, not the enum).

**Bundled presets (Axis B)** are typed constructors, exactly analogous to
`FormatConfig::premodern()`:

```text
CustomFormatDef::old_school_93_94() -> CustomFormatDef
CustomFormatDef::old_school_95()    -> extends 93-94's set/list Vecs
CustomFormatDef::middle_school()    -> restricted empty, larger banned
CustomFormatDef::classic_magic()    -> own combined lists
custom_format_registry() -> Vec<CustomFormatDef>   // parallel to GameFormat::registry()
```

**Lobby saves (Axis A)** produce the identical `CustomFormatDef` shape from a
different origin — a name plus the live `FormatConfig` a host just finished
tuning, with `legality` left at defaults (`legal_sets: None`, empty
banned/restricted, default `LegacyRuleSet`):

```text
CustomFormatDef::from_lobby_config(name: String, config: &FormatConfig) -> CustomFormatDef
```

Per maintainer review round 2 (CONTEXT.md point 2): this conversion must
capture every `StructuralRules` field from the live `FormatConfig` — full
fidelity, not a partial snapshot. If a future caller ever invokes this from a
`FormatConfig` state this conversion cannot faithfully represent (there is
none identified today, since `StructuralRules` now mirrors every
independently-meaningful `FormatConfig` field), it must reject explicitly
rather than silently drop data. The `CustomFormatId` this constructor
allocates is the ad-hoc sentinel described above, not a registry-stable ID.

Both paths converge on the same `custom_rules: Option<CustomFormatRules>` field
and the same `GameFormat::Custom(CustomFormatId)` variant — there is exactly
one runtime representation of "a custom format," authored two different ways.
See §7 for where each one is surfaced to a player.

(`CustomFormatDef` = display metadata + `CustomFormatRules`; the registry hands
the frontend labels/short-labels/descriptions just like `FormatMetadata`.)

## 2. Parameterizing the formats as data (not N blocks)

**Phase 1 preset — `swedish_old_school()` (see CONTEXT.md's "Further narrowing
Axis B's MVP").** This is the first Axis B preset targeted, chosen because it
needs zero `LegacyRuleSet` engine wiring. **Registration is currently blocked
by the preset-readiness gate (§7)** — its `ReprintPolicy` is unenforced by §3
and its specific value is unconfirmed (CONTEXT.md Open item 6) — so the
engine/schema work below can and should land in phase 1, but this preset does
not appear as a selectable format until those two items resolve:

- `swedish_old_school()` — sets = `Some([LEA, LEB, 2ED, ARN, ATQ, LEG, DRK,
  SUM])` (verify MTGJSON codes at implementation time, same caveat as below —
  `SUM` for "Summer Magic" is a placeholder pending that verification; fixed
  this round to include Summer Magic, which CONTEXT.md's legal-sets list
  already named but this preset sketch had dropped); banned = `[]` (a
  genuinely empty list — the schema must support this, not assume non-empty,
  and `legal_sets: Some(...)` here — never `None` — since this preset DOES
  restrict the pool); restricted = [**25** names, verbatim in CONTEXT.md —
  corrected this round from a "23" miscount]; legacy = default (all
  `false`/`Modern` — no mana burn, no damage-on-stack, no Wish/legend-rule
  reversion). Ante-card handling and reprint policy are open per CONTEXT.md
  items 5–6; do not encode either without resolving those first.

**Phase 2 presets — the four EC formats**, unchanged from the original design,
now explicitly sequenced after phase 1 (§8) since they need the
`LegacyRuleSet` wiring in §4:

The four formats form an incremental chain. Express it with builder-style reuse,
mirroring how `FormatConfig::pioneer()` spreads `..Self::standard()`:

- `old_school_93_94()` — base: sets = [LEA, LEB, 2ED, CED, CEI, ARN, ATQ, 3ED,
  LEG, DRK, FEM]; restricted = [22 names]; banned = [7 names]; legacy = {mana_burn}.
- `old_school_95()` — `let mut d = old_school_93_94(); d.legal_sets.extend([4ED,
  ICE, CHR, REN, HML]); d.restricted.extend([Demonic Consultation, Mana Crypt]);
  d.banned.extend([Amulet of Quoz, Timmerian Fiends]; legacy unchanged`.
- `middle_school()` — sets = Fourth Edition..Scourge; restricted = []; banned =
  [25 names]; reprint = AllowAnyPrinting; legacy = {mana_burn, damage_uses_stack,
  pre_m10_wish_reaches_exile}.
- `classic_magic()` — sets = Alpha..Scourge; restricted = [**44** names —
  corrected this round from a "37" mislabel; recounted directly against
  RESEARCH.md's verbatim list]; banned =
  [11 names]; reprint = OriginalPrintingsOnly; legacy = {mana_burn,
  damage_uses_stack, pre_m10_wish_reaches_exile}.

Set codes must be verified against the engine's `set_catalog` (MTGJSON codes)
during implementation — the codes above are the expected MTGJSON codes but
must be confirmed, not assumed. Card names are validated at preset-construction
time by a unit test that asserts every banned/restricted name resolves in the
`CardDatabase` (guards against typos silently no-op'ing a ban).

## 3. Deck-legality algorithm (engine, data-driven)

`evaluate_custom_format(db, request, rules) -> CompatibilityCheck`:

1. Structural checks (deck size, sideboard) via existing `FormatConfig` fields.
2. Pool legality: `match &rules.legal_sets { None => every card passes this
   check (no set restriction — Axis A's default), Some(sets) => for each
   card, db.printings_for(name); legal iff any printing's set code ∈ sets,
   else → illegal ("not legal in <format>"), reusing the existing
   `illegal_cards` accumulation shape }`. **Fixed per maintainer review round
   2** (CONTEXT.md point 1): the prior version checked membership
   unconditionally against a bare `Vec`, so an empty Vec (Axis A's actual
   default) rejected every card instead of none. The `None` arm is the
   correctness-critical addition — test both arms independently (§6).
3. Banned: name ∈ `rules.banned` → illegal (distinct "banned" label).
4. Restricted: name ∈ `rules.restricted` → insert into `restricted_canonical`,
   then call the **existing** `restricted_copy_violations` (CR 100.2b, `<= 1`).
5. Default 4-copy limit + card-intrinsic overrides via the **existing**
   `copy_limit_violations`.

This reuses four existing helpers verbatim and adds only the set-membership +
name-set sourcing. `GameFormat::Custom` gets one arm in
`format_compatibility_check` routing to `evaluate_custom_format`.

**Honest gap, flagged by maintainer review round 3 (CONTEXT.md point 5):**
`rules.reprint_policy` is declared on `LegalityRules` but **not consumed by
any step above** — a card passes step 2 based on legal-set membership alone,
regardless of which `ReprintPolicy` the preset declares. This is the same gap
as the already-flagged Open item 2 (printing/frame-data gap): real
enforcement needs the engine-vs-frontend printing cross-reference that item
already describes, not new work invented here. Per the new preset-readiness
rule (CONTEXT.md point 5, §7): no preset may be registered as selectable
while declaring a `ReprintPolicy` this algorithm doesn't enforce — this
blocks `swedish_old_school()` specifically until Open items 2 and 6 resolve
(see §7).

`legality_format()` returns `None` for `Custom` (no `LegalityFormat` mapping —
custom formats don't use the external legality table). `label`, `for_format`,
etc. each get a `Custom` arm reading the resolved def. `sideboard_policy` is
the one exception to "add a `Custom` arm on the `GameFormat` method" — see the
dedicated accessor design in §1: the correct arm lives on `FormatConfig::
sideboard_policy()`, not on `GameFormat::sideboard_policy()` itself, since the
latter has no access to `custom_rules`.

## 4. Legacy rules wiring — Phase 2 (not needed for the Swedish Old School preset)

Everything in this section is deferred to phase 2 (§8) — the phase-1 preset
(`swedish_old_school()`, §2) exercises none of it, by design, since the
Swedish ruleset makes no mention of any of these rules and uses fully modern
defaults. This section is unchanged from the original design and remains the
correct plan for phase 2's four EC-format presets.

- **Mana burn** (`LegacyRuleSet.mana_burn`) — **REWRITTEN AGAIN per
  maintainer review round 3** (CONTEXT.md point 1). Round 2 fixed the
  life-loss-not-damage half but got the mechanism wrong: it gated only the
  LIFE-LOSS check to phase-group boundaries while leaving the pool-emptying
  itself firing on every `Phase` transition — so by the time a phase-group
  boundary was reached, the pool had already been silently drained at the
  prior intra-phase-group step, with nothing left to burn. Two things
  verified directly this session that the round-2 fix missed:
  - **Life loss, not damage** (unchanged from round 2, still correct):
    `docs/MagicCompRules.txt:8278` (obsolete-rules glossary): "unspent mana
    caused a player to **lose life**." Damage and life loss are behaviorally
    different in this engine (damage can be prevented/redirected and
    triggers "dealt damage" abilities; life loss does neither).
  - **The pool itself must persist across intra-phase-group steps, not just
    skip the burn check.** The engine's `Phase` enum (`types/phase.rs`)
    flattens MTG's steps AND phases into one 11-variant list (e.g.
    `DeclareAttackers`/`DeclareBlockers`/`CombatDamage` are three separate
    `Phase` variants inside the single real "Combat" phase); modern CR 500.5
    empties the pool at every one of them, and `turns.rs`'s `enter_phase`
    sets `state.phase = next` near the top of the function — so anything
    checking "current vs. next" phase after that line sees the destination
    on both sides unless it captured the pre-transition value first.

  **Design — reuses an existing mechanism instead of inventing pool
  suppression.** The engine already has exactly this shape:
  `ManaExpiry` (`types/mana.rs:1509`) has `EndOfTurn` and `EndOfCombat`
  variants; `EndOfCombat`'s own doc comment reads "Mana persists through
  combat steps but drains at EndCombat → PostCombatMain," used by
  Firebending — literally "persist through this phase's internal steps,
  drain at the real phase-group boundary," already built, tested, and
  working, just special-cased to the Combat phase-group. The fix
  generalizes it:
  1. Add `ManaExpiry::EndOfPhaseGroup` — a **third** variant, not five new
     per-phase-group variants (`EndOfBeginning`, `EndOfPrecombatMain`, …).
     Like `EndOfCombat` and `EndOfTurn`, it does not parameterize which
     phase-group; it resolves contextually against whichever one is active
     when checked, via the same `fn phase_group(p: Phase) -> PhaseGroup`
     mapping round 2 introduced (5 variants: Beginning, PrecombatMain,
     Combat, PostcombatMain, Ending — generalizing the ad-hoc `in_combat`
     bool `turns.rs` already computes inline).
  2. Where mana units are constructed (`types/mana.rs`, the `ManaUnit`
     construction sites currently setting `expiry: None`), when the active
     format's `LegacyRuleSet.mana_burn` is set, construct with
     `expiry: Some(ManaExpiry::EndOfPhaseGroup)` instead.
  3. Extend the existing expiry-clearing logic (the generalization of
     `clear_expired_end_of_combat_retention_markers`, called from
     `enter_phase` where the pre-transition phase is still available before
     `state.phase = next` overwrites it) to detect a real phase-group
     crossing and convert those units to ordinary (`None`-expiry) units —
     exactly mirroring how `EndOfCombat` units convert at `EndCombat` →
     `PostCombatMain` today. Converted units flow into that SAME
     transition's already-firing `EmptyManaPool` event as `Drop`
     decisions — no new event, no suppressed event, the existing
     unconditional per-transition drain does the work once the units are no
     longer expiry-protected.
  4. Because units tagged `EndOfPhaseGroup` never enter the Drop path except
     at a real phase-group crossing (by construction — an intra-phase-group
     transition's `clear_expired_...` pass leaves them untouched, mirroring
     exactly how live `EndOfCombat` units are excluded from Drop mid-combat
     today), the drop count a `mana_burn` player has at ANY transition where
     drops occur *is* the burn amount — no separate "was this a boundary"
     branch needed at the life-loss application point itself. Apply that
     count as **life loss** (`GameEvent::ManaBurn { player_id, amount }`,
     distinguishable from a Yurlok-class event), computed at the SAME
     aggregation point `apply_empty_mana_pool_event` already uses for the
     existing `player_unspent_mana_loss_causes_life_loss` (Yurlok-class)
     check — independent of it, since a format flag and a card-granted
     static ability are different triggers that could theoretically both
     apply to the same event — and AFTER any player choice the drain pauses
     for (Kruphix/Horizon Stone `Keep` dispositions), reusing the pipeline's
     existing pause/resume continuation rather than adding a second,
     unsynchronized computation point (this resolves the "life loss can
     defer through replacement handling" concern directly).
  Annotate as a pre-M10 rule removed by the M10 update (cite the
  obsolete-glossary entry `MagicCompRules.txt:8277-8278`). Slightly larger
  than round 2's estimate (a new `ManaExpiry` variant + its construction-site
  and clearing-logic wiring, not just a life-loss check at an existing call
  site) but still small and, critically, reuses a proven pattern rather than
  adding a new one.
- **Pre-M10 Wish exile access** (`pre_m10_wish_reaches_exile`): fully traced in
  RESEARCH §9. This is a REAL functional difference (not wording-only): pre-M10,
  the "removed from the game" zone counted as *outside the game*, so a Wish could
  retrieve an owned card that had been removed from the game (modern: exile); the
  M10 update (CR 400.11/400.11a — "outside the game is not a zone"; only the
  sideboard is outside the game) removed this. The engine already implements the
  modern (post-M10) Wish cycle generically as `Effect::SearchOutsideGame` with
  `source_pool: OutsideGameSourcePool::Sideboard` (`types/ability.rs:246`), and
  already implements owned-face-up-exile retrieval for the Karn/Coax class via
  `OutsideGameSourcePool::SideboardAndFaceUpExile` + the tested
  `collect_face_up_exile_candidates` collector and `put_face_up_exile_into` mover
  (`game/effects/search_outside_game.rs:72,105,141`). **SMALL** (revise the prior
  "Medium"): the only change is a one-line pool-widening at the existing resolver
  hook (`search_outside_game.rs:72`) — when the flag on
  `GameState.format_config` (`types/game_state.rs:6787`) is set, treat a
  `Sideboard`-pool search as if it were `SideboardAndFaceUpExile`. No parser
  change (the flag is a runtime-resolution concern, not a parse concern), no new
  effect/state/WaitingFor, full reuse of the tested collector/mover. Annotate as
  a legacy rule reverting the M10 change (cite CR 400.11 / 400.11a / 701.23j).
- **Damage uses the stack** (`damage_uses_stack`): LARGE (RESEARCH §6). Its own
  sub-project; likely out of MVP. Gated by the flag so Middle School / Classic
  are playable without it (with a documented fidelity caveat) until it lands.
- **Legend-rule scope** (`legend_rule_scope`): SMALL (RESEARCH §10). The pre-M14
  form is *simpler* than the modern one — global (group same-named legendaries
  across all controllers, no `obj.controller == player_id` filter) and choiceless
  (all members of a ≥2 group go to owners' graveyards, no `WaitingFor::ChooseLegend`),
  which is exactly the shape of the existing `check_world_rule` (`sba.rs:1348`,
  CR 704.5k). The change is one branch at the top of `check_legend_rule`
  (`sba.rs:902`) selecting global-choiceless vs the current per-controller-choice
  path based on the resolved `GameState.format_config` scope, reusing the shared
  `move_sba_departing_permanent` mover (`sba.rs:618`) and the unchanged
  `legend_rule_exempt_with_gate` filter (`sba.rs:880`). No new WaitingFor, no new
  mover, no new state machine. **Not used by any of the four EC presets** (all
  default to `Modern`); shipped as the general historical-rules axis. Annotate as
  a legacy rule reverting the M14 change (cite CR 704.5j).

## 5. Frontend

The `custom_format_registry()` is exposed through the same WASM export path as
`GameFormat::registry()`; the client renders custom formats in the picker from
engine data (label, short-label, description, group). A new `FormatGroup`
variant (e.g. `Retro` or `Custom`) may be added for visual clustering. No
legality logic on the client.

## 6. Testing (building-block level, per CLAUDE.md)

- Set-membership legality: assert cards from in-pool and out-of-pool sets pass /
  fail — for arbitrary set lists, not just the four presets.
- **`legal_sets: None` accepts every card** (new — maintainer review round 2,
  CONTEXT.md point 1): a synthetic `CustomFormatRules` with `legal_sets: None`
  must legalize a card from an arbitrary, otherwise-never-configured set —
  the discriminating case that catches the empty-`Vec`-rejects-everything
  regression directly. Paired with the existing `Some(list)` restricted case
  above so both arms of the `Option` are independently covered, not just the
  restricted one.
- Restricted path: 2 copies of a restricted-list name flags; 1 copy passes —
  driven by a synthetic def, proving the general mechanism.
- Preset integrity: every banned/restricted name in all four EC presets plus
  `swedish_old_school()` resolves in the DB; each preset's `legacy` matches
  its source ruleset; each preset's stated list *count* in a doc comment
  matches `.len()` of the actual list (guards against the round-2 23-vs-25 /
  37-vs-44 class of mislabeling recurring silently).
- **Mana burn — pool persistence AND life loss** (revised again — maintainer
  review round 3, CONTEXT.md point 1; round 2's version only tested the
  life-loss gate and would have passed against the wrong mechanism):
  - With `LegacyRuleSet.mana_burn` set, unspent mana added during
    `DeclareAttackers` must still be present (not dropped, no life loss) at
    `DeclareBlockers` (a transition WITHIN `PhaseGroup::Combat`) — asserts
    the pool itself persists via `ManaExpiry::EndOfPhaseGroup`, not just that
    a life-loss check was skipped.
  - That same unspent mana, carried untouched through every step of Combat,
    must be dropped AND cause life loss equal to the full accumulated amount
    at the `EndCombat` → `PostCombatMain` transition (crossing a phase-group
    boundary) — a single discriminating test proving both the persistence
    and the eventual burn, not two independently-passable halves.
  - Assert life loss specifically (no `GameEvent::DamageDealt`, no
    prevention-shield interaction).
  - With the flag off, mana empties at every transition exactly as today
    (regression guard against changing default behavior).
  - A pause case: unspent mana at a real phase-group boundary where a
    `Keep`-disposition replacement effect (Kruphix/Horizon Stone-shaped
    synthetic def) also applies — burn amount reflects only the units
    actually dropped after that choice resolves, proving the life-loss
    application reuses the pipeline's existing pause/resume point rather
    than computing before the choice is known.
- **`FormatConfig::sideboard_policy()` accessor** (new — maintainer review
  round 3, CONTEXT.md point 2): a `Custom`-format `FormatConfig` with
  `structural.sideboard_policy: Forbidden` must make `deck_loading.rs`'s
  sideboard-dropping behavior and `match_flow.rs`'s max-sideboard-size
  computation both observe `Forbidden` via the new accessor — a regression
  test pinned to the two migrated call sites specifically, not just the
  accessor's return value in isolation, so a future revert of the migration
  (reverting to `.format.sideboard_policy()`) fails visibly.
- **`uses_commander` requires both conditions** (new — maintainer review
  round 3, CONTEXT.md point 4): `command_zone: true` alone (no threshold) and
  `commander_damage_threshold: Some(_)` alone (no command zone) both derive
  `uses_commander: false`; only both together derive `true` — the exact
  mismatched-combination cases the review asked for.
- Serde round-trip of `FormatConfig` with `Some(custom_rules)` and `None`.
- `CustomFormatDef::from_lobby_config`: a `FormatConfig` with non-default
  `starting_life`/`max_players`/`deck_size`/`range_of_influence`/`team_based`/
  `command_zone`/`commander_damage_threshold`/`sideboard_policy` round-trips
  into a `CustomFormatRules.structural` that matches field-for-field, with
  `legality` at its defaults (`legal_sets: None`) — the general Axis-A save
  mechanism, not a specific saved format's values. Does NOT include
  `archenemy_player` (removed per round 3, CONTEXT.md point 3 — not part of
  `StructuralRules` at all).

## 7. Delivery surface — RESOLVED via maintainer input, see CONTEXT.md "Maintainer input"

Previously framed as a three-way (a) UI / (b) config / (c) both choice with a
(b)-first recommendation. **Resolved to (c), entered through Axis A via the
existing lobby, not a new designer screen and not (b)-first.** The three
options below are kept for the record; the recommendation that follows
supersedes the original one.

- **(a) Player-facing designer UI.** A from-scratch client screen to author a
  format interactively. *Rejected as the entry point* — unnecessarily large
  (persistence, sharing/import, validation UX all designed before anything
  ships) when the near-identical capability already has a home.
- **(b) Operator/preset config.** Formats delivered as bundled, version-
  controlled preset data (the four EC formats as `CustomFormatDef`
  constructors), optionally extendable by a self-hosted instance via a config
  the server loads at startup. *Changes:* schema + presets + one
  deck-validation arm + the registry export. Presets are typed Rust
  constructors, audited like `FormatConfig::premodern()`. Still the right
  shape for Axis B (a banned list should be curated, not free-typed by a
  player) — no longer the *first* thing to ship, just Axis B's delivery shape.
- **(c) Both, one schema, Axis-A-first.** Extend the existing lobby
  (`HostSetup.tsx`) with a "save as custom format" action once a host has
  tuned `starting_life`/player count/`deck_size`/`range_of_influence`/
  `team_based`/`singleton` to taste — this calls
  `CustomFormatDef::from_lobby_config(name, &config)` and registers the
  result. Axis B ships via (b)'s typed presets, on the same schema, in
  parallel or immediately after.

**Recommendation: ship (c), Axis-A-first.** Reasoning: (1) a "save" button on a
screen that already exposes most of Axis A's fields is a *much* smaller slice
than a new designer UI — no new persistence design, no new sharing mechanism
beyond whatever the lobby already does for a format choice, no new
multiplayer-agreement problem (the host's `FormatConfig` is already
authoritative and transmitted today). (2) It's useful to every casual
multiplayer table immediately, not gated behind the four EC formats being
finished. (3) It still fully validates the schema — `CustomFormatDef` is the
single source of truth whichever path authors it, so the four EC presets
remain a straightforward Axis B addition on the identical type, not a
redesign. (4) The harder, genuinely-new Axis B work (legal sets, banned lists,
legacy rules) is unaffected in scope or design — this only changes what ships
*first* and *how a player reaches Axis A*, not what Axis B needs to be
correct.

**Preset-readiness gate (new — maintainer review round 3, CONTEXT.md point
5).** A preset (Axis B typed constructor OR an Axis A lobby save that sets
non-default `legality`) must not be registered/exposed as a selectable format
until every legality/legacy field it declares is BOTH fully specified AND
actually consumed by the evaluator/engine at an authoritative seam — not
merely present as a struct field. This is a general rule, not a one-off:
- `swedish_old_school()` (phase 1) is currently BLOCKED from registration by
  this rule: it declares a `ReprintPolicy`, which §3 confirms is not yet
  enforced, and CONTEXT.md's Open item 6 confirms the specific policy value
  is itself unconfirmed against the primary source. Both must resolve before
  this preset ships as selectable, even though the engine-level schema and
  evaluator work (§1, §3) can and should land first.
- No phase-2 EC preset may declare `LegendRuleScope::PreM14AnyController`
  until the historical conflation flagged in CONTEXT.md/RESEARCH.md resolves.
  Moot today (all four EC presets default this to `Modern`), but binding on
  any future preset.

## 8. Sequencing

**Phase 1 — general engine + the smallest end-to-end slice.** Deliberately
excludes all `LegacyRuleSet` engine wiring; ships something real and testable
first.

1. **General engine** — `CustomFormatId`, `CustomFormatRules` (with
   `StructuralRules` + `LegalityRules` sub-structs, §1), `ReprintPolicy`,
   `LegacyRuleSet` (present in the schema, unused by anything phase 1 ships),
   `GameFormat::Custom` variant + all match arms, `FormatConfig.custom_rules`,
   `evaluate_custom_format` reusing existing enforcers, registry export.
   (Compiler-guided, mirrors phase 53.)
2. **Axis A lobby save** — `CustomFormatDef::from_lobby_config`, a "save as
   custom format" action on `HostSetup.tsx`, and a load path so a saved
   `CustomFormatDef` appears as a selectable format on return visits. Small
   frontend-plus-plumbing slice; useful on its own to any casual table.
   Validates the schema's Axis A end from a real UI.
3. **`swedish_old_school()` preset (Axis B, phase 1)** — the one Axis B
   preset that needs zero legacy-rules wiring (§2). Validates the engine's
   Axis B end (legal-set membership, empty banned list, the 25-name
   restricted list) without touching §4 at all. Resolve CONTEXT.md items 5–6
   (ante-card handling, reprint policy) before finalizing this preset's
   constructor. **Registration as a selectable format is gated separately
   (§7's preset-readiness gate)** on `ReprintPolicy` enforcement landing in
   §3 — the constructor and its tests can land in this step, but do not wire
   it into the format-selection UI/registry as choosable until that gate
   clears. Can proceed in parallel with step 2 — disjoint fields of the same
   schema.

**Phase 2 — the four EC formats + the legacy-rules engine work they need.**
Ships once phase 1 has proven the schema and both axes end-to-end.

4. **Four EC formats as data (Axis B)** — the four `CustomFormatDef`
   constructors + registry entries + preset-integrity tests (§2).
5. **Mana burn** — `LegacyRuleSet.mana_burn` at the drop-site hook. Small.
   Enables full fidelity for 93-94 / 95 and partial for Middle School / Classic.
6. **Pre-M10 Wish exile access** (`pre_m10_wish_reaches_exile`) — SMALL
   (RESEARCH §9); one-line pool-widening at `search_outside_game.rs:72`, gated by
   the flag, reusing the existing tested face-up-exile collector/mover. Can land
   with or just after mana burn (step 5).
7. **Damage uses the stack** — LARGE; its own sub-project; likely post-MVP
   even within phase 2. Middle School / Classic are playable-with-caveat
   until it lands.
8. **Eternal Chaos (stretch)** — depends on 93-94 existing (step 4) **plus** a
   genuinely new in-match pack-opening mechanic (Booster Tutor / Opening
   Ceremony / Summon the Pack + tutor-from-opened-packs errata + pack-based
   sideboard). Not designed here; flagged as a follow-on that is mostly a new
   *mechanic*, only lightly a new *format*.

## Risks / open items

- **Set-code verification** against `set_catalog` at implementation time (codes
  in §2 are expected-but-unconfirmed MTGJSON codes).
- **Reprint-policy fidelity** limited to set-code granularity (RESEARCH §3) —
  frame/art enforcement needs new per-printing data; flagged for the user.
- **Damage-uses-the-stack** effort is genuinely large and not fully scoped here;
  do not commit it to MVP without a dedicated combat-rework spike.
