# Custom Format Engine — Phased Implementation Plan

**Status:** the design (`PLAN.md`/`CONTEXT.md`/`RESEARCH.md`) is merged. This
document is the phased charter for building it, for visibility into the
overall shape and sequencing before each phase's code lands as its own PR.
It is not itself a design change — no line of `PLAN.md`/`CONTEXT.md`/
`RESEARCH.md` is revised here.

## Sequencing

Seven phases, landing as separate PRs in order. Each phase is independently
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
| **1b** | Consumption-site audit: migrates every bare-`GameFormat` reader that needs full format context (`companion.rs`, `deck_loading.rs`, `match_flow.rs`, three `engine-wasm` exports) to read the resolved `FormatConfig` instead. | 1a |
| **1c** | Axis A — "save the current lobby setup as a custom format," an ad-hoc, client-persisted format built from a lobby's live settings. | 1a, 1b |
| **1d** | Real deck-legality evaluation for `Custom` formats (mirroring the existing constructed/commander evaluators), the `custom_format_registry()` gate, and the first registry preset, `swedish_old_school()`. | 1a, 1b, 1c |
| **2a** | Axis B presets: `old_school_93_94()` and `old_school_95()`. | 1d |
| **2b** | Mana burn (`ManaExpiry::EndOfPhaseGroup`) — the first `LegacyRuleSet` axis to get real engine behavior. | 1a |
| **2cd** | Pre-M10 Wish exile access and legend-rule scope — the remaining two `LegacyRuleSet` axes short of combat-damage timing. | 1a |

1a and 1b are both prerequisites for everything after them; 2a/2b/2cd have no
dependency on each other and could in principle land in any order relative
to one another, but are listed in the order they're expected to ship.

## Phase 1a — General engine schema

**Status: implemented, [#7818](https://github.com/phase-rs/phase/pull/7818).**

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
- **`sideboard_policy()` and `default_deck_copy_limit()` do not panic for
  `Custom`.** The original sketch had them (along with `uses_commander()`
  and `for_format()`) permanently `unreachable!()`, on the assumption that a
  bare `GameFormat` genuinely can't answer them without the resolved
  `FormatConfig`. That's true, but both are directly reachable from
  already-shipped `engine-wasm` exports with attacker-controlled input the
  moment `GameFormat::Custom` deserializes — before any legitimate UI to
  construct one exists. Both now return a disclosed, fail-closed fallback
  (`Forbidden` / `UpTo(1)`) instead. `uses_commander()` and `for_format()`
  remain permanently `unreachable!()` — nothing reachable can call them with
  a bare `Custom` value once the deck-validation dispatch functions honestly
  reject `Custom` up front.

## Phase 1b — Consumption-site audit + caller migration

Three functions in `crates/engine-wasm/src/lib.rs` currently call a bare
`GameFormat` method directly with no `FormatConfig` context:
`sideboard_policy_for_format`, `max_deck_copies_for_format`, and — corrected
during Phase 1a's review, which traced the actual call graph rather than
assuming — `validate_name_deck_for_format_full` (reached from
`initialize_game_impl`, the core game-init path), **not**
`is_card_commander_eligible_for_format` as an earlier pass of this charter
named (that function has its own local wildcard-matched dispatch and was
never at risk). This phase widens all three — and the `companion.rs`/
`deck_loading.rs`/`match_flow.rs` call sites that read `state.format_config.
format.uses_commander()`/`.sideboard_policy()` directly — to read the
already-resolved `FormatConfig` field instead of recomputing from the bare
enum.

One additional correction: `FormatConfig` does not yet have a stored
`sideboard_policy` field the way it has stored `uses_commander`/
`supplies_fixed_deck`/`default_deck_copy_limit` fields — adding one is part
of this phase's own work, not something Phase 1a already did.

## Phase 1c — Axis A: save-as-custom-format

The lobby host's "save the current settings as a custom format" action.
Builds a `CustomFormatDef` from a lobby's live `FormatConfig`-shaped
settings (starting life, player count, singleton, command zone, sideboard
policy, etc.) and persists it client-side. This is the first phase where
`GameFormat::Custom` becomes constructible through any real UI action.

## Phase 1d — Deck-validation dispatch + first registry preset

Real `evaluate_custom_format`/`quick_custom_format_check` bodies, mirroring
the existing `evaluate_constructed`/`quick_constructed_check` pipeline
(pool legality → banned → restricted → copy limit). The
`custom_format_registry()` gate (rejects a preset that declares a
`LegacyRuleSet` axis the engine doesn't implement yet, or whose
`reprint_policy`/`printing_fidelity` pairing disagrees) goes live here, with
its first real entry, `swedish_old_school()`.

## Phase 2a — Eternal Central presets: Old School 93/94, Old School 95

Two more registry presets, reusing the schema and evaluator built in 1a–1d.
No new engine mechanism — these formats need no `LegacyRuleSet` axis beyond
what 2b/2cd add.

## Phase 2b — Mana burn

The first `LegacyRuleSet` axis with real runtime behavior: a
`ManaExpiry::EndOfPhaseGroup` variant, so unspent mana persists across a
phase group's steps and only causes life loss at the group's real boundary,
gated behind the `LegacyRuleSet.mana_burn` flag so every other format's
behavior is unaffected.

## Phase 2cd — Pre-M10 Wish exile access + legend-rule scope

The remaining two `LegacyRuleSet` axes short of `CombatDamageTiming`: widen
Wish-effect exile access to include face-up exile piles under
`WishOutsideGameScope::AnyCardOutsideGame`, and add a
`LegendRuleScope::Global` (choiceless, cross-controller) branch to the
legend-rule state-based action. Independent of each other and of 2a/2b;
merged into one phase for delivery efficiency, not because of a shared
dependency.
