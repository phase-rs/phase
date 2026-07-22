# cr-suite

Executable **Comprehensive Rules** scenario suite for phase.rs.

Annotation tracking (`cargo rules-audit`) records which CR numbers appear in
comments. This crate turns those citations into **enforceable contracts**:

1. Parse `docs/MagicCompRules.txt`
2. Emit per-rule TOML fixtures under `scenarios/<section>/` (generated; not committed en masse)
3. Promote fixtures to `status = "executable"` with typed setup/steps/assertions
4. Run them through `GameScenario` / `GameRunner` (engine APIs only)

Tracked in https://github.com/phase-rs/phase/issues/6343.

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
