# Custom Format Engine — Phased Implementation Plan

**Status:** the design (`PLAN.md`/`CONTEXT.md`/`RESEARCH.md`) is merged. This
document is the phased charter for building it, for visibility into the
overall shape and sequencing before each phase's code lands as its own PR.
It is not itself a design change — no line of `PLAN.md`/`CONTEXT.md`/
`RESEARCH.md` is revised here.

## Sequencing

Seven phases, each landing as its own separate PR, in dependency order —
not necessarily numeric order: see the table below for which phases can
land in either order relative to each other. Each phase is independently
planned, reviewed, implemented, and reviewed again before merge — the same
plan → review → implement → review pipeline used for engine bug fixes, run
per phase rather than once for the whole feature, because the full feature
is too large a unit to plan, implement, and review coherently in one pass.

`CombatDamageTiming::OnStack` — and the two Eternal Central presets that need
it, `middle_school()` and `classic_magic()` — is deliberately **not** in this
charter. `PLAN.md` §6/§7/§8 already flag it as needing its own design pass
before implementation (its own no-caveated-exposure gate: shipping a preset
that silently doesn't enforce a rule it claims to isn't acceptable). It's a
separate, later, larger sub-project.

| Phase | What it delivers | Depends on |
|---|---|---|
| **1a** | General engine schema: `GameFormat::Custom(CustomFormatId)`, `CustomFormatRules`/`StructuralRules`/`LegalityRules`/`CustomFormatDef`, the four `LegacyRuleSet` axes (schema only, gated so none can be declared until a later phase implements it), hand-written `GameFormat` wire format. No registry, no evaluation, no frontend. | — |
| **1b** | Consumption-site audit: widens three `engine-wasm` exports to take a resolved `FormatConfig` instead of a bare `GameFormat`, and adds `FormatConfig`'s still-missing `default_deck_copy_limit` stored field (the `uses_commander()`/`sideboard_policy()` migrations across `companion.rs`/`deck_loading.rs`/`match_flow.rs`, and their stored fields, landed in Phase 1a itself — see its section below). | 1a |
| **1c** | Axis A — "save the current lobby setup as a custom format," an ad-hoc, client-persisted format built from a lobby's live settings. | 1a, 1b |
| **1d** | Real deck-legality evaluation for `Custom` formats (mirroring the existing constructed/commander evaluators), the `custom_format_registry()` gate, and the first registry preset, `swedish_old_school()`. | 1a, 1b, 1c |
| **2a** | Axis B presets: `old_school_93_94()` and `old_school_95()`. Constructors + preset-integrity tests only depend on 1d; **registration** (making them selectable via `custom_format_registry()`) additionally depends on 2b — see below. | 1d (construction); 1d + 2b (registration) |
| **2b** | Mana burn (`ManaExpiry::EndOfPhaseGroup`) — the first `LegacyRuleSet` axis to get real engine behavior. | 1a |
| **2cd** | Pre-M10 Wish exile access and legend-rule scope — the remaining two `LegacyRuleSet` axes short of combat-damage timing. | 1a |

1a is a prerequisite for every later phase — it's the schema everything else
is built on. 1b is additionally required before 1c/1d specifically, since
both flow through the exact WASM/consumption-site call paths 1b migrates.
2b and 2cd are independent `LegacyRuleSet` axis implementations that never
touch the commander-eligibility/consumption-site code 1b changes, so they
depend only on 1a directly and could in principle land before or after
1b/1c/1d. 2a is the one phase with a split dependency: see its own row above.

## Phase 1a — General engine schema

**Status: merged, [#7818](https://github.com/phase-rs/phase/pull/7818)
(merge commit `315e4950124f2e006d3edaed0c64b04a159e601d`) — this phase is
complete.**

Adds `GameFormat::Custom(CustomFormatId)` and its supporting schema
(`crates/engine/src/types/custom_format.rs`, new), threaded through every
exhaustive match over `GameFormat` in `crates/engine/src/types/format.rs`
and `crates/engine/src/game/deck_validation.rs`.

Two corrections surfaced during this phase's own plan review, worth
recording here since they affect how later phases should be read against
the charter:

- **Scope-path count is 5, not 4.** `deck_validation.rs` has two of its own
  exhaustive `match` blocks over `GameFormat` (the deck-compatibility
  dispatch functions) that an earlier pass of this charter didn't carry
  forward into Phase 1a's file list. They're compiler-forced the moment the
  `Custom` variant exists, so they had to land in the same commit.
- **None of the seven exhaustive-match methods panic for `Custom` anymore.**
  The original sketch had `sideboard_policy()`/`default_deck_copy_limit()`
  return a disclosed, fail-closed fallback (`Forbidden` / `UpTo(1)`), and
  `uses_commander()`/`for_format()` permanently `unreachable!()`, on the
  assumption that a bare `GameFormat` genuinely can't answer any of them
  without the resolved `FormatConfig` — true, but `sideboard_policy()` and
  `default_deck_copy_limit()` are directly reachable from already-shipped
  `engine-wasm` exports with attacker-controlled input the moment
  `GameFormat::Custom` deserializes, before any legitimate UI to construct
  one exists, so a disclosed fallback was the right call for those two.
  `for_format()` and `uses_commander()` are different: both are public
  queries a caller could hold any `GameFormat` for (including one parsed
  straight from untrusted input via `FromStr`), and neither has a safe
  fallback value to disclose — a Custom format can legitimately resolve to
  a commander-using configuration or genuinely different structural rules,
  so guessing `false`/a default `FormatConfig` would be silently wrong, not
  safe. Both now return `Result` instead of panicking, matching each
  other's pattern. `uses_commander()` additionally needed every real-world
  caller found and migrated to the resolved `FormatConfig.uses_commander`
  boolean first (see below) — the earlier claim here that "nothing
  reachable can call it with Custom once deck-validation rejects Custom up
  front" was wrong; three ordinary game-flow files reached it by a
  different path entirely, unrelated to deck validation. Once those
  callers were fixed, the method itself was made fallible too, since being
  safe for *today's* callers doesn't make it a safe *public* contract for
  the next one. `CommanderEligibilityRule::from_source_format()` had the
  identical problem (no production caller yet, but still a `pub fn`
  `unreachable!()` for `Custom`) and got the same fix — it now returns
  `Result<Option<Self>, FormatConfigError>`, keeping `Ok(None)` ("this
  built-in has no commander-eligibility concept") distinct from `Err`
  ("Custom has no source format to read").

Review on #7818 also caught that this schema's `LegacyRuleSet` axis enums
had drifted from `PLAN.md`'s canonical variant names (an artifact of this
charter's own planning rounds using placeholder names that were never
cross-checked against the merged design). Both the code and this document
now use the real names throughout: `ManaBurnPolicy::{Modern,Obsolete}`,
`CombatDamageTiming::{Modern,OnStack}`, `WishOutsideGameScope::
{PostM10SideboardOnly,PreM10ReachesExile}`,
`LegendRuleScope::{Modern,PreM14AnyController}`. It also caught two
unvalidated-input gaps: `StructuralRules`' `command_zone`/
`commander_damage_threshold`/`commander_eligibility_rule` were three
independently-settable fields that could represent an incoherent state
(command zone off with a commander-damage threshold set) — replaced with
one discriminated `CommandZoneMode` enum. And `validate_custom_rules_
consistency` existed but nothing called it — `FormatConfig`'s deserialization
now enforces it as the single authoritative ingress, unconditionally
rejecting `GameFormat::Custom` until a real resolver exists to derive this
struct's own runtime fields (`command_zone`/`uses_commander`/
`commander_damage_threshold`/`singleton`) from `custom_rules.structural`,
rather than leaving them independently, unvalidatedly writable
side-by-side with it.

Later review rounds went further and found the deserialize-boundary fix
alone doesn't make `GameFormat::Custom` safe: it stays fully constructible
in-memory (this schema's own tests need that), and three files —
`companion.rs`, `deck_loading.rs`, `match_flow.rs` — called the bare
`GameFormat`'s `uses_commander()` unguarded on ordinary game-flow paths
(deck loading, companion setup, between-games handling), which panics for
`Custom`. **This migration is done in Phase 1a itself, not deferred to
Phase 1b as an earlier pass of this document had it** — the maintainer
correctly identified it as a merge blocker, since `GameFormat::Custom`
being real and constructible the instant this schema lands isn't something
a later phase's timeline can gate. `deck_loading.rs`/`match_flow.rs` now
read the already-existing `FormatConfig.uses_commander` stored field
directly; `companion.rs`'s four companion-resolution functions were widened
to take the resolved `uses_commander: bool` instead of a bare `GameFormat`,
with every call site updated to pass it from the caller's own resolved
context.

The same three files had the identical problem for `sideboard_policy()`,
found in the next review round — and worse in one respect: unlike
`uses_commander`, a Custom format's declared sideboard policy already
exists as a real field (`custom_rules.structural.sideboard_policy`) the
moment `CustomFormatRules` is constructed, so the disclosed `Forbidden`
fallback wasn't just "no safe answer available," it silently discarded
known data — submitted sideboards were emptied, capped at zero, and hidden
from companion candidates even when the real policy was `Unlimited`. Fixed
the same way: `FormatConfig` now stores its own `sideboard_policy` field
(mirroring `uses_commander`/`supplies_fixed_deck`), derived once at
construction for every built-in, and the three consumers read that field.

**Correction to an earlier claim in this document:** `FormatConfig` now
stores `uses_commander`, `supplies_fixed_deck`, *and* `sideboard_policy` as
derived fields (all landed in Phase 1a). Only `default_deck_copy_limit`
remains the bare `GameFormat::default_deck_copy_limit()` method (which
already returns a disclosed fail-closed fallback for `Custom` rather than
panicking, so this is a code-cleanliness gap for Phase 1b to close, not a
safety one).

## Phase 1b — Consumption-site audit + caller migration

With the `uses_commander()`/`sideboard_policy()` migrations above pulled
forward into Phase 1a, this phase's remaining scope is narrower than
originally charted:

- Three functions in `crates/engine-wasm/src/lib.rs` currently call a bare
  `GameFormat` method directly with no `FormatConfig` context:
  `sideboard_policy_for_format`, `max_deck_copies_for_format`, and —
  corrected during Phase 1a's review, which traced the actual call graph
  rather than assuming — `validate_name_deck_for_format_full` (reached from
  `initialize_game_impl`, the core game-init path), **not**
  `is_card_commander_eligible_for_format` as an earlier pass of this
  charter named (that function has its own local wildcard-matched dispatch
  and was never at risk). This phase widens all three to take a resolved
  `FormatConfig` instead of a bare `GameFormat`.
- Add the stored `default_deck_copy_limit` field `FormatConfig` is still
  missing (see the correction above) — the last of the four derived fields
  not yet mirrored as a stored field.

## Phase 1c — Axis A: save-as-custom-format

The lobby host's "save the current settings as a custom format" action.
`CustomFormatDef::from_lobby_config(name, config: &FormatConfig) ->
CustomFormatDef` (`PLAN.md`'s canonical constructor contract) captures a
lobby's live built-in `FormatConfig` into a `CustomFormatDef` **definition**
— it produces a definition, not an active `FormatConfig`, and stays scoped
to exactly that; this is the first phase where `GameFormat::Custom` becomes
constructible through any real UI action, but only as a saved definition,
not yet as something a game can start with.

**Owns building the shared active-`FormatConfig` resolver, separate from
`from_lobby_config`.** A saved definition (or a registry preset — see Phase
1d) is only useful once something turns it into a real, active `FormatConfig`
at the moment a player *selects* it to start a game — a distinct step from
saving/defining it, with no owner in earlier passes of this charter. This
phase builds that one shared resolver (name/signature is this phase's own
implementation decision): given a `CustomFormatRules`, it derives the
**complete** `StructuralRules -> FormatConfig` mapping — not a subset —
since `StructuralRules`' own doc comment states every field mirrors an
existing `FormatConfig` field 1:1, and `PLAN.md:713-719` requires
`from_lobby_config` (the reverse direction) to capture every one of them
with full fidelity; a resolver covering only a subset would leave two
independently-writable representations of the omitted fields with no
stated authority for keeping them consistent. The direct-copy fields —
`starting_life`/`min_players`/`max_players`/`deck_size`/`singleton`/
`range_of_influence`/`team_based`/`sideboard_policy` — pass through
unchanged. The `CommandZoneMode`-derived fields —
`command_zone`/`commander_damage_threshold`/`uses_commander` — come from
`custom_rules.structural.command_zone_mode`'s own discriminant:
`CommandZoneMode::Disabled` resolves to `command_zone: false`,
`commander_damage_threshold: None`, `uses_commander: false`;
`CommandZoneMode::Enabled { commander_damage_threshold, .. }` resolves to
`command_zone: true` and that same `commander_damage_threshold` unchanged
(which the `Enabled` arm itself permits to be `None` — a command zone
without commander damage, e.g. Tiny Leaders/Oathbreaker-style formats), with
`uses_commander: commander_damage_threshold.is_some()` — **not**
unconditionally `true` on `Enabled` alone. This matches `PLAN.md`'s
already-stated invariant (`command_zone && commander_damage_threshold.is_some()`)
exactly, now expressed through the enum's own discriminant instead of three
independently-settable fields, and keeps the enabled-without-threshold case
(a supported format class) representable and resolved correctly rather than
forced to `uses_commander: true`. **This phase's own tests must cover the
`Enabled { commander_damage_threshold: None, .. }` case explicitly** (a
Tiny-Leaders/Oathbreaker-shaped custom format) asserting it resolves to
`uses_commander: false`, alongside the `Some(_)` case resolving to `true` —
not just the two `CommandZoneMode` variants in isolation. `CommandZoneMode::
Enabled`'s `eligibility_rule` is not mirrored onto `FormatConfig` at all —
`FormatConfig` has no such field; it stays on
`custom_rules.structural` for the commander-eligibility check (Phase 1d) to
read directly. `supplies_fixed_deck` is the one `FormatConfig` field this
resolver does **not** derive from `custom_rules.structural` — per `PLAN.md`'s
own accounting, it is always `false` for every Custom format, since no
custom-format use case for an engine-supplied fixed deck exists today; this
is a deliberate, named exclusion, not an oversight. The resolver sets
`format`/`custom_rules` consistently and is the one place `PLAN.md`'s
validated-construction requirement (§1; `CONTEXT.md` open item 1) is
actually satisfiable — `from_lobby_config`'s own output has no `format`
field to validate that invariant against. The resolver's callers:
Axis-A saved-definition selection (this phase) and Axis-B registry-preset
selection (Phase 1d, reusing this same resolver — no separate one built
there). Phase 1a's `FormatConfig` deserialization boundary currently rejects
every external `GameFormat::Custom` unconditionally because no resolver
exists yet to derive its duplicate runtime fields — this phase also revises
that boundary to use the resolver's own derivation logic for a full
consistency re-check (not just the `custom_rules.id` match Phase 1a already
does), since that's the same piece of work as building the resolver, not a
separate one.

Corrections surfaced during this phase's own plan review, recorded the same
way Phase 1a's were:

- **`StructuralRules` was missing `default_deck_copy_limit` entirely.** Phase
  1b added `FormatConfig.default_deck_copy_limit` as the fourth stored
  derived field, but the mirror on `StructuralRules` was never added, so an
  Axis-A save would have captured every other structural field and silently
  dropped the copy ceiling — and the resolver would have had nothing to
  rebuild it from but `GameFormat::Custom(_).default_deck_copy_limit()`'s
  fail-closed `UpTo(1)`. That is the identical silent-data-loss bug
  `sideboard_policy` was added to prevent in Phase 1a, one field later. Fixed
  by adding `StructuralRules.default_deck_copy_limit: DeckCopyLimit` as a
  direct-copy mirror.
- **`StructuralRules.deck_size` retyped `u16` -> `DeckSizeRule`.**
  `StructuralRules`' own doc comment claims every field mirrors an existing
  `FormatConfig` field 1:1, but `FormatConfig.deck_size` is a `DeckSizeRule`
  and this one was a bare `u16` — which cannot round-trip WHICH rule a saved
  format uses. CR 903.5a's exact-100 and CR 903.13f(1)'s 60-minimum are
  different rules, and the resolver would have had to guess. This is the same
  correction `FormatConfig` itself already took: lobby protocol **v42**
  (`crates/lobby-broker/src/protocol.rs`) records that exact `u16 ->
  DeckSizeRule` retype as an unconditional two-way PARSE break needing a
  version bump. **This phase needs no protocol bump for its own retype**: the
  changed shape is inside `CustomFormatRules`, reachable on the wire only via
  `FormatConfig.custom_rules`, and no live Custom `FormatConfig` has ever
  been accepted at the deserialization boundary (Phase 1a rejected every one
  categorically) nor producible by any shipped UI — so no peer has ever
  serialized or parsed this shape. The v42 precedent is why the retype is
  correct, not a reason to bump; the field it broke was one every format
  populated on every message, which is exactly the property this one lacks.
- **Archenemy and Momir are rejected as lobby-save sources, for two separate
  reasons with two separate citations.** Archenemy: CR 408.1 + CR 408.3 +
  CR 904.3 — the command zone holds specialized objects each casual variant
  defines for itself, and Archenemy's is a supplementary scheme deck of at
  least twenty scheme cards, not a commander. `CommandZoneMode::Enabled`
  can only name a commander-eligibility rule, and a saved definition carries
  no scheme deck, so the save would claim a command zone it cannot populate.
  Momir: CR 109.4c + CR 114.1 — its command zone holds a game-start *emblem*
  (a marker object with abilities and no other characteristics), controlled
  by the player who put it there. Corroborated in code: `deck_loading.rs`
  (the Momir branch around lines 946-974) grants that emblem keyed off
  `GameFormat::Momir` itself, not off any `StructuralRules` field, so a saved
  copy would resolve to a command zone with no emblem and no way to grant
  one. Collapsing these into one citation would misattribute a scheme-deck
  rule to an emblem format.
- **`from_lobby_config` returns `Result`, not a bare `CustomFormatDef`.**
  `PLAN.md`'s signature sketch is infallible, but `PLAN.md:713-719` also
  requires it to "reject explicitly rather than silently drop data," and this
  phase found four such states: an empty/whitespace-only name, a
  `GameFormat::Custom` source (re-saving a save would drop the source's own
  `legality`), and the two command-zone formats above. The signature is
  `Result<Self, FormatConfigError>`, matching the fallible-public-factory
  posture `for_format`/`uses_commander`/`from_source_format` already took in
  Phase 1a.
- **`sideboard_policy` comes from the resolved field, not the bare method.**
  `PLAN.md` specifies `structural.sideboard_policy:
  config.format.sideboard_policy()`, valid only under its stated
  built-in-source precondition. Corrected to read `config.sideboard_policy`
  (and likewise `config.default_deck_copy_limit`) — the whole point of Phase
  1a/1b's stored derived fields is that a lobby host may have tuned a value
  away from its format default, and re-deriving from `GameFormat` would save
  something the host never configured. The built-in-source precondition is
  now enforced rather than assumed (see the `Result` correction above), so
  the method call is not merely worse, it is unnecessary.
- **`passes_legacy_axis_gate` narrowed to `&LegacyRuleSet`.** It only ever
  read `def.rules.legality.legacy`, and this phase gives it a second caller
  that holds no `CustomFormatDef` at all: `FormatConfig`'s `Deserialize`
  impl, which sees a `CustomFormatRules` (display metadata never travels on
  an active config). Both ingresses must apply the identical gate — a
  deserialized custom format declaring an unimplemented axis would otherwise
  receive behavior no engine code enforces, bypassing the registry gate
  entirely since a deserialized config never passes through
  `custom_format_registry()`. **The asymmetry with
  `legal_sets`/`banned`/`restricted` (not gated) is deliberate:** those are
  declarative card-pool data the evaluator applies in full or not at all, so
  there is no partial-implementation risk; a `LegacyRuleSet` axis instead
  promises *runtime behavior* (mana burn, legend-rule scope, Wish reach) that
  may not be built yet, so declaring one misrepresents how the game will
  actually play.
- **The sentinel-collision guard is a real `assert!`, active in every build
  profile.** `LOBBY_SAVE_CUSTOM_FORMAT_ID` (`CustomFormatId(0)`) is reserved
  for Axis-A saves; no bundled preset may claim it, or a client-persisted
  ad-hoc save becomes indistinguishable from a registry-stable preset
  (`GameFormat::label()` would report the preset's name, and Phase 1d's
  evaluator would hand it the preset's banned/restricted lists). Verified
  against the workspace `Cargo.toml`: neither `[profile.release]` nor
  `[profile.server-release]` sets `debug-assertions = true`, so a
  `debug_assert!` would be compiled out of exactly the shipped builds that
  need it, and a documentation-only convention would not fail at all. The
  assert lives in an extracted
  `assert_no_lobby_save_sentinel_collision(&[CustomFormatDef])` helper that
  `custom_format_registry()` calls before filtering, so it is testable while
  the preset list is still empty.
- **Planechase was missed as a third unrepresentable lobby-save source,
  found in `/review-impl`.** The command-zone/eligibility check above
  correctly excludes Archenemy and Momir, but that check is gated on
  `config.command_zone`, and `FormatConfig::planechase()` sets
  `command_zone: false` — `deck_loading.rs:937` grants Planechase's shared
  communal planar deck (CR 901.15a) keyed on the literal
  `GameFormat::Planechase` comparison, exactly the same defect class as
  Archenemy's scheme deck and Momir's emblem, just reached by a mechanism the
  command-zone guard cannot see. A saved Planechase lobby would have passed
  `from_lobby_config` "successfully" and silently lost the planar deck.
  Fixed by naming the general class explicitly:
  `GameFormat::has_unrepresentable_auxiliary_deck_component` (matching
  `Planechase | Archenemy | Momir`), checked in `from_lobby_config` ahead of
  the command-zone match. Archenemy/Momir's rejection message changed from
  citing "command zone" to the shared "auxiliary deck or component" wording
  now that one predicate covers all three; the `(true, None)` match arm is
  kept as a defensive fallback rather than removed, since it still protects
  against a future command-zone format added to
  `CommanderEligibilityRule::from_source_format`'s `Ok(None)` bucket without
  also being added to the new predicate.

## Phase 1d — Deck-validation dispatch + first registry preset

Real `evaluate_custom_format`/`quick_custom_format_check` bodies, mirroring
the existing `evaluate_constructed`/`quick_constructed_check` pipeline
(pool legality → banned → restricted → copy limit). The
`custom_format_registry()` gate (rejects a preset that declares a
`LegacyRuleSet` axis the engine doesn't implement yet, or whose
`reprint_policy`/`printing_fidelity` pairing disagrees) goes live here, with
its first real entry, `swedish_old_school()`. Registry preset constructors
(`swedish_old_school()` here; `old_school_93_94()`/`old_school_95()` in
Phase 2a) produce `CustomFormatDef` values the same way `from_lobby_config`
does — a definition, not an active `FormatConfig` — and reuse Phase 1c's
shared resolver when a preset is actually selected to start a game.

**Owns widening `DeckCompatibilityRequest.selected_format`.** Today it's a
bare `Option<GameFormat>` — confirmed to have no `FormatConfig`/
`custom_rules` field at all (`CONTEXT.md` open item 1), so `Custom` reaches
`companion_candidates` and both selected-format dispatch functions with
nothing for `evaluate_custom_format` to read `legal_sets`/`banned`/
`restricted` from. This phase widens the request to carry the resolved
`FormatConfig`/`CustomFormatRules` before Custom dispatch, covering both
the summary and full compatibility paths — paired with the evaluator that
first needs it, rather than split across an earlier phase that has no use
for it yet.

## Phase 2a — Eternal Central presets: Old School 93/94, Old School 95

Two more registry presets, reusing the schema and evaluator built in 1a–1d.
No new engine mechanism — these formats need no `LegacyRuleSet` axis beyond
what 2b/2cd add.

**Construction and registration are separate steps here, per `PLAN.md`'s own
sequencing (§8, step 4/5).** Both presets declare `mana_burn:
ManaBurnPolicy::Obsolete` (a non-default `LegacyRuleSet` axis), so
`custom_format_registry()`'s `IMPLEMENTED_LEGACY_AXES` gate — built in Phase
1a specifically to keep an unimplemented axis from being declarable —
rejects them until `LegacyAxis::ManaBurn` is added to that list. That
addition is Phase 2b's own work. The two `CustomFormatDef` constructors and
their preset-integrity tests can land as soon as 1d does; actually
registering them as selectable formats cannot happen before 2b lands too,
regardless of which phase's code merges first.

## Phase 2b — Mana burn

The first `LegacyRuleSet` axis with real runtime behavior: a
`ManaExpiry::EndOfPhaseGroup` variant, so unspent mana persists across a
phase group's steps and only causes life loss at the group's real boundary,
gated behind the `LegacyRuleSet.mana_burn` flag so every other format's
behavior is unaffected.

## Phase 2cd — Pre-M10 Wish exile access + legend-rule scope

The remaining two `LegacyRuleSet` axes short of `CombatDamageTiming`: widen
Wish-effect exile access to include face-up exile piles under
`WishOutsideGameScope::PreM10ReachesExile`, and add a
`LegendRuleScope::PreM14AnyController` (choiceless, cross-controller) branch
to the legend-rule state-based action. Independent of each other and of
2a/2b; merged into one phase for delivery efficiency, not because of a
shared dependency.
