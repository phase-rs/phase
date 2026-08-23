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
// (`types/format.rs:174-202`) and several are already host-adjustable today in
// `client/src/components/lobby/HostSetup.tsx` for a chosen built-in
// `GameFormat` — this struct is a NAMED, SAVEABLE snapshot of that same
// knob set, not new game-rule surface. This is the axis the lobby "save as
// custom format" action captures.
StructuralRules {
    starting_life: i32,
    min_players: u8,
    max_players: u8,
    deck_size: u16,
    singleton: bool,
    range_of_influence: Option<Box<RangeOfInfluenceConfig>>,  // mirrors FormatConfig's field exactly
    team_based: bool,
}

// Axis B — legality/era-rules. Genuinely new data; no existing UI surface.
// This is what makes the four EC formats rules-correct; kept exactly as
// originally designed, just now named as its own struct rather than sharing
// `CustomFormatRules`'s top level with Axis A undifferentiated.
LegalityRules {
    legal_sets: Vec<SetCode>,           // set codes; membership = pool legality
    reprint_policy: ReprintPolicy,
    banned: Vec<CardName>,              // fully illegal
    restricted: Vec<CardName>,          // legal, max 1 (CR 100.2b path)
    legacy: LegacyRuleSet,
}

// A format saved from the lobby ("FFA, but I bumped starting life to 30 and
// capped it at 4 players") sets `structural` and leaves `legality` at
// defaults (no set restriction, no legacy rules) — a fully valid
// `CustomFormatRules` value. The four EC formats set `legality` to real data
// and `structural` to sane multiplayer/duel defaults. Neither axis requires
// the other to be present; this is the orthogonality Block Constructed
// already proved for `LegacyRuleSet` (§ below), now proved a second time
// between Axis A and Axis B.

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
tuning, with `legality` left at defaults:

```text
CustomFormatDef::from_lobby_config(name: String, config: &FormatConfig) -> CustomFormatDef
```

Both paths converge on the same `custom_rules: Option<CustomFormatRules>` field
and the same `GameFormat::Custom(CustomFormatId)` variant — there is exactly
one runtime representation of "a custom format," authored two different ways.
See §7 for where each one is surfaced to a player.

(`CustomFormatDef` = display metadata + `CustomFormatRules`; the registry hands
the frontend labels/short-labels/descriptions just like `FormatMetadata`.)

## 2. Parameterizing the four EC formats as data (not four blocks)

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
- `classic_magic()` — sets = Alpha..Scourge; restricted = [37 names]; banned =
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
2. Pool legality: for each card, `db.printings_for(name)`; legal iff any
   printing's set code ∈ `rules.legal_sets`. Else → illegal ("not legal in
   <format>"), reusing the existing `illegal_cards` accumulation shape.
3. Banned: name ∈ `rules.banned` → illegal (distinct "banned" label).
4. Restricted: name ∈ `rules.restricted` → insert into `restricted_canonical`,
   then call the **existing** `restricted_copy_violations` (CR 100.2b, `<= 1`).
5. Default 4-copy limit + card-intrinsic overrides via the **existing**
   `copy_limit_violations`.

This reuses four existing helpers verbatim and adds only the set-membership +
name-set sourcing. `GameFormat::Custom` gets one arm in
`format_compatibility_check` routing to `evaluate_custom_format`.

`legality_format()` returns `None` for `Custom` (no `LegalityFormat` mapping —
custom formats don't use the external legality table). `sideboard_policy`,
`label`, `for_format`, etc. each get a `Custom` arm reading the resolved def.

## 4. Legacy rules wiring

- **Mana burn** (`LegacyRuleSet.mana_burn`): at the step-end drop site
  (`types/mana.rs:1707`), tally dropped units per player; after the drain, if
  the flag is set, deal that many damage to the owner and emit a new
  `GameEvent::ManaBurn { player_id, amount }`. Annotate as a pre-M10 rule
  removed by the M10 update (cite the obsolete-glossary entry
  `MagicCompRules.txt:8277`). ~Small; no new state machine.
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
- Restricted path: 2 copies of a restricted-list name flags; 1 copy passes —
  driven by a synthetic def, proving the general mechanism.
- Preset integrity: every banned/restricted name in all four presets resolves in
  the DB; each preset's `legacy` matches the EC spec.
- Mana burn: unspent N mana at step end deals N with flag on, 0 with flag off —
  tests the flag+hook, not a specific card.
- Serde round-trip of `FormatConfig` with `Some(custom_rules)` and `None`.
- `CustomFormatDef::from_lobby_config`: a `FormatConfig` with non-default
  `starting_life`/`max_players`/`deck_size`/`range_of_influence`/`team_based`
  round-trips into a `CustomFormatRules.structural` that matches field-for-
  field, with `legality` at its defaults — the general Axis-A save mechanism,
  not a specific saved format's values.

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

## 8. Sequencing

1. **General engine** — `CustomFormatId`, `CustomFormatRules` (with
   `StructuralRules` + `LegalityRules` sub-structs, §1), `ReprintPolicy`,
   `LegacyRuleSet`, `GameFormat::Custom` variant + all match arms,
   `FormatConfig.custom_rules`, `evaluate_custom_format` reusing existing
   enforcers, registry export. (Compiler-guided, mirrors phase 53.)
2. **Axis A lobby save** — `CustomFormatDef::from_lobby_config`, a "save as
   custom format" action on `HostSetup.tsx`, and a load path so a saved
   `CustomFormatDef` appears as a selectable format on return visits. Small
   frontend-plus-plumbing slice; ships before the EC formats and is useful on
   its own to any casual table. Validates the schema's Axis A end from a real
   UI, ahead of Axis B.
3. **Four EC formats as data (Axis B)** — the four `CustomFormatDef`
   constructors + registry entries + preset-integrity tests. Validates the
   engine's Axis B from step 1; can proceed in parallel with step 2 since they
   touch disjoint fields of the same schema.
4. **Mana burn** — `LegacyRuleSet.mana_burn` at the drop-site hook. Small.
   Enables full fidelity for 93-94 / 95 and partial for Middle School / Classic.
5. **Pre-M10 Wish exile access** (`pre_m10_wish_reaches_exile`) — SMALL
   (RESEARCH §9); one-line pool-widening at `search_outside_game.rs:72`, gated by
   the flag, reusing the existing tested face-up-exile collector/mover. Can land
   with or just after mana burn (step 4).
6. **Damage uses the stack** — LARGE; its own sub-project; likely post-MVP.
   Middle School / Classic are playable-with-caveat until it lands.
7. **Eternal Chaos (stretch)** — depends on 93-94 existing (step 3) **plus** a
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
