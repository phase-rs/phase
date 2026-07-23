# cr-suite

Executable **Comprehensive Rules** scenario suite for phase.rs.

Annotation tracking (`cargo rules-audit`) records which CR numbers appear in
comments. This crate turns those citations into **enforceable contracts**:

1. Parse `docs/MagicCompRules.txt`
2. Emit per-rule TOML fixtures under `scenarios/<section>/` (generated; not committed en masse)
3. Promote fixtures to `status = "executable"` with typed setup/steps/assertions
4. Run them through `GameScenario` / `GameRunner` (engine APIs only)

Tracked in https://github.com/phase-rs/phase/issues/6343; structural expansion
(this wave) is https://github.com/phase-rs/phase/issues/6514.

## What is committed

This PR ships the **runner/schema/generator** plus a **small seed set** under
`scenarios/`:

- **Executable** fixtures drive production engine paths (`GameAction::CastSpell`
  / `Effect::DealDamage` / SBA via priority) and assert discriminating
  transitions (CR `104.1`, `704.5a`, `704.5f`, `704.5g`).
- **Deferred** fixtures document follow-ups that would otherwise only assert
  setup (starting life, phase placement, nonlethal damage without a 1-damage
  spell step).

The full CompRules skeleton corpus is **not** committed — generate it
mechanically and land it in a separate follow-up PR:

```bash
cargo cr-suite --generate --update
```

Direct `GameState` mutation is not a legal scenario step. Damage must go through
`cast_lightning_bolt` + `resolve_top` (or a future production spell step).

## Commands

```bash
# Generate / refresh skeleton fixtures (preserves authored non-skeleton files)
cargo cr-suite --generate --update

# Catalog summary
cargo cr-suite --summary

# Run all executable fixtures
cargo cr-suite --run

# Filter
cargo cr-suite --run --section 704
cargo cr-suite --run --rule 704.5a --fail-fast
```

## Fixture status

| Status | Meaning |
|--------|---------|
| `skeleton` | Auto-generated from CompRules; not run |
| `executable` | Full setup/steps/assertions; runner executes |
| `not-applicable` | Definitional / non-scenario-testable |
| `deferred` | Waiting on engine primitives |

## Extending coverage

1. Generate skeletons (`cargo cr-suite --generate --update`) or hand-author a fixture
2. Set `status = "executable"`
3. Fill `[setup]`, `[[steps]]`, `[[assertions]]` using kinds from `predicates.rs`
4. Run `cargo cr-suite --run --rule <N>` and `cargo test -p cr-suite`

Do **not** duplicate game logic in assertions — only read engine state.

## Structure (issue #6514)

- `src/assert/` — typed assertion evaluators, one module per family
  (`life`, `zone`, `stack`, `priority`, `library`, `damage`, `combat_state`,
  `keywords`, `command`) plus documentation stubs for families that read state
  the runner can't yet reach (`targeting` CR 115, `layers` CR 613,
  `replacement` CR 614/615). Every `AssertionSpec` variant is wired through
  `assert::evaluate_assertion`.
- `src/section_plans/` — per-section coverage plans (structural guidance for
  promoting skeletons to executables). CR anchors are grep-verified against
  `docs/MagicCompRules.txt`.
- `src/predicates.rs` — the catalog mapping assertion kinds to the CR sections
  they advertise coverage for. Keep it in sync when adding an assertion kind.
- `src/fixtures/` — curated, typed named board pieces (creatures / spells) for
  scenario authoring.
- `src/report/` — plain / Markdown (`report::markdown`) / JSON (`report::json`)
  renderers; select with `cargo cr-suite --run --format md|json`.

## Skeleton corpus

The full CompRules skeleton corpus (thousands of `status = "skeleton"` TOML
files, one per included rule) is **generated**, not hand-written:

```bash
cargo run -p cr-suite --bin cr-suite -- \
  --generate --update \
  --comp-rules docs/MagicCompRules.txt \
  --scenarios-dir crates/cr-suite/scenarios
```

`--update` (`preserve_authored`) never overwrites an authored (executable /
deferred / not-applicable) fixture, and — since #6514 — never overwrites an
existing file it cannot read or parse. The PowerShell helper
`scripts/generate_skeletons.ps1` is now a thin wrapper around this same binary
(single source of truth) and fails loudly if zero rules are parsed.
