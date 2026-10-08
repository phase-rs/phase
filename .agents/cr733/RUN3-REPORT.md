# CR733 resolved-command journal — Run 3 P0 v3 reconciliation report

## Outcome

P0 now mirrors the corrected v3 census and has no hard-stop residue. P1 may begin.

Evidence: the supplied v3 census is hash `c7791dddbbdd786e4b76e63432a6681c55b4d2f124723cc14b3bf4018e75a87e` at `a60993aeed`; a fresh run at the adopted `b7109c22c7` produced the same 3,593 records and content after excluding only the generator's `head_commit` field. The current coverage gate regenerates the census and requires exact site-for-site equality for writes, entropy/allocation, and side effects.

## Classification counts

| Population | Proposed authority | Derived | Out-of-closure clone | Blocked | Outside reachable closure |
| --- | ---: | ---: | ---: | ---: | ---: |
| 256 matrix fields | 234 | 15 | 0 | 0 | 7 |
| 2,590 write sites | 2,412 | 42 | 127 | 0 | 9 |

The seven field / nine-site outside-closure records are unchanged detector boundary artifacts (`debug_mode`, `format_config`, `log_player_names`, `max_lands_per_turn`, `rng_seed`, `rng_word_pos`, and `seat_order`); none is reachable from the P0 roots. They are retained transparently rather than forced into a command family.

There are 1,949 canonical P2 reroute sites. The rest are derived rebuilds, source-verified discarded clones/projections, non-`GameState` receiver collisions, or existing final-authority implementation sites. In particular, 47 `filter_state_for_viewer` writes are source-verified DTO projection mutations and 47 `zones.rs` writes are source-verified final Zone-authority implementation steps; neither set is a P2 bypass.

## Field classification flips

| Field | Run 2 → Run 3 | Enclosing-function evidence |
| --- | --- | --- |
| `battlefield` | clone-only → Zone-change proposed authority | `zones::remove_from_zone` retains the exact object at line 1426; `zones::add_to_zone` pushes it at line 1493. Both are the Plan-03 authority's internal delivery steps. |
| `spells_cast_this_game_by_player` | clone-only → Ledger proposed authority | `restrictions::record_spell_cast_from_zone` constructs the CR 117.1 cast record and appends its game-lifetime entry at line 276. |
| `assassin_or_commander_dealt_combat_damage_this_turn` | outside reachable closure → Ledger proposed authority | `triggers::collect_pending_triggers_with_collection` inserts the player fact at line 4039; `turns::start_next_turn` clears the turn scope at line 1025. |
| `commander_damage` | outside reachable closure → Ledger proposed authority | `combat_damage::apply_combat_damage` updates the keyed entry at line 1061 or appends it at line 1068. |
| `exile` | outside reachable closure → Zone-change proposed authority | `zones::remove_from_zone` retains at line 1431 and `zones::add_to_zone` pushes at line 1495. |
| `may_trigger_auto_choices` | outside reachable closure → Continuation/prompt proposed authority | `GameState::set_may_trigger_auto_choice` updates a matching key at line 15591 or inserts at line 15598; exact-key removal and actor-scoped clear are adjacent methods. |
| `pending_trigger_abandons` | outside reachable closure → Trigger/LKI proposed authority | `triggers::abandon_ceased_pending_trigger` appends the exact recovery fact at line 6010. |

The corrected zone census also added canonical sites for players, objects, command-zone/exile placement, LKI caches, trigger-index maintenance, continuous effects, mana-tap facts, pending ETB counters, zone-change records, deck pools, stack-paid facts, and object allocation. I read each added enclosing function. The matrix maps zone-owned cleanup/delivery writes to Zone change or Object deletion/cleanup with `reroute_required=false` when the site is already `zones.rs`/`zone_pipeline.rs`; direct caller writes retain their reviewed P2 seams.

## Receipt and side-effect maps

- `rng_allocator_map.json` now maps all 75 RNG and 171 allocator sites. Newly visible examples are the modal/selection RNG chains, `zones::random_top_slot_index`, `zones::create_object` allocation, zone-move allocation, and resolution-frame ID high-water sites. Each maps to `TypedEntropyReceipt` or `AllocationReceipt` and records the consuming authority.
- `side_effect_map.json` now maps all 710 event publications and 47 information sites. The five added event records are `engine::apply_action` and the `move_to_zone` / `move_to_library_at_index` ZoneChanged/Unattached publications; they remain consequences, not state-mutating commands.

## Coverage gate and verification

`cr733_resolved_commands_p0.rs` still regenerates the census with the read-only Python generator. It now pins the v3 family/site counts and rejects any difference in the full write-site, RNG/allocator, or event/information site multisets. It continues to require exactly one matrix row for every written `GameState` field and validates clone provenance.

Validation performed without Cargo build/test/check/clippy/run:

- Fresh census at `b7109c22c7`: 3,593 sites; write 2,590; RNG 75; allocator 171; event emission 710; information 47; 8,707 reachable functions.
- Exact multiset comparison: 2,590 write fixture records, 246 receipt records, and 757 side-effect records; no missing or unexpected v3 sites.
- Parsed all four JSON fixtures with `jq` and ran `git diff --check`.

`cargo fmt --all` is run immediately before the final commit, as the charter permits. No other Cargo command is used.

## P1 decision and next-run fact

P1 may begin because the narrowed hard-stop residue is empty.

The most important P2 fact is that `zones.rs` is not a raw-write bypass: its newly visible writes are the final Zone authority's own cleanup/delivery implementation. Factor callers to construct exact Zone commands and preserve those implementation sites as non-rerouted authority internals.
