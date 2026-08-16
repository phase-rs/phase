# CR733 resolved-command journal — Run 4 P1 report

## Outcome

P1 landed in `9c6f08de1c` (`cr733(p1): record mana provenance journal`). It is provenance-only: the existing reducer and payment paths remain the behavior authority; no P2 command applier, selective reconstruction, P0 fixture, or authority-matrix seam changed.

Evidence (high confidence): source review traced the real production authority (`GameState::add_mana_to_pool`), all non-simulation `pay_cost_with_demand_and_choices` funnels, explicit/basic-land mana activation, and inline triggered-mana resolution. The runtime integration test drives `GameAction::ActivateAbility` on a real Dimir Signet ability paid by an auto-tapped basic land.

## Landed P1 surface

- `types/resolved_commands.rs` supplies checked `ResolvedCommandOrdinal` and `SettlementNodeOrdinal`, typed `RulesExecutionNodeRef`, `SettlementNode`, exact produced/spent `ManaUnit` records, recipient identity, and a validated append-only journal.
- Each activated mana ability receives a node, including the intrinsic basic-land fallback. Nested activations are caused by the active parent. Inline triggered mana receives a distinct node with `TriggerDefinitionRef`, event-derived cause, and `bundle_parent`; an explicit event cause intentionally wins over an ambient outer scope, so a trigger from a nested producer stays with that producer.
- `add_mana_to_pool` returns the stamped exact unit and records it under the active node (or an automatic Proposal node). Production helpers return the exact inserted units. Existing pool-only seed/debug call sites explicitly discard the new `Option` result.
- The existing real spend funnels now record exact solver-returned `ManaUnit`s in consumption order: cast payment, non-cast payment, nested auto-tap payment, and a mana ability's own mana sub-cost. Records retain the stamped pip, producing node, original restrictions/grants, payment node, and exact object-or-player recipient.
- `GameState` carries a serde-default journal, includes it deliberately in manual equality, clears historical provenance when normalizing a mandatory-loop position, and redacts the journal from all viewer projections. Old/restored pool units are backfilled to automatic Proposal producers when payment restamps their pip identities.
- Inline tests cover checked ordinal monotonicity/uniqueness/overflow, trigger bundle causality, exact produced/spent conservation including restrictions, and journal serde round-trip plus duplicate/nonmonotonic ordinal rejection. The integration test covers a real nested activation and opponent-view redaction.

## Judgment calls and intentional deviations from §5

- The P1 journal stores ordered `Vec`s rather than the §5 illustrative `BTreeMap`. Ordinals are contiguous append indexes, serialization rejects duplicate/nonmonotonic records, and no map/key iteration supplies application order. This is smaller and preserves wire arrival order for validation; P2 can extend entries with semantic commands without changing the ordering contract.
- `SettlementNode` is one canonical ordered node vector. Proposal/PlayerLeave refs retain their typed command-ordinal identity while every node also carries its independent `SettlementNodeOrdinal`, as §5 permits.
- `bundle_parent` is an added metadata field. It is necessary to express “distinct triggered node, parent-selected bundle” without conflating causal identity and selection ownership.
- P1 creates an ordered journal slot when each node begins. These are identity/order records only, not P2 semantic command application; a node's command list is deliberately not reconstructed or replayed yet.
- `GameState::add_mana_to_pool` changed from `()` to `Option<ManaUnit>` to preserve its existing no-op behavior for an unknown player while exposing the exact inserted unit. This caused the small `let _ =` signature ripple at legacy callers; no caller logic changed.

## Verification

- Starting charter state was confirmed: clean `ff7fc1781b5aca2181da66cb84d7b7be3135738a` on `cr733/resolved-commands`.
- CR annotations were rechecked against `/Users/matt/dev/forge.rs/docs/MagicCompRules.txt`: CR 104.4b, 106.4, 118.3a, 605.3b, and 605.4a.
- `cargo fmt --all` and `git diff --check` passed immediately before the implementation commit. The commit's parser-combinator pre-commit gate passed.
- No Cargo build, check, clippy, or test command was run, per the charter. `./scripts/tilt-wait.sh clippy test-engine` returned `3`: Tilt watches `/Users/matt/dev/forge.rs/crates`, outside this checkout, so its green state cannot verify this worktree.

## Continuation boundary

Remaining work is P2 and later only: add final semantic command payloads/appliers to the P1 slots, use the P0 authority seams, then build dependency-closed selective reconstruction. The most important next-run fact is that exact spent-pip dependencies are already available on `Payment` nodes; P2 must consume those recorded identities and must not re-solve payment or re-enter the ordinary effect dispatcher.
