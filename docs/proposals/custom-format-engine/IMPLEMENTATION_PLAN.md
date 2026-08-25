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
| **1b** | Consumption-site audit: widens three `engine-wasm` exports to take a resolved `FormatConfig` instead of a bare `GameFormat`, adds `FormatConfig`'s still-missing `sideboard_policy`/`default_deck_copy_limit` stored fields, and migrates the one remaining bare-`GameFormat` `.sideboard_policy()` call in `companion.rs` (the `uses_commander()` migration across `companion.rs`/`deck_loading.rs`/`match_flow.rs` landed in Phase 1a itself — see its section below). | 1a |
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

**Status: in review, [#7818](https://github.com/phase-rs/phase/pull/7818)
(open, not yet merged) — code is written and has been through two rounds of
maintainer/CodeRabbit feedback, addressed on the current head. Per this
charter's own review-before-merge rule, this phase is not "done" until that
PR actually merges; update this line then, not before.**

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

**Correction to an earlier claim in this document:** `FormatConfig` stores
`uses_commander` and `supplies_fixed_deck` as derived fields — it does
**not** yet store `sideboard_policy` or `default_deck_copy_limit`; both
remain the bare `GameFormat::sideboard_policy()`/`::default_deck_copy_limit()`
methods (which, per Phase 1a, already return a disclosed fail-closed
fallback for `Custom` rather than panicking, so this is a code-cleanliness
gap for Phase 1b to close, not a safety one).

## Phase 1b — Consumption-site audit + caller migration

With the `uses_commander()` migration above pulled forward into Phase 1a,
this phase's remaining scope is narrower than originally charted:

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
- Add the stored `sideboard_policy`/`default_deck_copy_limit` fields
  `FormatConfig` is still missing (see the correction above), and migrate
  `companion.rs`'s remaining direct `.sideboard_policy()` bare-`GameFormat`
  call (already panic-safe today, since that method has a disclosed
  fallback for `Custom` — this is architectural cleanup, not a safety fix)
  to read the new stored field instead.

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
implementation decision): given a `CustomFormatRules`, it derives every
runtime field (`command_zone`/`uses_commander`/`commander_damage_threshold`/
`singleton`) from `custom_rules.structural` rather than accepting them
independently, sets `format`/`custom_rules` consistently, and is the one
place `PLAN.md`'s validated-construction requirement (§1; `CONTEXT.md` open
item 1) is actually satisfiable — `from_lobby_config`'s own output has no
`format` field to validate that invariant against. The resolver's callers:
Axis-A saved-definition selection (this phase) and Axis-B registry-preset
selection (Phase 1d, reusing this same resolver — no separate one built
there). Phase 1a's `FormatConfig` deserialization boundary currently rejects
every external `GameFormat::Custom` unconditionally because no resolver
exists yet to derive its duplicate runtime fields — this phase also revises
that boundary to use the resolver's own derivation logic for a full
consistency re-check (not just the `custom_rules.id` match Phase 1a already
does), since that's the same piece of work as building the resolver, not a
separate one.

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
