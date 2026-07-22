# cr-suite

Executable **Comprehensive Rules** scenario suite for phase.rs.

Annotation tracking (`cargo rules-audit`) records which CR numbers appear in
comments. This crate turns those citations into **enforceable contracts**:

1. Parse `docs/MagicCompRules.txt`
2. Emit per-rule TOML fixtures under `scenarios/<section>/`
3. Promote fixtures to `status = "executable"` with typed setup/steps/assertions
4. Run them through `GameScenario` / `GameRunner` (engine APIs only)

Tracked in https://github.com/phase-rs/phase/issues/6343.

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

1. Find the skeleton under `scenarios/<section>/cr_<rule>.toml`
2. Set `status = "executable"`
3. Fill `[setup]`, `[[steps]]`, `[[assertions]]` using kinds from `predicates.rs`
4. Run `cargo cr-suite --run --rule <N>`

Do **not** duplicate game logic in assertions — only read engine state.
