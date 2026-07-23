# CR733 resolved-command journal — Run 2 P0 classification report

## Outcome

Run 2 removes the false P0 hard stop. The matrix now maps current direct writes to final P2 factoring seams rather than treating the absence of an already-factored single writer as a blocker. P1 may begin: the narrowed `blocked` residue is empty.

## Classification counts

| Population | Proposed authority | Out-of-closure clone | Derived | Blocked | Other non-reachable |
| --- | ---: | ---: | ---: | ---: | ---: |
| All 256 matrix fields | 227 | 2 | 15 | 0 | 12 |
| Former Run-1 blocker sites | 1,897 | 76 | 0 | 0 | 0 |

Of the 1,897 site records carrying `proposed_authority`, 1,882 are canonical P2 reroutes. The remaining 15 are retained detector collisions: 13 parser-local `constraints`/`def` writes and two presentation `views` writes whose field names happen to match `GameState` fields. The source-read clone provenance list covers every one of the 76 `out_of_closure_clone` sites; two field rows (`battlefield` and `spells_cast_this_game_by_player`) consist exclusively of such sites.

## Ten largest proposed authority seams

| Final seam | P2 reroute sites | Fields |
| --- | ---: | ---: |
| `game::effects::apply_resolution_prompt_transition` (new) | 683 | 53 |
| `game::object_state::apply_resolved_object_edit` (new) | 314 | 1 |
| `game::triggers::apply_resolved_trigger_occurrence` (new) | 208 | 18 |
| `game::turns::apply_resolved_turn_transition` (new) | 170 | 39 |
| `game::ledger::apply_resolved_ledger_edit` (new) | 128 | 50 |
| `game::information::apply_recorded_visibility_edit` (new) | 61 | 5 |
| `game::casting::apply_pending_cast_announcement` (new) | 58 | 1 |
| `game::zone_pipeline::deliver` (promote) | 58 | 16 |
| `game::library::apply_resolved_library_edit` (new) | 42 | 3 |
| `GameState::apply_resolved_player_edit` (new) | 39 | 1 |

The counts are canonical reroute counts, not raw census-hit counts. Clone/probe writes and parser/presentation receiver collisions do not inflate a P2 seam.

## Big-five seam judgments

### `waiting_for`

`waiting_for` has 377 canonical reroute sites, so it cannot stay a collection of unrelated assignments. The P2 seam is `game::effects::apply_resolution_prompt_transition`, which installs or clears the visible prompt at a typed resolution-frame boundary. Plan 04 already makes `ResolutionStack` the paused-work authority through its top-only `push_inner`, `insert_parent_of_active`, `pop_expected`, and `replace_active` APIs, with `resume_resolution_frames` as the resume dispatcher; P2 must delegate there rather than search or reconstruct frames. The only second scope is a turn/game handoff to Priority or GameOver, which is disjoint from an active suspended frame and is recorded separately in the matrix.

### `objects`

`objects` is shared object storage, not a single semantic command family, and its 314 canonical hits cover lifecycle, status, and counter work. The proposed `game::object_state::apply_resolved_object_edit` owns exact non-zone object-incarnation edits, while zone location/incarnation changes delegate to the existing Plan-03 `zone_pipeline::move_object` and `deliver` seams. The matrix names disjoint Zone change, Token/object creation, Object deletion/cleanup, status, and Counter scopes so a command cannot use a whole-map replacement to cross their boundaries. This keeps the final ownership at the object semantic layer while preserving the zone pipeline’s replacement and delivery authority.

### `players`

`players` has 39 canonical container-level hits, but its semantic edits are per-player and must not replace the vector. The proposed `GameState::apply_resolved_player_edit` is the umbrella for exact scalar and player-owned-zone edits, with fields keyed by the affected player or occurrence. Real-pool Mana insert deliberately stays on the already-read `GameState::add_mana_to_pool` seam, which stamps the exact pip ID; Mana spend removes recorded IDs rather than selecting a substitute. The other scopes—scalar deltas, exact zone membership, and per-turn reset—are recorded as disjoint so the player container never becomes a whole-state patch.

### `stack`

`stack` has 25 canonical reroute sites and needs an exact-entry command applier rather than raw deque edits. The new `game::stack::apply_resolved_stack_mutation` will own exact insertion, removal, and replacement by recorded entry identity and position, promoting the existing `push_to_stack` construction path where appropriate. `ResolutionStack` is intentionally not reused for this: Plan 04’s resolution frames represent paused control flow, while `GameState.stack` is the game’s spell-and-ability stack. Trigger context sidecars remain entry-keyed trigger/LKI scopes, preventing an ordinary stack command from re-running trigger matching.

### `pending_cast`

`pending_cast` has 58 canonical reroute sites and is the CR-601 announcement surface this journal is intended to serve. The new `game::casting::apply_pending_cast_announcement` will replace only the exact announced/cost-selection payload at a declared continuation boundary. Mana production/spending and eventual stack insertion remain distinct commands, so replay neither recomputes payment choices nor rebuilds a spell entry from the pending structure. The matrix therefore classifies it as Continuation/prompt state with an announcement-specific scope, rather than conflating it with generic object or stack mutation.

## Source-backed judgment calls

- `smart_shortcut_response`, the analysis resource comparators/projections, casting legality probes, loop-normalization helpers, and projected turn order were opened individually. Each constructs a clone from an immutable state or simulation input and returns only a comparison/probe result, never writing that receiver back to canonical rules state.
- A clone is not automatically outside the closure: `auto_activate_spell_mana_abilities_before_deferred_sacrifice`, scoped-library-search preparation, match restart, and committed shortcut drives all write their constructed state back through `*state = …`; those sites remain canonical reroutes.
- `activated_abilities_this_game` is not blocked: source inspection confirms the existing `restrictions::record_ability_activation` writer. Its per-turn sibling records activation insertion and turn-reset scopes separately, matching the amended disjoint-family rule.
- `complete_logical_zone_trigger_collection`, `mark_logical_zone_events_consumed_before_priority`, `zone_pipeline::move_object`/`deliver`, `GameState::add_mana_to_pool`, `ResolutionStack` top-only APIs, and `resume_resolution_frames` were read and used as the existing seams where their domains match.

## Coverage gate and verification

`cr733_resolved_commands_p0.rs` now rejects the obsolete classification, validates every new matrix classification’s required evidence, and checks the per-site companion’s clone disposition. Its fresh census comparison still requires every write-family field exactly once and fails if a newly reachable write-family field is unmapped. No Cargo build, test, check, clippy, or run command was executed; the binding charter permits only `cargo fmt --all`, which is run before the final commit.

## Next-run requirement

P2 must treat `authority_matrix.json` as a factoring contract: ordinary execution must construct a semantic command and call the recorded seam, while replay must use the exact operands/preconditions and must not recreate selection, entropy, allocation, trigger matching, or prompt state.
