# CR733 resolved-command journal — Run 1 / P0 report

## Outcome

**P0 is blocked by the plan’s hard-stop condition.** The run produced the requested census fixtures and mutation-coverage seed, but it did not fabricate final authorities for fields whose current writes are dispersed. A reviewed authority-factoring revision is required before P1.

## Produced artifacts

- [`authority_matrix.json`](../../crates/engine/tests/fixtures/cr733/authority_matrix.json): 256 written fields exactly once — 15 derived/recomputable, 12 outside the reachable closure, and 229 hard-stop records. The derived records name an existing rebuild entry; blocked records carry their exact reachable site IDs.
- [`rng_allocator_map.json`](../../crates/engine/tests/fixtures/cr733/rng_allocator_map.json): 242 sites (74 RNG → `TypedEntropyReceipt`; 168 allocator → `AllocationReceipt`). 62 RNG and 158 allocator sites are marked reachable.
- [`side_effect_map.json`](../../crates/engine/tests/fixtures/cr733/side_effect_map.json): 752 sites. All 705 event publications are explicitly out of command closure because events are consequences rather than commands; all 47 reveal/visibility candidates are classified as Information boundary receipts. 695 event and 32 information sites are marked reachable.
- [`blocked_write_sites.json`](../../crates/engine/tests/fixtures/cr733/blocked_write_sites.json) and [`RUN1-BLOCKERS.md`](RUN1-BLOCKERS.md): every one of the 1,973 direct-write hard-stop hits.
- [`cr733_resolved_commands_p0.rs`](../../crates/engine/tests/integration/cr733_resolved_commands_p0.rs): regenerates the census with `python3 scripts/cr733_mutation_census.py --json …`, requires every fresh write-family field exactly once, rejects nonexistent fixture fields, and therefore fails for a new unmapped reachable write.

## Evidence

- Base: `e48c7d6e1861d735f1b39aa5fd60d5d79e53b418`.
- Regeneration exactly matched the supplied P0 census: `c6afb7c698f464d2a84c7733ea7f71197877a66d4a29a44b33960f399a3ac376`; 3,362 sites; 8,662 reachable functions; 256 fields.
- The 994 `module::function` references used by the RNG/allocator and side-effect maps were source-verified against their declared Rust function definitions. The derived rebuild entries were separately read in `printed_cards.rs`, `derived.rs`, `layers.rs`, `public_state.rs`, `replacement.rs`, `trigger_index.rs`, and `static_source_index.rs`.
- No production engine game-module external-state writer was found by the targeted filesystem/process/network source scan. Confidence is medium: the census itself intentionally does not resolve macros, traits, or aliases, so this absence is not a proof beyond that review boundary.

## Commits

- `5556a7134ee0833c0f6e1b066f5a4f4407fdf4bf` — `cr733(p0): add mutation authority fixtures`.
- `3699eb5f08a57408e413f30384e6582bc3b9c83d` — `cr733(p0): add mutation coverage gate`.
- This report and the exhaustive blockers inventory are committed in the following documentation commit.

## Verification

- Ran `python3 scripts/cr733_mutation_census.py --json /tmp/cr733-census-current.json` and `--hash`; it exactly matched the supplied baseline.
- Ran `cargo fmt --all` (the only Cargo command allowed by the charter).
- Did not run Cargo build, check, clippy, test, or run. The new Rust test is intentionally left for the authorized validation checkout.
- Repository pre-commit parser gates passed for both implementation commits; they did not compile the engine.

## Judgment calls

- `lki_cache` is **not** classified derived despite the plan’s illustrative examples. The inspected code treats it as a broad live LKI authority, and no post-command rebuild entry exists that can recreate historical snapshots after an object leaves. It remains a blocker until a resolved zone/LKI command owns that information.
- `GameEvent` emissions are not proposed as commands. The plan explicitly makes them consequence/publication records; trigger/LKI state belongs in semantic state commands, which is why its current dispersed fields remain blockers.
- `layers_dirty`, indexes, database hydration, display derivation, and public-state dirtiness are classified derived only where an existing rebuild entry was read. This is a conservative exception, not a blanket treatment for transient state.

## Confidence and self-challenge

Confirmed: the P0 census, count/hash, direct-write concentration, fixture coverage, and source existence checks. Confidence is high that P1 is unsafe without an authority-factoring revision. A finding that could change this is a precise call/alias analysis proving a listed field is outside the actual mana/cost/replacement closure, or a pre-existing single semantic authority hidden behind a path the name-based census cannot identify. Such a result must update the generator or the blocked record with source evidence; it is not a license to assign an authority by convention.
