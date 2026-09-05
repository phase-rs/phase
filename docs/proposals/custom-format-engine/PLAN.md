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
  `LegacyRuleSet` axes at their `Modern`/default variant** (`mana_burn:
  ManaBurnPolicy::Modern`, `damage_timing: CombatDamageTiming::Modern`,
  `wish_scope: WishOutsideGameScope::PostM10SideboardOnly`,
  `legend_rule_scope: LegendRuleScope::Modern`). It is a `CustomFormatDef`
  that is purely a set-list restriction.
- A **pre-M10-era block** (a block from before the 2010 "M10" rules update)
  would set `mana_burn: ManaBurnPolicy::Obsolete` (and potentially
  `wish_scope: WishOutsideGameScope::PreM10ReachesExile` / `legend_rule_scope`
  depending on the exact era) via the **same** mechanism — proving
  `LegacyRuleSet` is *not* special-cased to the four EC presets but is a
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
//   - `sideboard_policy` — NOT a `FormatConfig` field today; it's a
//     `GameFormat` method (`format.rs:270`,
//     `fn sideboard_policy(self) -> SideboardPolicy`), computed from the
//     enum variant for built-in formats, with no `FormatConfig` access to
//     check anything for `Custom`. `StructuralRules` carries the DECLARED
//     value explicitly; `FormatConfig` gains a matching STORED field (not a
//     method — see the fallible-validation section below, maintainer
//     review round 4, which also explains why round 3's method-plus-
//     `.expect()` design was itself wrong, not just incomplete).
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
    sideboard_policy: SideboardPolicy,  // the DECLARED value for this custom format;
                                        // FormatConfig.sideboard_policy (a stored field,
                                        // see the fallible-validation note below) is the
                                        // RESOLVED value every consumer actually reads —
                                        // same two-layer shape as uses_commander already has
}

// --- Fallible validation + resolved-context threading (REWRITTEN —
// maintainer review round 4, CONTEXT.md point 1; round 3's `.expect()`-based
// accessor was itself wrong, not just incomplete) ---
//
// Round 3 added `FormatConfig::sideboard_policy()` as a METHOD with an
// `.expect("Custom format must carry custom_rules")` arm, and migrated two
// call sites. Both were mistakes:
//   (a) `.expect()` panics in production if `custom_rules` is ever `None`
//       while `format == Custom(_)` — a malformed/inconsistent value should
//       be rejected where it enters the system, not allowed to exist and
//       panic downstream wherever it's read.
//   (b) Two call sites were nowhere near enough. A full audit this round
//       (grep for every real, non-test consumer of `.uses_commander()` /
//       `.sideboard_policy()` on a bare `GameFormat`) found SEVEN files:
//       `game/companion.rs` (4 sites: `companion_offers`/
//       `companion_starting_deck`-adjacent code at lines 193, 205, 254, 388),
//       `game/deck_loading.rs` (681, 697), `game/match_flow.rs` (64, 357),
//       `game/deck_validation.rs` (98, 1881, 1922, 2273, 2320). Round 3 found
//       none of `companion.rs`'s four; maintainer review round 4 caught one
//       of them (`companion.rs:252-260`) directly.
//
// **Deeper finding this round, beyond what was flagged**: several of these
// functions don't take `&FormatConfig` at all — they take a bare
// `format: GameFormat` parameter (`companion_offers`,
// `companion_starting_deck`, and — found independently this round, not
// previously flagged by anyone — `deck_validation.rs`'s
// `DeckCompatibilityRequest.selected_format: Option<GameFormat>`, threaded
// through `companion_candidates`, `quick_constructed_check`'s caller, and
// the format-dispatch `match` in the ~1900-2300 line range). A method-only
// fix on `FormatConfig` cannot help these — there is no `FormatConfig` in
// scope at all, only the bare enum. This is the real reason "add an
// accessor and migrate the two call sites I already knew about" was never
// going to be sufficient — the actual shape of the problem is that several
// engine call paths were designed assuming `GameFormat` alone carries all
// necessary information, which is true for every built-in format and false
// for `Custom`.
//
// **Fix — validated ingestion + resolved fields, not per-call-site patches:**
//
// 1. `FormatConfig` construction/deserialization gains fallible validation
//    of the `format`/`custom_rules` invariant:
//    `format == GameFormat::Custom(id) ⟺ custom_rules == Some(rules) &&
//    rules.id == id`. Every constructor (`from_lobby_config`, preset
//    constructors, and the WASM/network deserialization boundary) goes
//    through one `fn validate_custom_rules_consistency(&FormatConfig) ->
//    Result<(), FormatConfigError>` check. A malformed value is rejected at
//    the boundary — never constructed, never sent, never silently accepted
//    — so nothing downstream needs to guard against it, and no consumer
//    needs `.expect()`/`.unwrap()` on `custom_rules`.
//
//    **Widened this round (caught by an automated review pass, and a real
//    finding, not overreach — see below): `sideboard_policy` and
//    `uses_commander` are PLAIN serialized fields on `FormatConfig` today
//    (confirmed: `#[serde(default)]` on `supplies_fixed_deck`, no field has
//    `#[serde(skip)]`), meaning a hand-crafted or corrupted wire payload
//    could already set `format: GameFormat::Commander` with
//    `uses_commander: false` and nothing validates the two agree — for
//    BUILT-IN formats, today, before this proposal touches anything. Adding
//    a Custom-only consistency check would leave that pre-existing gap
//    exactly as wide as it already is, just alongside a narrower one for
//    Custom. Since this validation function is the natural place to close
//    it, `validate_custom_rules_consistency` checks derived-field agreement
//    for EVERY format, not only `Custom`: `sideboard_policy ==
//    (match format { Custom(_) => custom_rules.structural.sideboard_policy,
//    other => other.sideboard_policy() })`, and the equivalent for
//    `uses_commander` and `supplies_fixed_deck`. This is a small, genuine
//    improvement to existing behavior that falls out of building the
//    Custom-format check correctly, not scope creep invented for its own
//    sake — flag it to whoever reviews the implementation as a
//    pre-existing-gap fix bundled with new work, in case that combination
//    needs its own sign-off.
// 2. `sideboard_policy` becomes a STORED FIELD on `FormatConfig`
//    (`pub sideboard_policy: SideboardPolicy`), computed once at
//    construction — this MATCHES the existing pattern `uses_commander` and
//    `supplies_fixed_deck` already use (both are stored fields on
//    `FormatConfig`, computed at construction time and kept in sync by a
//    dedicated test, `format.rs:1512`/`:1513` — confirmed this round, not
//    assumed), not a new pattern invented for this feature. For built-in
//    formats, construction sets it from `format.sideboard_policy()`; for
//    `Custom`, from `custom_rules.structural.sideboard_policy` — same shape
//    as how `uses_commander`/`supplies_fixed_deck` are already populated.
// 3. Every call site above migrates from calling `.sideboard_policy()` /
//    `.uses_commander()` on a bare `GameFormat` to reading the STORED FIELD
//    on the `FormatConfig` it already has. Two shapes, not a free per-site
//    choice (tightened this round — a real distinction, not overreach):
//    - `companion_offers` / `companion_starting_deck` genuinely only need
//      the two derived facts (`uses_commander`, `sideboard_policy`), not the
//      full ruleset — a small `ResolvedFormatFacts { uses_commander: bool,
//      sideboard_policy: SideboardPolicy }` copy-type is the right, minimal
//      widening for these two.
//    - `DeckCompatibilityRequest.selected_format` is DIFFERENT: its own
//      dispatch `match` routes to `evaluate_custom_format` (§3) for
//      `Custom`, which needs `legal_sets`/`banned`/`restricted` — the FULL
//      `CustomFormatRules`, not just two derived booleans. A facts-only
//      struct would leave this call site unable to actually evaluate
//      Custom-format deck legality at all. This request struct needs either
//      the whole `&FormatConfig` or an explicit
//      `custom_rules: Option<CustomFormatRules>` field alongside
//      `selected_format`, resolved via a validated registry/config lookup
//      before Custom dispatch — not the lighter facts struct.
// 4. This list is the one confirmed by direct grep this session — treat it
//    as the discovered floor, not a ceiling. Before implementation, run the
//    same audit this repo's `add-engine-variant` skill already prescribes
//    for any new enum variant: search for every consumer of `GameFormat`
//    (not just these two methods) and confirm each one either doesn't need
//    Custom-awareness (rare — most branch on behavior) or is migrated.
//
// `GameFormat::sideboard_policy(self)` and `GameFormat::uses_commander(self)`
// themselves keep their exhaustive match over built-in variants; each gains
// a `Custom(_) => unreachable!("read FormatConfig.sideboard_policy /
// .uses_commander instead — GameFormat alone cannot resolve this")` arm,
// documented as a guard against exactly the mistake round 3 made, not a
// path any correct caller should reach.

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
// REVISED per maintainer review round 6: `reprint_policy` is REMOVED from
// this struct entirely (not just documented-as-inert-in-place, which round
// 5 tried and round 6 correctly rejected as an unresolved contradiction —
// a field can't sit inside `CustomRules`/`LegalityRules`, structurally
// implying it's part of the resolved, engine-consumed ruleset, while a
// comment beside it insists it's inert metadata nobody reads. Those are
// incompatible claims about the same field's contract, not a documented
// nuance). It moves to `CustomFormatDef`'s metadata side, below — the
// resolved `CustomFormatRules` payload (this struct, embedded in
// `FormatConfig.custom_rules`, is what actually travels over the wire and
// gets enforced) now contains ONLY genuinely behavior-bearing fields.
LegalityRules {
    legal_sets: Option<Vec<SetCode>>,   // None = unrestricted; Some(list) = pool membership
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
// RUNTIME DISPLAY IDENTITY (round 9, maintainer review) — the gap: §7's
// claim below ("`label`, `for_format`, etc. each get a `Custom` arm reading
// the resolved def") is WRONG for an Axis-A save, and the two functions'
// signatures make it wrong in two DIFFERENT ways, not one:
//   - `GameFormat::label(self) -> &'static str` (crates/engine/src/types/
//     format.rs:395) cannot return owned, per-save `String` data (`label`
//     on `CustomFormatDef`, round 6) from a `&'static str`-returning
//     function no matter what's threaded in — this is a signature problem,
//     not just a missing-context problem.
//   - Even with the right signature, there is no "the resolved def" to read
//     for Axis A specifically: (a) above already establishes
//     `SavedCustomFormat.name` is client-local-only and never enters
//     `CustomFormatRules`/`FormatConfig.custom_rules`, so a peer holding a
//     live `GameState` mid-match has ONLY the rules payload — no name, no
//     lookup key back to the saving client's local storage. This is
//     different from Axis B: a registry-backed preset's `GameFormat::
//     Custom(id)` carries a STABLE id every peer's `custom_format_registry()`
//     already has an entry for (see (a)/(b) above), so a real name IS
//     resolvable there without transporting anything extra.
//
// RESOLUTION — axis-differentiated, no new wire surface (Matt's option (b),
// not (a) — deliberately, not by default): adding a `custom_label` field to
// `FormatConfig` would re-open the exact wire-compatibility/version-skew
// surface round 1's CodeRabbit pass and this section's VERSION SKEW note
// already treat as a real cost, for a purely cosmetic value. Axis A's own
// design already decided the engine "never learns about 'saved format #3'"
// (above) — extending that same boundary to the display NAME, not just the
// identity, is the consistent reading of that decision, not a new one:
//   - `GameFormat::label(self) -> Cow<'static, str>` (signature change,
//     REQUIRED regardless of axis, since `CustomFormatDef.label: String` can
//     never satisfy `&'static str`): built-in variants return
//     `Cow::Borrowed(...)` unchanged — zero behavior change for all 21
//     existing variants. `Custom(id)`: look up `custom_format_registry()` by
//     `id`; a HIT (Axis B) returns `Cow::Owned(def.label.clone())` — the
//     real preset name, genuinely resolvable, no transport gap. A MISS
//     (Axis A's ad-hoc sentinel id, never registered) returns
//     `Cow::Borrowed("Custom Format")` — a fixed, honest, generic runtime
//     label. The real player-chosen name stays exactly where round 2 always
//     said it lives: the local `SavedCustomFormat`/lobby picker, never
//     promised at runtime for an ad-hoc save. `short_label`/`description`
//     need NO equivalent fix — both are registry/picker-time-only data (§2's
//     `custom_format_registry()` → frontend picker), never read by a
//     runtime engine function the way `label`/`for_format` are; this gap is
//     specific to those two pre-existing bare-`GameFormat` functions, not a
//     property of display metadata in general.
//   - **Second, independently-found instance of the same root cause**:
//     `FormatConfig::for_format(format: GameFormat) -> Self`
//     (`format.rs:1056`) has exactly two real callers today, both in
//     `deck_validation.rs` (lines 1075, 2256 — confirmed by direct grep this
//     round, not assumed from the doc comment's claimed "lobby broker"
//     caller, which does not exist anywhere in the codebase today; flag
//     that stale doc comment for correction too), both reading
//     `.deck_size` off the returned default config. For `Custom`, `for_format`
//     has the identical problem as `label` did before round 3/4's
//     `sideboard_policy`/`uses_commander` fix: a bare id has no default
//     structural rules to reconstruct (Axis A has none at all; Axis B's are
//     real but `for_format`'s own doc comment already accepts "customizations
//     not recovered" for built-ins, so a registry-default lookup would be a
//     silently-approximate answer for a value — `deck_size` — that's supposed
//     to be exact for deck-legality gating). Fix identically to
//     `sideboard_policy`: `for_format` gains a `Custom(_) => unreachable!("for_format
//     cannot resolve an ad-hoc Custom format's deck_size — read
//     custom_rules.structural.deck_size from the resolved request/config
//     instead")` arm, and both `deck_validation.rs` call sites migrate to
//     prefer `request.custom_rules.as_ref().map(|r|
//     usize::from(r.structural.deck_size))` when present, falling back to
//     `for_format` only for genuine built-in formats — the exact same
//     two-tier shape §1's `sideboard_policy` fix already established for
//     `DeckCompatibilityRequest`, extended to cover `deck_size` (a site the
//     original round-4 audit's line list did not separately enumerate).
//
// VERSION SKEW — CONCRETE MODEL (round 4, maintainer review point 4; rounds
// 2-3 flagged this without designing it). An older client that has never
// heard of the `GameFormat::Custom` enum variant at all cannot be rescued by
// `#[serde(default)]` on `custom_rules` — the failure happens at the
// enum-variant level during deserialization of `format: GameFormat` itself,
// before any field-level default ever applies.
//
// This reuses EXISTING infrastructure rather than inventing a new
// negotiation protocol: `server-core/src/protocol.rs` already defines
// `PROTOCOL_VERSION` / `MIN_SUPPORTED_PROTOCOL` (currently
// `PROTOCOL_VERSION = 33`) AND a SEPARATE `LOBBY_PROTOCOL_VERSION` /
// `MIN_SUPPORTED_LOBBY_PROTOCOL` pair — its own doc comment states this is
// "the wire version of the lobby message set, independent of
// `PROTOCOL_VERSION`" (confirmed this session, not assumed). Both are
// checked at their respective handshakes with a "refuses to proceed on
// mismatch" doc comment.
//
// **Bump BOTH, not just `PROTOCOL_VERSION`** (caught by an automated review
// pass — a real refinement, not just style): format selection itself
// happens during LOBBY setup, before a game's `FormatConfig` ever crosses
// the general game-state wire, so `LOBBY_PROTOCOL_VERSION` /
// `MIN_SUPPORTED_LOBBY_PROTOCOL` is the handshake that actually needs to
// reject an incompatible client — an old broker/client could otherwise get
// past the lobby handshake and only fail later, at general `FormatConfig`
// deserialization, with a worse failure mode than a clean upfront rejection.
// Bump `PROTOCOL_VERSION` too, since a custom format's resolved
// `CustomFormatRules` payload does cross the general game-state wire once
// the game itself starts. The fix: bump both constants in the same release
// that ships `GameFormat::Custom`, using the existing lobby-handshake and
// general-handshake validation symbols exactly as they already work for
// every other breaking wire change — not a new, custom-format-specific
// negotiation layer.

// `ReprintPolicy` — round 5 resolved this to "documentation metadata, not
// consumed by the evaluator" but round 6 correctly caught that leaving the
// FIELD inside `LegalityRules` while saying so in a comment is an
// unresolved contradiction: the struct's own shape says "this is part of
// the resolved, engine-consumed ruleset," and the prose says the opposite.
// Round 6 picks the first of matthewevans's two offered resolutions
// (rather than the alternative — building real engine-owned printing
// enforcement and gating registration on it): the field MOVES to
// `CustomFormatDef` (display/authoring metadata), fully OUT of
// `CustomFormatRules`/`LegalityRules` (the resolved, wire-traveling,
// engine-consumed payload). See the `CustomFormatDef` struct later in this
// section for where it lives now. Its behavior is still fully absorbed by
// `legal_sets` curation (a reprint outside `legal_sets` is already excluded
// by plain set-membership) — the one thing it can't capture, frame/art-level
// fidelity, is Open item 2's gap, not this enum's job to close. Kept as a
// typed enum (not deleted, not a bool) purely so a preset's authored intent
// stays machine-readable for whoever eventually builds Open item 2.

// RESOLVED — round 11, maintainer review + direct follow-up discussion.
// **PARTIALLY SUPERSEDED — round 12 (see the `printing_fidelity` note near
// the `CustomFormatDef` struct later in this section for the current
// resolution).** Round 11's registration-gate conclusion below (no new
// blocking axis, `IMPLEMENTED_LEGACY_AXES`/mana burn only) still holds — no
// engine-owned printing predicate is being built, per the maintainer's own
// second offered option, exercised in round 12. What's retracted is the
// PREMISE that `legal_sets`-only enforcement is therefore not an
// approximation of anything: the maintainer's later review of this exact
// resolution correctly rejected that framing (an existing format with no
// documented printing requirement, Premodern included, isn't a
// rules-correctness argument for a new preset whose OWN source rule is
// stricter). Read this block for its still-accurate research on what the
// engine does and doesn't check; read round 12's note for what the
// proposal actually resolves to disclose about that gap.
// SUPERSEDES round 10's resolution below (retracted, not layered on top —
// round 11's review correctly rejected it): round 10 tried to answer
// CONTEXT.md open item #2 by pairing "legal_sets is the whole legality
// check" with a display-only fix (`ArtChainEntry` becoming legal-set-aware)
// framed as resolving the source rules' printing-fidelity requirement.
// matthewevans's round-11 review correctly rejected this: "an optional
// rendering preference [is not] a legality resolution" — a cosmetic default
// cannot satisfy a rules-correctness claim, and the rollout still registered
// Old School 93-94/95 without addressing that the schema literally cannot
// express their source rule's printing/foil requirement.
//
// The actual resolution, reached in direct discussion rather than picked
// unilaterally, is simpler than either round 10's or the two options
// matthewevans offered (gate on future enforcement, or explicitly downgrade
// the claim) — it's a THIRD answer, grounded in existing precedent rather
// than a new policy call:
//
// **`legal_sets` membership is not an approximation of Old School 93-94/95's
// legality — it is the exact same legality model every format in this
// engine already uses, built-in or custom, with zero exceptions.**
// Confirmed directly against the actual source this round:
// `GameFormat::Premodern => Some(LegalityFormat::Premodern)`
// (`crates/engine/src/types/format.rs:243`) is an oracle-card-level check —
// "does this card name have a legal printing," full stop. No built-in
// format (Premodern included, and Standard/Modern/Legacy/etc. via the same
// `LegalityFormat` mechanism) has EVER checked which specific printing,
// frame, border, or foil status a card uses. `PrintedCardRef { oracle_id,
// face_name }` (`crates/engine/src/types/card.rs:89-92`) — the actual
// runtime card identity used everywhere a legal deck is played — has no
// set-code field at all, for any format. So "legal_sets membership only" is
// not old-school-specific caveated exposure; it is this engine's ONE
// existing, consistent legality model, applied identically to a fourth
// (fifth, sixth...) format the same way it's already applied to every one
// that shipped before this proposal. There is no inconsistency to gate
// against, because there is no format anywhere in this engine that does
// more than this.
//
// This means: **the registration gate for Old School 93-94/95 reverts to
// exactly what §8 had before round 10 — keyed to
// `IMPLEMENTED_LEGACY_AXES`/mana burn only, no new blocking axis.** Nothing
// about their legality model needs to wait on unscoped future work, because
// nothing about it falls short of the standard every other format in this
// engine already meets.
//
// **The display-fix material from round 10 is retracted from this
// proposal, not merged with this resolution — confirmed directly, not
// this document's unilateral call.** Making a per-player display
// preference legal-set-aware is a real, worthwhile idea, but it belongs in
// its own proposal (a general frontend/`ArtChainEntry` enhancement, not
// scoped to or gated by this custom-format-engine work) rather than being
// used to paper over a legality question this resolution no longer needs
// papered over. Tracked as a follow-up idea, not designed further here —
// see CONTEXT.md's open items.
//
// **Genuine per-card printing selection (a player declaring which specific
// printing/set — not frame/foil, which stays purely cosmetic even on
// platforms that track it, e.g. Arena/MTGO's foil is a rendering/collection
// attribute, not a format-legality one) is a separate, real, larger future
// feature, also NOT designed or scoped here.** Briefly surveyed this round
// for awareness, not design: the deck-builder already has the needed UI
// shape (`PrintingPickerModal.tsx`, `client/src/components/deck-builder/`)
// and a client-side type that already carries printing detail on import
// (`DeckEntry.sourcePrinting`, `client/src/services/deckParser.ts:5-9`) —
// but that detail is explicitly discarded at the `expandParsedDeck`
// boundary (`deckParser.ts:45-73`) before it ever reaches
// `PlayerDeckList`/`PrintedCardRef` (which have no printing field to receive
// it). Building this for real would be a moderate plumbing lift reusing
// existing components, not a from-scratch feature — but it is general to
// EVERY format (nothing about it is old-school-specific) and is explicitly
// out of scope for this proposal, which changes nothing about how decklists
// identify cards.
//
// `ReprintPolicy`'s three variants (`AllowSpecialReprintSets`,
// `AllowAnyPrinting`, `OriginalPrintingsOnly`, §2) are UNCHANGED by this
// resolution — still exactly what round 6/7 resolved them to be:
// authoring-intent documentation metadata on `CustomFormatDef`, consumed by
// nothing, kept machine-readable for whichever of the two future features
// above eventually gets built.

// REVISED per maintainer review round 4 (CONTEXT.md point 3): every axis
// below is now a typed enum, not a bool — `legend_rule_scope` already was;
// the other three are converted for consistency and because CLAUDE.md's
// own bool-vs-enum principle applies here: each names a REAL two-value
// historical space (a pre-removal form vs. the modern absence of the rule),
// not an arbitrary on/off switch, and a typed variant is self-documenting at
// every call site (`ManaBurnPolicy::Obsolete` reads correctly; `mana_burn:
// true` requires the reader to already know which direction "true" points).
// This mirrors `LegendRuleScope`'s existing shape rather than introducing a
// new convention.
LegacyRuleSet {                         // INDEPENDENT era-rule axes (RESEARCH §8, §10)
    mana_burn: ManaBurnPolicy,
    damage_timing: CombatDamageTiming,
    wish_scope: WishOutsideGameScope,
    legend_rule_scope: LegendRuleScope, // RESEARCH §10: modern per-controller
                                        // (default) vs pre-M14 any-controller.
                                        // Already a typed enum since round 1 —
                                        // the historical space is not a clean
                                        // binary and this leaves room without
                                        // a later refactor. Same reasoning now
                                        // applied to the three siblings above.
}

ManaBurnPolicy {                        // RESEARCH §5
    Modern,                             // no mana burn (removed post-M10). DEFAULT.
    Obsolete,                            // life loss for unspent mana at real
                                        // phase-group boundaries (§4's ManaExpiry
                                        // design). EC/Swedish target era.
}

CombatDamageTiming {                    // RESEARCH §6
    Modern,                             // CR 510.2: simultaneous, does not use
                                        // the stack. DEFAULT.
    OnStack,                            // pre-modern (pre-6th-edition): assigned
                                        // combat damage was itself placed ON
                                        // THE STACK as a stack object — NOT a
                                        // triggered ability — giving players a
                                        // priority window between assignment
                                        // and dealing before it resolved. A
                                        // reversal of CR 510.2's control flow.
                                        // LARGE — see §4 and §8.
}

WishOutsideGameScope {                  // RESEARCH §9
    PostM10SideboardOnly,               // modern CR 400.11/400.11a: "outside
                                        // the game" is not a zone; only the
                                        // sideboard is reachable. DEFAULT.
    PreM10ReachesExile,                 // pre-M10: a Wish could retrieve an
                                        // owned card that had been removed
                                        // from the game (today's exile).
                                        // Renamed from the first-pass
                                        // placeholder name during round 1;
                                        // canonical everywhere in this
                                        // proposal, including RESEARCH.md
                                        // (fixed this round — two stale
                                        // references to the old name had
                                        // survived there since round 1).
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
banned/restricted, default `LegacyRuleSet`) and `reprint_policy: None`
(maintainer review round 7 — a lobby save has no authored paper-format
reprint intent to declare; see the `CustomFormatDef` struct above for the
full reasoning):

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

**`label`/`short_label`/`description` construction rule (maintainer review
round 8) — the one gap this constructor still had.** Round 6 gave
`CustomFormatDef` a real struct sketch requiring these three fields (see
above); this constructor's signature (`name` + `&FormatConfig`) never
specified how a lobby save produces `short_label` or `description`, and
those two aren't optional — the registry hands them to the frontend exactly
like `FormatMetadata` already does for built-in formats
(`crates/engine/src/types/format.rs:24-34`), so an unspecified derivation
means the Axis-A factory has no defined complete output. Fixed by deriving
both from `name`/`config` alone — never inventing user-facing rules
metadata a lobby host didn't provide:
- `label: name` directly — unchanged from what was already implied.
- `short_label`: the first 3 alphanumeric characters of `name.trim()`,
  uppercased. This is not a new UI convention — it's the existing
  `FormatMetadata.short_label` contract (a fixed 3-character code,
  load-bearing for the badge width in `GameListItem.tsx`) applied via the
  *same* derivation the frontend already falls back to today for any
  format it doesn't recognize (`format.slice(0, 3).toUpperCase()`, present
  independently in `GameListItem.tsx`, `StatsPanel.tsx`, and
  `CardCoverageDashboard.tsx`). Moving that derivation into
  `from_lobby_config` means the engine now supplies a real value for every
  Axis-A format instead of the frontend silently computing one — per
  CLAUDE.md's "the engine owns all logic," this was arguably a latent
  frontend-computes-derived-data gap even before Axis A existed, now closed
  as a side effect of fixing this finding. Edge case, explicitly allowed:
  a name with fewer than 3 alphanumeric characters yields a shorter code
  (there is no meaningful 3-character abbreviation to invent for e.g. a
  2-character name) — this is a deliberate, documented deviation from the
  "always exactly 3" convention every hand-curated built-in entry happens
  to satisfy, not a silent violation of it. `from_lobby_config` rejects an
  empty `name.trim()` outright (no format to label at all), consistent with
  the existing "reject explicitly rather than silently drop data" posture
  from round 2 above.
- `description`: generated from `StructuralRules` alone via a new
  `derive_structural_description(&StructuralRules) -> String` helper —
  Matt's suggested "engine-provided structural description" option. Mirrors
  the existing built-in phrasing style (`"100-card singleton, 2–4 players"`
  for Commander, `"Tournament 1v1 Commander, 30 life"` for DuelCommander —
  both short comma-joined structural fragments, no terminal punctuation)
  by composing from the same fields those hand-authored strings already
  describe: deck size + singleton, player count/range, starting life, and
  `command_zone`/`team_based` when set. E.g. `deck_size: 60, singleton:
  false, min_players: max_players: 2, starting_life: 20` → `"60-card,
  2-player, 20 life"`. This is Axis A only — Axis B presets keep their
  existing hand-authored descriptions unchanged; `derive_structural_description`
  exists because Axis A has no human curator to write one. Exact wording
  is an implementation-time polish detail (the shape — which fields
  contribute and in what order — is the part this design commits to), not
  an architectural question this proposal needs to settle further.

**`sideboard_policy`'s specific source, made explicit (maintainer review
round 4, point 2 — round 3 added the field but never stated this)**:
`structural.sideboard_policy: config.format.sideboard_policy()`. This is a
valid, direct call because `from_lobby_config`'s precondition is that
`config.format` is always a BUILT-IN `GameFormat` at the moment this runs —
saving a custom format FROM an already-custom format (re-saving a save) is
out of scope for this design and not offered by the lobby UI; if that need
ever arises it requires its own resolution path (reading the source's
already-resolved `FormatConfig.sideboard_policy` field instead of calling
the method), not assumed to fall out of this constructor for free.

Both paths converge on the same `custom_rules: Option<CustomFormatRules>` field
and the same `GameFormat::Custom(CustomFormatId)` variant — there is exactly
one runtime representation of "a custom format," authored two different ways.
See §7 for where each one is surfaced to a player.

**`CustomFormatDef` — given an actual struct sketch this round (round 6);
previously only described in prose as "display metadata + CustomFormatRules,"
which is exactly the ambiguity that let `reprint_policy` end up in the wrong
place):**

```text
CustomFormatDef {
    rules: CustomFormatRules,           // the resolved, engine-consumed, wire-traveling payload
                                        // (§1 above) — genuinely behavior-bearing fields ONLY
    label: String,                      // "Swedish Old School 93/94", parallel to FormatMetadata
    short_label: String,
    description: String,
    reprint_policy: Option<ReprintPolicy>,  // REVISED per maintainer review
                                        // round 7: MUST be Option, not a bare
                                        // ReprintPolicy — round 6 moved the
                                        // field but missed that an Axis-A
                                        // lobby save has no authored paper-
                                        // format reprint intent at all (it
                                        // isn't modeling any published
                                        // ruleset), so none of the three real
                                        // variants would be honest for it.
                                        // `None` = "no reprint intent to
                                        // declare" (every Axis A lobby save);
                                        // `Some(policy)` = a real, sourced
                                        // paper-format intent (every Axis B
                                        // preset — old_school_93_94(),
                                        // old_school_95(), middle_school(),
                                        // classic_magic(), swedish_old_school()
                                        // all set this explicitly; see §2).
                                        // Same `Option<T>`-over-forcing-a-value
                                        // pattern this proposal already uses
                                        // for `legal_sets` (§1) and
                                        // `range_of_influence` — not a new
                                        // convention.
    printing_fidelity: PrintingFidelity,    // NEW — round 12, maintainer
                                        // review (repeated rejection of the
                                        // round-11 "same model as Premodern"
                                        // framing — see §1's round-12 note
                                        // below for the full argument).
                                        // Required on every `CustomFormatDef`,
                                        // no default: whichever variant
                                        // applies must be an explicit
                                        // authoring decision, exactly like
                                        // `reprint_policy` above.
}

enum PrintingFidelity {
    /// This preset's `reprint_policy` is `None` — no source paper-format
    /// reprint intent is declared (every Axis A lobby save; `swedish_old_school()`
    /// until CONTEXT.md Open item 6 resolves its own `reprint_policy` value).
    /// There is nothing to approximate because nothing is being claimed.
    NotApplicable,
    /// This preset's `reprint_policy` is `Some(_)` — a real, sourced paper-format
    /// reprint intent IS declared, but the engine enforces it only at set-code
    /// membership (`legal_sets`, §3) — the same granularity every format in this
    /// engine, built-in or custom, has ever enforced (round 11's finding, still
    /// true). It does NOT enforce printing/frame/border/foil, which some source
    /// rulesets (Old School 93-94/95 among them, RESEARCH.md §1) state as a real
    /// requirement. `description` MUST disclose this in player-facing text (§6's
    /// preset-integrity test enforces the pairing, not just the field's presence).
    SetCodeApproximation,
}
```

**Why this field exists — round 12, maintainer review (supersedes round 11's
framing below, which is kept for its still-accurate research, not as the
resolution):** round 11 argued that `legal_sets` membership isn't an
approximation of Old School 93-94/95's legality at all, because no format in
this engine — Premodern included — has ever enforced more than set-code
membership. The maintainer's review of that exact resolution correctly
rejected it on a second look: **an existing format with no documented
printing-level requirement is not a rules-correctness argument for a NEW
preset whose OWN documented source rule is stricter.** `Premodern` never
claimed frame/art fidelity in the first place, so it enforcing only set codes
is not an approximation of anything — nothing is missing relative to its own
source. Old School 93-94/95's own cited source (RESEARCH.md §1) explicitly
requires non-foil reprints with original frame and art; `legal_sets`
membership genuinely falls short of that specific, stated requirement, and
round 11's "no format does more" observation, while accurate, doesn't change
what this format's own source rule says.

**The product decision (round 12, maintainer's second offered option, taken
deliberately over the first):** do not build engine-owned printing/frame
enforcement — CONTEXT.md's Open item 2 survey already scoped that as a real,
separate, moderate-lift feature (the engine and frontend printing systems are
disconnected today; wiring them is a genuine, general, future project, not
specific to Old School). Instead, accept the set-code-only approximation
explicitly, and require every preset that makes it to disclose it in its own
player-facing `description` rather than only in a developer-facing doc
comment. The reasoning this proposal rests that acceptance on, distinct from
round 11's now-superseded analogy: **in a digital-only client, a printing's
frame, border, and foil status have no gameplay consequence at all** — a
card with identical Oracle text is identical for every rules purpose the
engine cares about, regardless of which physical printing it "represents."
The frame/art requirement exists in the paper community's own ruleset for a
reason that has no digital equivalent — provenance and anti-counterfeiting
at a physical table — not because the alternate-frame printing plays
differently. Declining to model a purely cosmetic, paper-specific concern is
a legitimate product scope decision; silently presenting the approximation
as full fidelity is not, which is why `printing_fidelity` and its
disclosure requirement exist as a typed, tested construction-time
requirement rather than a comment. This directly satisfies the maintainer's
second offered resolution ("explicitly scope the presets as an oracle-card/
set-code approximation") without the first (build the predicate) — the
`PrintingFidelity::SetCodeApproximation` label on every affected preset IS
that explicit scoping, made structural rather than a documented convention
this proposal's own §7 principle would otherwise reject.

The registry (`custom_format_registry() -> Vec<CustomFormatDef>`) hands the
frontend labels/short-labels/descriptions just like `FormatMetadata` already
does for built-in formats — unchanged from the original design. What changed
is only where `reprint_policy` sits: previously a field on `LegalityRules`
(implying it travels with and is enforced by the resolved ruleset); now a
field on `CustomFormatDef` alongside the other purely-descriptive fields,
never serialized into `FormatConfig.custom_rules` at all. `printing_fidelity`
sits beside it for the same reason (§1's round-12 note above): both are
authoring-time, player-facing disclosures about what a preset's `rules`
payload does and does not enforce, never part of the wire-traveling,
engine-consumed struct.

## 2. Parameterizing the formats as data (not N blocks)

**Every card list, count, and rule below is sourced, not invented — see
RESEARCH.md §1 for the full per-format citation, repeated here inline so this
section stands on its own for anyone auditing the actual preset data without
cross-referencing another file:**
- Swedish Old School: `http://oldschool-mtg.blogspot.com/p/banrestriction.html` (RESEARCH.md §1, "Swedish Old School 93/94" subsection).
- The four EC formats: `raw.githubusercontent.com/northern-information/lordsofthepit.com/main/src/pages/formats.md` (RESEARCH.md §1, EC ruleset header).

**Phase 1 preset — `swedish_old_school()` (see CONTEXT.md's "Further narrowing
Axis B's MVP").** This is the first Axis B preset targeted, chosen because it
needs zero `LegacyRuleSet` engine wiring. **Registration is currently blocked
by CONTEXT.md Open item 6** — its `ReprintPolicy` metadata VALUE is
unconfirmed against the primary source (not an enforcement gap — see §3's
resolution: `ReprintPolicy` is documentation, not a gated axis) — so the
engine/schema work below can and should land in phase 1, but this preset does
not appear as a selectable format until that item resolves:

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
  reversion). `reprint_policy` (on `CustomFormatDef`): `None` for now — NOT
  a value pending confirmation, but the genuinely correct value until
  CONTEXT.md Open item 6 resolves (there is no confirmed authored intent to
  declare yet; `None` says exactly that, distinctly from a lobby save's
  permanent `None`, which has no intent to declare, period). `printing_fidelity`
  (NEW — round 12): `NotApplicable`, following directly from `reprint_policy:
  None` per §1's pairing rule — revisit alongside Open item 6, since resolving
  that item to a real `Some(_)` value also flips this to
  `SetCodeApproximation` with a matching `description` update. Ante-card
  handling remains open per Open item 5 separately; do not encode either
  without resolving its own item first.

**Phase 2 presets — the four EC formats**, unchanged from the original design,
now explicitly sequenced after phase 1 (§8) since they need the
`LegacyRuleSet` wiring in §4:

The four formats form an incremental chain. Express it with builder-style reuse,
mirroring how `FormatConfig::pioneer()` spreads `..Self::standard()`:

- `old_school_93_94()` — `rules`: sets = [LEA, LEB, 2ED, CED, CEI, ARN, ATQ,
  3ED, LEG, DRK, FEM]; restricted = [22 names]; banned = [7 names]; legacy =
  `{ mana_burn: ManaBurnPolicy::Obsolete, ..default }` (damage timing and
  Wish scope stay `Modern`/`PostM10SideboardOnly` — EC's 93-94 lists mana
  burn as its only legacy exception). `reprint_policy` (on `CustomFormatDef`):
  `Some(AllowSpecialReprintSets)` — RESEARCH.md's §1 subsection for this
  format explicitly includes CE/ICE as legal reprint sources within
  `legal_sets`. `printing_fidelity` (NEW — round 12): `SetCodeApproximation`
  — this format's source rule requires non-foil original-frame/art reprints
  (RESEARCH.md §1) and the engine enforces only set-code membership (§1's
  round-12 note); `description` MUST state this (e.g. "Legality is approximated
  at the set-code level; original-printing frame/foil is not enforced.").
- `old_school_95()` — `let mut d = old_school_93_94();
  d.rules.legality.legal_sets.get_or_insert_with(Vec::new).extend([4ED, ICE,
  CHR, REN, HML]); d.rules.legality.restricted.extend([Demonic Consultation,
  Mana Crypt]); d.rules.legality.banned.extend([Amulet of Quoz, Timmerian
  Fiends]); legacy unchanged`. **Fixed this round (maintainer review): two
  independent bugs in the earlier sketch, both against §1's own declared
  schema —**
  1. **Nesting.** `legal_sets`/`restricted`/`banned` are not fields on
     `CustomFormatDef` (`d`) at all — they live at `d.rules.legality.*`
     (`CustomFormatDef.rules: CustomFormatRules`, `CustomFormatRules.legality:
     LegalityRules`, §1). The prior sketch accessed `d.legal_sets` etc.
     directly, which does not compile against the struct this proposal itself
     defines. Every mutation now composes through the real path.
  2. **`Option` handling.** `legal_sets` is `Option<Vec<SetCode>>` (§1), not a
     bare `Vec` — `.extend()` cannot be called on it directly.
     `get_or_insert_with(Vec::new)` extends the base's `Some(...)` vector in
     place (its only legal state per `old_school_93_94()`'s own construction
     — a restricted-pool preset never leaves this `None`) while still being
     correct if a future refactor ever changed that invariant, rather than
     assuming it silently.

  `printing_fidelity`: inherited `SetCodeApproximation` from the base `d` —
  same source requirement, same enforcement gap. **New preset-inheritance
  test (§6):** asserts `old_school_95()`'s resolved `rules.legality` contains
  every `old_school_93_94()` set/restricted/banned entry PLUS exactly its own
  five/two/two declared additions — not just that the extra entries are
  present, so a future edit can't silently drop or duplicate the inherited
  93-94 base while adding 95's deltas.
- `middle_school()` — `rules`: sets = Fourth Edition..Scourge; restricted =
  []; banned = [25 names]; legacy = `{ mana_burn: ManaBurnPolicy::Obsolete,
  damage_timing: CombatDamageTiming::OnStack, wish_scope:
  WishOutsideGameScope::PreM10ReachesExile }`. `reprint_policy` (on the
  `CustomFormatDef`, not `rules` — round 6): `Some(AllowAnyPrinting)`.
  `printing_fidelity` (NEW — round 12): `SetCodeApproximation`, same
  disclosure requirement as Old School above. **Per the
  preset-readiness gate (§7, tightened this round): this preset may not be
  registered until `CombatDamageTiming::OnStack` (§4/§6, LARGE) is fully
  implemented — no partial/caveated exposure.**
- `classic_magic()` — `rules`: sets = Alpha..Scourge; restricted = [**44**
  names — corrected this round from a "37" mislabel; recounted directly
  against RESEARCH.md's verbatim list]; banned = [11 names]; legacy =
  `{ mana_burn: ManaBurnPolicy::Obsolete, damage_timing:
  CombatDamageTiming::OnStack, wish_scope:
  WishOutsideGameScope::PreM10ReachesExile }`. `reprint_policy` (on the
  `CustomFormatDef`): `Some(OriginalPrintingsOnly)`. `printing_fidelity`
  (NEW — round 12): `SetCodeApproximation`, same disclosure requirement.
  **Same registration block
  as Middle School** — not selectable until `CombatDamageTiming::OnStack` lands.

Set codes must be verified against the engine's `set_catalog` (MTGJSON codes)
during implementation — the codes above are the expected MTGJSON codes but
must be confirmed, not assumed. Card names are validated at preset-construction
time by a unit test that asserts every banned/restricted name resolves in the
`CardDatabase` (guards against typos silently no-op'ing a ban).

## 3. Deck-legality algorithm (engine, data-driven)

`evaluate_custom_format(db, request, rules) -> CompatibilityCheck`:

1. Structural checks (deck size, sideboard) via existing `FormatConfig` fields
   — parameterized by `rules.structural.deck_size` (not a hardcoded 60, per
   maintainer review round 5's finding below).
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
5. Copy limit: `copy_limit_violations(db, &counts, if rules.structural.singleton
   { 1 } else { 4 })` — **FIXED per maintainer review round 5** (see below);
   the field existed on `StructuralRules` since round 2 but step 5 never read
   it, always calling the helper with a hardcoded `4`. `copy_limit_violations`
   ALREADY takes this exact parameter — every built-in singleton format
   (Commander, Brawl, etc.) already calls it with `1`
   (`deck_validation.rs:929,1096,1335,1668,2215`, confirmed this round, not
   assumed) and every non-singleton format with `4`
   (`:447,535,643,2054`) — this is parameterizing an existing call, not new
   logic. Card-intrinsic overrides (`DeckCopyLimit::UpTo(n)`, "any number"
   cards like Relentless Rats/Nazgûl) are already handled generically inside
   `copy_limit_violations` regardless of the format-level limit passed in —
   confirmed via its own existing tests
   (`deck_validation.rs:3916-3941`, e.g. `Nazgûl` with limit `1` still passes
   at 9 copies) — no new "preserve overrides" logic needed, the helper
   already composes correctly.

This reuses four existing helpers verbatim and adds only the set-membership +
name-set sourcing, now correctly parameterized by every `StructuralRules`
field the algorithm needs (`deck_size`, `singleton`) rather than assuming
built-in-format defaults. `GameFormat::Custom` gets one arm in
`format_compatibility_check` routing to `evaluate_custom_format`.

**Maintainer review round 5 finding, addressed above**: this whole algorithm
requires the VALIDATED `FormatConfig`/resolved `CustomFormatRules` as input —
confirmed again that `DeckCompatibilityRequest.selected_format:
Option<GameFormat>` (§1's audit) cannot supply `deck_size`/`singleton` any
more than it could supply `legal_sets`/`banned`/`restricted` — the same
signature-widening fix from §1 covers this too, not a separate mechanism.

**`ReprintPolicy` is metadata on `CustomFormatDef`, not a field on
`CustomFormatRules`/`LegalityRules` at all (RESOLVED — maintainer review
round 6; supersedes round 5's "leave it in place, document it as inert"
attempt, which round 6 correctly rejected as an internal contradiction — a
field can't structurally sit inside the resolved, engine-consumed ruleset
while a comment insists nothing consumes it).**

History: round 4's registry gate (`IMPLEMENTED_LEGACY_AXES`, §7) covered only
`LegacyRuleSet`'s four axes and was never extended to `ReprintPolicy`; round
5 tried to resolve this by declaring `reprint_policy` non-behavior-bearing
via a comment while leaving the field inside `LegalityRules` — round 6
correctly caught that this is two incompatible claims about the same field
(its position says "resolved ruleset," its comment says "never read"). The
actual fix is structural, not documentary: `reprint_policy` MOVED to
`CustomFormatDef` (§1's struct sketch) — the display/authoring-metadata side
that already exists for `label`/`short_label`/`description` — fully out of
`CustomFormatRules`, which now contains only genuinely behavior-bearing
fields. This is one of the two resolutions matthewevans offered (move it to
metadata outside `CustomFormatRules`/`LegalityRules`) rather than the other
(build real engine-owned printing enforcement and gate registration on it).

This document's OWN original research already explains why the metadata-only
answer is correct, not just convenient (RESEARCH.md §3, written before any
review round): "model reprint policy as *which set codes are in the legal
list*, and flag frame/art-level fidelity as a known limitation." That is:
`ReprintPolicy`'s actual behavior is already fully absorbed into `legal_sets`
curation (a modern reprint of an old card lives in a set code simply not
present in `legal_sets`, so it's excluded by step 2 alone) — the ONLY thing
it can't capture is the finer frame/art-level distinction, which is Open
item 2's gap, not this field's job. `swedish_old_school()` is still gated on
CONTEXT.md Open item 6 (getting the metadata VALUE right, since an
inaccurate label could mislead a future maintainer), but that's now cleanly
a documentation-accuracy blocker on a metadata field — not a
missing-enforcement gap on a rules field, which is exactly the contradiction
round 6 caught.

`legality_format()` returns `None` for `Custom` (no `LegalityFormat` mapping —
custom formats don't use the external legality table). **REVISED — round 9,
maintainer review; the previous text here ("`label`, `for_format`, etc. each
get a `Custom` arm reading the resolved def") was wrong and is retracted, not
refined — see §1's "Runtime display identity" for why "the resolved def" does
not exist to read at runtime for an Axis-A save.** `sideboard_policy` and
`uses_commander` are NOT "add a `Custom` arm on the `GameFormat` method" —
the correct arm lives on `FormatConfig::sideboard_policy()`, not
`GameFormat::sideboard_policy()`, since the latter has no access to
`custom_rules` (§1). `label` and `for_format` follow the same "the bare enum
cannot resolve this" shape, generalized: `label` becomes
`Cow<'static, str>`-returning with a registry-lookup-or-generic-fallback
`Custom` arm (§1), and `for_format` gets the same `unreachable!()` guard
`sideboard_policy`/`uses_commander` already use, with its two real callers
migrated to read `custom_rules.structural.deck_size` directly (§1). No
`GameFormat` method gains a `Custom` arm that "reads the resolved def" — none
of them have one to read; every fix here is either a registry lookup keyed
by a stable id (Axis B only) or a caller migration to a type that actually
carries `custom_rules` (both axes).

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
  - **Life loss, not damage** (unchanged from round 2, still correct): the
    obsolete-rules glossary entry "Mana Burn (Obsolete)": "unspent mana
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
  obsolete-glossary entry "Mana Burn (Obsolete)"). Slightly larger than round
  2's estimate (a new `ManaExpiry` variant + its construction-site and
  clearing-logic wiring, not just a life-loss check at an existing call site)
  but still small and, critically, reuses a proven pattern rather than adding
  a new one.
- **Pre-M10 Wish exile access** (`wish_scope: WishOutsideGameScope`, renamed
  from the placeholder `pre_m10_wish_reaches_exile` bool during round 1 —
  same field, now typed): fully traced in RESEARCH §9. This is a REAL
  functional difference (not wording-only): pre-M10, the "removed from the
  game" zone counted as *outside the game*, so a Wish could retrieve an
  owned card that had been removed from the game (modern: exile); the M10
  update (CR 400.11/400.11a — "outside the game is not a zone"; only the
  sideboard is outside the game) removed this. The engine already
  implements the modern (post-M10) Wish cycle generically as
  `Effect::SearchOutsideGame` with `source_pool:
  OutsideGameSourcePool::Sideboard` (`types/ability.rs:246`), and already
  implements owned-face-up-exile retrieval for the Karn/Coax class via
  `OutsideGameSourcePool::SideboardAndFaceUpExile` + the tested
  `collect_face_up_exile_candidates` collector and `put_face_up_exile_into`
  mover (`game/effects/search_outside_game.rs:72,105,141`). **SMALL**
  (revise the prior "Medium"): the only change is a one-line pool-widening
  at the existing resolver hook (`search_outside_game.rs:72`) — when
  `wish_scope == PreM10ReachesExile` on `GameState.format_config`
  (`types/game_state.rs:6787`), treat a `Sideboard`-pool search as if it
  were `SideboardAndFaceUpExile`. No parser change (this is a
  runtime-resolution concern, not a parse concern), no new
  effect/state/WaitingFor, full reuse of the tested collector/mover.
  Annotate as a legacy rule reverting the M10 change (cite CR 400.11 /
  400.11a / 701.23j).
- **Combat damage timing** (`damage_timing: CombatDamageTiming`, renamed
  from `damage_uses_stack: bool` this round): LARGE (RESEARCH §6). Its own
  sub-project. **Per the tightened preset-readiness gate (§7, maintainer
  review round 4, point 3): no preset may register while declaring
  `CombatDamageTiming::OnStack` until this fully lands — there is no
  "playable with a caveat" tier.** This is a real, meaningful timeline
  consequence: Middle School and Classic Magic (both require `OnStack`) are
  blocked from ever being selectable until this LARGE item ships, not
  available-with-a-caveat in the interim as the original sequencing implied.
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

- **`reprint_policy` has no effect on evaluation** (new — maintainer review
  round 6): two synthetic `CustomFormatDef` values with identical `rules`
  (`CustomFormatRules`) but different `reprint_policy` metadata must produce
  IDENTICAL `evaluate_custom_format` results — proving the field is truly
  inert for legality purposes, not just absent from the struct by
  convention. Since `reprint_policy` isn't a field on `CustomFormatRules` at
  all after the round-6 move, this is also implicitly a compile-time
  guarantee (`evaluate_custom_format` takes `&CustomFormatRules`, which has
  no such field to accidentally read) — the runtime test exists to catch a
  future regression that reintroduces the field in the wrong place.
- **`reprint_policy` is `Option`, and every constructor sets the correct arm**
  (new — maintainer review round 7): `CustomFormatDef::from_lobby_config`
  always produces `reprint_policy: None` — assert this directly, not just
  that construction succeeds, so a future edit can't silently start
  fabricating a value for an Axis A save. Each of the five Axis B
  constructors (`old_school_93_94`, `old_school_95`, `middle_school`,
  `classic_magic`, and `swedish_old_school` once Open item 6 resolves)
  produces `Some(_)` with its own specific documented variant — five
  separate assertions, not one shared "is `Some`" check, so a copy-paste
  preset that forgets to set its own value is caught.
- **`printing_fidelity` is paired with `reprint_policy`, and disclosed in
  `description`, for every registered preset** (new — maintainer review
  round 12, §1's new field): for every `CustomFormatDef` `custom_format_registry()`
  would return, assert `def.reprint_policy.is_some() == matches!(def.printing_fidelity,
  PrintingFidelity::SetCodeApproximation)` — the two fields must never
  disagree (a `Some(_)` reprint intent with `NotApplicable` fidelity would
  silently claim full fidelity; a `None` reprint intent with
  `SetCodeApproximation` would disclose a limitation for a format that never
  claimed the stricter requirement in the first place). For every preset
  whose `printing_fidelity` is `SetCodeApproximation` (today: `old_school_93_94`,
  `old_school_95`, `middle_school`, `classic_magic`), additionally assert
  `description` contains a substring disclosing the set-code-only
  approximation — not merely that the enum variant is set, so a future
  preset can't satisfy the pairing check while leaving the player-facing
  text silent about it. This is the technical mechanism the round-12 note
  in §1 promises — the disclosure is enforced, not a documented convention.
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
- **Singleton copy limit is actually parameterized, not defaulted** (new —
  maintainer review round 5): a synthetic `CustomFormatRules` with
  `structural.singleton: true` flags 2 copies of an arbitrary non-basic,
  non-"any number" card and accepts 1 — proving step 5 reads
  `structural.singleton` rather than always calling
  `copy_limit_violations(..., 4)`. Paired with a `singleton: false` case
  accepting up to 4, so both branches of the parameter are covered, not just
  the new one. A third case reuses an existing card-intrinsic override
  (Relentless Rats-shaped "any number" card, or a `DeckCopyLimit::UpTo(n)`
  card) under `singleton: true` and confirms the override still applies —
  proving `copy_limit_violations` composes the format limit and the
  card-intrinsic override correctly for Custom exactly as it already does
  for built-in singleton formats (Commander etc.), not a new interaction to
  design.
- Preset integrity: every banned/restricted name in all four EC presets plus
  `swedish_old_school()` resolves in the DB; each preset's `legacy` matches
  its source ruleset; each preset's stated list *count* in a doc comment
  matches `.len()` of the actual list (guards against the round-2 23-vs-25 /
  37-vs-44 class of mislabeling recurring silently).
- **`old_school_95()` correctly inherits `old_school_93_94()`'s base and adds
  only its own delta** (new — maintainer review round 12): assert
  `old_school_95().rules.legality.legal_sets` is `Some(_)` containing every
  set in `old_school_93_94().rules.legality.legal_sets` PLUS exactly the
  five declared additions (4ED, ICE, CHR, REN, HML) — not a superset check
  alone, an exact-set comparison, so a future edit can't silently drop an
  inherited entry while still passing a "contains my new sets" assertion.
  Mirrored for `restricted` (93-94's list plus exactly Demonic Consultation
  and Mana Crypt) and `banned` (93-94's list plus exactly Amulet of Quoz and
  Timmerian Fiends). This is also where a `d.rules.legality.*`-vs-`d.*`
  nesting regression (§2's round-12 fix) would be caught mechanically,
  rather than only by inspection.
- **Mana burn — pool persistence AND life loss** (revised again — maintainer
  review round 3, CONTEXT.md point 1; round 2's version only tested the
  life-loss gate and would have passed against the wrong mechanism):
  - With `mana_burn: ManaBurnPolicy::Obsolete`, unspent mana added during
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
- **Fallible `custom_rules` validation, not `.expect()`** (new — maintainer
  review round 4, CONTEXT.md point 1; supersedes round 3's accessor-only
  test): a `FormatConfig` with `format: GameFormat::Custom(id)` and
  `custom_rules: None` must be REJECTED by
  `validate_custom_rules_consistency` at construction/ingestion, not
  accepted and later panic wherever `sideboard_policy`/`uses_commander` is
  read. Same for `custom_rules: Some(rules)` where `rules.id != id`. Both
  are constructed-then-rejected cases, proving the invariant is enforced at
  the boundary, not merely documented.
- **Every real consumer reads the resolved `FormatConfig` field, not the
  bare-`GameFormat` method** (new — maintainer review round 4, expanding
  round 3's two-call-site test to the full audited list): for a `Custom`
  format with `sideboard_policy: Forbidden`, assert `deck_loading.rs`'s
  sideboard-dropping, `match_flow.rs`'s max-sideboard-size, AND
  `companion.rs`'s `companion_offers` sideboard-slot branch (the consumer
  round 3 missed and maintainer review round 4 caught directly) all observe
  `Forbidden` — three call sites pinned specifically, not just the field's
  value in isolation, so a future regression to any one of them fails
  visibly. Same shape for `uses_commander` across its own five real
  call sites (§1's audit list).
- **`uses_commander` requires both conditions** (new — maintainer review
  round 3, CONTEXT.md point 4): `command_zone: true` alone (no threshold) and
  `commander_damage_threshold: Some(_)` alone (no command zone) both derive
  `uses_commander: false`; only both together derive `true` — the exact
  mismatched-combination cases the review asked for.
- **Preset-readiness gate is actually enforced, not just documented** (new —
  maintainer review round 4, CONTEXT.md point 5 / "no caveated exposure"): a
  synthetic preset declaring `damage_timing: CombatDamageTiming::OnStack`
  before that engine support exists must be rejected by the registry/format
  list at registration time (not merely "not recommended in a doc comment")
  — the concrete test that proves `middle_school()`/`classic_magic()` cannot
  silently become selectable by a future author who skips reading §7.
- **Protocol-version compatibility** (new — maintainer review round 4,
  CONTEXT.md point 4, version-skew design): a client below the
  bumped `MIN_SUPPORTED_PROTOCOL` is rejected at the existing hello/handshake
  gate when the host's `FormatConfig.format` is `Custom` — reusing whatever
  test harness already covers `protocol_version` mismatch rejection
  (`server-core/src/protocol.rs`'s existing tests), extended with a
  Custom-format case rather than a new mechanism.
- Serde round-trip of `FormatConfig` with `Some(custom_rules)` and `None`.
- `CustomFormatDef::from_lobby_config`: a `FormatConfig` with non-default
  `starting_life`/`max_players`/`deck_size`/`range_of_influence`/`team_based`/
  `command_zone`/`commander_damage_threshold`/`sideboard_policy` round-trips
  into a `CustomFormatRules.structural` that matches field-for-field, with
  `legality` at its defaults (`legal_sets: None`) — the general Axis-A save
  mechanism, not a specific saved format's values. Does NOT include
  `archenemy_player` (removed per round 3, CONTEXT.md point 3 — not part of
  `StructuralRules` at all).
- **NEW — maintainer review round 8**: the same round-trip test also
  asserts `label == name` verbatim, `short_label` is `name`'s first 3
  alphanumeric characters uppercased (plus a dedicated case for a
  fewer-than-3-alphanumeric-character name, asserting the shorter code
  rather than a panic or silent padding), an empty/whitespace-only `name`
  is rejected rather than producing an empty `label`, and
  `derive_structural_description` produces a non-empty, config-derived
  string for at least two distinct `StructuralRules` values (proving it
  actually reads the config rather than returning a constant).
- **NEW — maintainer review round 9 (runtime display identity, §1)**:
  - `GameFormat::label()` for a registry-backed Axis-B id (e.g.
    `old_school_93_94()`'s id) returns `Cow::Owned` matching that preset's
    real `CustomFormatDef.label` — proving the registry lookup actually
    fires, not just that the function compiles.
  - `GameFormat::label()` for an Axis-A sentinel id (never present in
    `custom_format_registry()`) returns exactly `"Custom Format"` — the
    documented generic fallback, not a panic, not an empty string, and
    critically NOT the saving player's original chosen name (proving the
    boundary is actually honored, since silently leaking the name back in
    would look like a fix while re-opening the exact "resolved def doesn't
    exist here" gap this round closes).
    `GameFormat::label()` for every existing built-in variant still returns
    the identical `&'static str` value as before this change (a
    `Cow::Borrowed` equality check against the pre-round-9 constants) —
    proving the signature change is genuinely behavior-preserving for the
    21 existing variants, not just source-compatible.
  - `FormatConfig::for_format(GameFormat::Custom(_))` — a dedicated test
    asserting this panics via the documented `unreachable!()` (matching the
    existing `sideboard_policy`/`uses_commander` guard-arm test pattern,
    §1/§3), i.e. proving no caller path can silently receive a
    wrong-but-plausible default `deck_size` for a Custom format.
  - `evaluate_selected_format`'s deck-size check (`deck_validation.rs:1075`
    and the sibling call at `:2256`) with a `DeckCompatibilityRequest`
    carrying `custom_rules` for an Axis-A lobby save with `deck_size: 30`
    (a value distinct from every built-in format's — confirmed by direct
    check against `format.rs`'s constructors, which range over 40/50/60/100
    only) rejects a 60-card deck sized to the default `for_format` would
    otherwise silently fall back to — the concrete regression test for the
    deck-size correctness bug this round's audit found, not just the
    display-label issue Matt's review text quoted.
- **NEW — maintainer review round 11 (§1's Premodern-precedent
  resolution)**: a dedicated test asserting `old_school_93_94()`/
  `old_school_95()`'s legality evaluation path is the SAME function
  (`evaluate_custom_format`'s pool-legality step, §3) that every other
  `legal_sets`-bearing format already goes through — not a special-cased
  arm — and a comparison-style test confirming `Premodern`'s own legality
  check (`LegalityFormat::Premodern`) and a `legal_sets`-restricted Custom
  format both accept ANY printing of a card whose name/oracle-id has a
  legal printing, with neither rejecting on set/frame/foil grounds — proving
  the two models are genuinely equivalent in what they check, not just
  similar in spirit. (The round-10 `ArtChainEntry` display test is removed,
  not superseded in place — that feature is out of scope for this proposal,
  see §1.)

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

**Preset-readiness gate (introduced round 3, TIGHTENED round 4 — CONTEXT.md
points 5 and, this round, the "no caveated exposure" rule from maintainer
review round 4 point 3).** A preset (Axis B typed constructor OR an Axis A
lobby save that sets non-default `legality`) must not be registered/exposed
as a selectable format until every legality/legacy field it declares is BOTH
fully specified AND actually consumed by the evaluator/engine at an
authoritative seam — not merely present as a struct field. **Round 4
tightens this further: there is no intermediate "playable with a fidelity
caveat" state.** A preset is either fully correct for every axis it declares,
or it does not appear as a selectable format at all. This directly overrides
this document's own earlier "Middle School / Classic Magic are
playable-with-caveat until damage-on-stack lands" framing (§4, §8) — that
framing is retracted, not merely superseded.

**This must be a technical gate, not a documented convention (sharpened by
an automated review pass — correct to push on, since "don't register it, per
this doc" is not itself an enforcement mechanism a future author is bound
by).** `custom_format_registry()` — the function `custom_format_registry() ->
Vec<CustomFormatDef>` already named in §1 — validates every preset it would
return against a small static capability table (e.g. `const
IMPLEMENTED_LEGACY_AXES: &[...]`, populated as each axis in §4 actually
lands) BEFORE including it in the returned list. A preset declaring an axis
not yet in that table is dropped from the registry (or the registry
construction fails a debug assertion / startup check, implementation's
choice) — a future author who adds `middle_school()` before
`CombatDamageTiming::OnStack` lands gets a build-time or startup-time
failure, not a silently-ignored doc-comment warning. This is the concrete
mechanism §6's "actually enforced, not just documented" test targets.

**Gate scope, precisely (maintainer review round 5 caught that this wasn't
stated precisely enough; round 6's `reprint_policy` relocation, §1/§3, makes
it precise by construction rather than by exemption): `IMPLEMENTED_LEGACY_AXES`
covers exactly `LegacyRuleSet`'s four axes — `mana_burn`, `damage_timing`,
`wish_scope`, `legend_rule_scope` — because those are now the ONLY fields on
`CustomFormatRules` that are independently declarable AND behavior-bearing.**
`reprint_policy` doesn't need an exemption from this gate anymore — it isn't
on `CustomFormatRules` at all after round 6's move to `CustomFormatDef`, so
there's nothing there for the gate to either cover or exempt. There is no
ante-card field to cover yet either (CONTEXT.md Open item 5 — no schema slot
exists). If a future field is ever added to `CustomFormatRules` with real
behavior attached (an ante-list, say), it must be added to this gate in the
SAME change — the gate's job is to cover every behavior-bearing field on the
RESOLVED RULES struct, and it is a review-time checklist item whenever
`CustomFormatRules`/`LegacyRuleSet` gains a member, not a one-time list to
write and forget. A field that's purely descriptive (like `reprint_policy`
now) belongs on `CustomFormatDef` instead, outside this gate's scope by
construction — round 6's actual lesson: when a field doesn't cleanly belong
in the gate's coverage, that's a signal it may be in the wrong struct, not
just missing from the gate.

**A second, separate construction-time gate — round 12, `CustomFormatDef`
metadata, not `CustomFormatRules`:** `custom_format_registry()` additionally
rejects (build-time or startup-check, same choice as above) any def where
`reprint_policy.is_some() != matches!(printing_fidelity,
PrintingFidelity::SetCodeApproximation)`, and any `SetCodeApproximation` def
whose `description` doesn't disclose the limitation (§6's test is this same
check, run offline). This is deliberately a DIFFERENT gate from
`IMPLEMENTED_LEGACY_AXES` — it does not block registration on unimplemented
engine work (there is none to wait for; §1's round-12 note explains why no
printing predicate is being built), it blocks on an authoring omission
(a preset that declares reprint intent but forgets to disclose the known
approximation, or vice versa). Both gates independently apply to the same
four EC presets; neither substitutes for the other.
- `swedish_old_school()` (phase 1) is BLOCKED from registration by
  CONTEXT.md Open item 6 alone — the reprint-policy metadata VALUE needs
  confirming against the primary source before shipping, so a future
  maintainer doesn't inherit an inaccurate label. This is a documentation-
  accuracy blocker, not a missing-enforcement one (see §3's resolution) —
  it does not require `IMPLEMENTED_LEGACY_AXES` coverage, since
  `reprint_policy` was never meant to be in that gate's scope.
- `middle_school()` and `classic_magic()` (phase 2) are BLOCKED from
  registration until `CombatDamageTiming::OnStack` (§4, LARGE) is fully
  implemented — both declare it, and per the no-caveated-exposure rule
  neither may register while any declared axis is unimplemented. This one
  DOES require `IMPLEMENTED_LEGACY_AXES` coverage, since `damage_timing` is
  a real `LegacyRuleSet` axis.
- No phase-2 EC preset may declare `LegendRuleScope::PreM14AnyController`
  until the historical conflation flagged in CONTEXT.md/RESEARCH.md resolves.
  Moot today (all four EC presets default this to `Modern`), but binding on
  any future preset.

## 8. Sequencing

**Phase 1 — general engine + the smallest end-to-end slice.** Deliberately
excludes all `LegacyRuleSet` engine wiring; ships something real and testable
first.

1. **General engine** — `CustomFormatId`, `CustomFormatRules` (with
   `StructuralRules` + `LegalityRules` sub-structs, §1) plus `LegacyRuleSet`
   (present in the schema, unused by anything phase 1 ships),
   `CustomFormatDef` (with `ReprintPolicy` as its metadata field, §1 —
   NOT part of `CustomFormatRules`), `GameFormat::Custom` variant + all
   match arms, `FormatConfig.custom_rules`, `evaluate_custom_format` reusing
   existing enforcers, registry export. (Compiler-guided, mirrors phase 53.)
2. **Axis A lobby save** — `CustomFormatDef::from_lobby_config`, a "save as
   custom format" action on `HostSetup.tsx`, and a load path so a saved
   `CustomFormatDef` appears as a selectable format on return visits. Small
   frontend-plus-plumbing slice; useful on its own to any casual table.
   Validates the schema's Axis A end from a real UI.
3. **`swedish_old_school()` preset (Axis B, phase 1)** — the one Axis B
   preset that needs zero legacy-rules wiring (§2). Validates the engine's
   Axis B end (legal-set membership, empty banned list, the 25-name
   restricted list) without touching §4 at all. Resolve CONTEXT.md items 5–6
   (ante-card handling, reprint policy VALUE) before finalizing this
   preset's constructor. **Registration as a selectable format is gated on
   Open item 6 alone** (the metadata value, per §3/§7 — `ReprintPolicy` is
   not part of `IMPLEMENTED_LEGACY_AXES` since it isn't behavior-bearing) —
   the constructor and its tests can land in this step, but don't wire it
   into the format-selection UI/registry as choosable until that item
   resolves. Can proceed in parallel with step 2 — disjoint fields of the
   same schema.

**Phase 2 — the four EC formats + the legacy-rules engine work they need.**
Ships once phase 1 has proven the schema and both axes end-to-end.

4. **`old_school_93_94()` / `old_school_95()` as data (Axis B)** — these two
   `CustomFormatDef` constructors + registry entries + preset-integrity
   tests (§2). **Registerable once step 5 (mana burn) lands** — neither
   needs `CombatDamageTiming::OnStack`, so neither is blocked by step 7.
5. **Mana burn** — `LegacyRuleSet.mana_burn` (`ManaBurnPolicy`) at the
   transition/empty-pool seam (§4's `ManaExpiry::EndOfPhaseGroup` design).
   Small. Unlocks step 4's registration.
6. **Pre-M10 Wish exile access** (`wish_scope: WishOutsideGameScope`) —
   SMALL (RESEARCH §9); one-line pool-widening at
   `search_outside_game.rs:72`, gated by the enum value, reusing the
   existing tested face-up-exile collector/mover. Can land with or just
   after mana burn (step 5).
7. **Combat damage timing (`CombatDamageTiming::OnStack`)** — LARGE; its own
   sub-project; likely the last thing to land in phase 2. **Per the
   tightened preset-readiness gate (§7): `middle_school()` and
   `classic_magic()`'s `CustomFormatDef` constructors and tests can be
   written and land in source alongside step 4, but neither may be
   registered/exposed as selectable until this step is fully done — no
   "playable with a caveat" interim state.**
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
