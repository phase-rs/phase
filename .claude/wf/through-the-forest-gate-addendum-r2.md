### Current-head author addendum r2 — supersedes addendum r1 and every earlier body revision

Remedy for review finding **F2** (published contribution record), now covering the merged head.
This file is committed **inside the merge commit** it describes, so it cannot name its own SHA.
Identity of that head:

- merge commit carrying this file: `f0be2a81210e92221ef161ea709a124eac1c5313`
- first parent (the branch head the review examined): `d33a0513c2e8f6d6bc0a7fc4f8865360486a71c2`
- second parent (`origin/main` at merge time): `4b6808c7c9a5c81b6c0c7cedab5f1b24734ca81a`
- r1 (`through-the-forest-gate-addendum-r1.md`) is historical: it describes `d33a0513` only.
  Any commit after the merge is documentation-only and leaves every claim above unchanged.

Everything in the **Historical body** section at the bottom is historical and must not be read as a
description of this head. Specifically void: the `default_silent_dig_rest_to_library` finished-card
pass, "four changed paths", "one function plus three test additions", "unchanged
`follow_the_lumarets` snapshots", and the Gate A line at
`2c9125506aa3a0c9a23809d075eabeaff69ea5d7`.

---

## 0. What the merge changed (and what it did not)

`origin/main` independently landed the same class of fix for the private-look form
(`bind_those_cards_bottom_to_prior_look`, `crates/phase-parser/src/parser/oracle_effect/assembly.rs`,
admitted only for whole-set noun phrases by `library_placement_puts_whole_noun_phrase_set`). Three
parser files conflicted and were resolved keeping both intents; no source was reverted:

| Conflict | Resolution |
|---|---|
| `oracle_effect/mod.rs` (`refine_put_at_library_position`) | One `Unstated` arm now carries **both** policies: our whole-set `TrackedSetSize` cardinality for a plural set anaphor **and** main's `arranger = follower` rule for a tracked set placed "in any order". |
| `oracle_effect/sequence.rs` (`Effect::Dig` → `PutRest`) | Kept main's `!you may` guard (an optional placement is its own instruction, CR 608.2c) **and** our exclusion of `"put those cards"` (a whole-set move is not a remainder). |
| `oracle_effect/tests.rs` | Kept both test sets; the whole-set test now pins both authorities (see §3, Petals of Insight). |
| `assembly.rs` (auto-merged) | Main's new `Effect::Dig` literal needed the typed field: it constructs `rest_remainder: DigRestRemainder::Route` (the pre-existing routing behaviour), so no `Effect::Dig` exists without the policy. |

The protocol record (`protocol/changes/7e2fb31c-….json`, `surfaces: ["full","wire"]`) is unchanged by
the merge.

## 1. What the change actually is

### 1a. A typed remainder on the look (the issue #7754 fix)

CR 701.20e: looking at cards in a library does not move them. The fix is **not** a card-parse-completion
pass. It is a typed enum carried on the effect and on every pause point that re-encodes it:

- `DigRestRemainder { Route, StayInLibrary }` — `crates/phase-ast/src/types/ability.rs`
  - `Effect::Dig.rest_remainder`, `#[serde(default, skip_serializing_if = "DigRestRemainder::is_route")]`
  - `WaitingFor::DigChoice.rest_remainder` — `crates/engine/src/types/game_state.rs`
  - `BatchCompletion::RevealRestPile.rest_remainder` — `game_state.rs`
- The policy is lowered **once, at assembly**, by `declared_selection_remainder`
  (`crates/phase-parser/src/parser/oracle_effect/sequence.rs`). It answers `StayInLibrary` only when the
  clause names a kept destination, keeps a non-zero count, and names **no** remainder destination and
  **no** rest split; everything else is `Route`.
- Three call sites declare it from the same fields: the `DigFromAmong` continuation, the
  `ExileLookedAtCardsFaceDown` continuation, and the "instead" alternative construction
  (`conditions.rs`).
- A later explicit "put the rest …" clause owns the remainder by **re-declaring** `Route` in its existing
  authority, `patch_rest_destination_recursively` (`sequence.rs`).

Runtime consumers: both Dig paths live in `crates/engine/src/game/effects/dig.rs` —
`resolve_from_prior_look` (the interactive `WaitingFor::DigChoice`) and `resolve_mass_put_all` (the mass
put). Each gates the rest pile on `rest_remainder.is_route()`: a non-route remainder produces an empty
rest pile and no destination, so the unchosen looked-at cards are never routed through
`rest_destination.unwrap_or(Zone::Graveyard)` (`engine_resolution_choices.rs`). The mass path calls the
new `end_reveal_for_cards_left_in_library` so the reveal ends with the instruction instead of publishing
the positions a later shuffle gives those cards (CR 701.20a + CR 401.2). This is what makes Through the
Forest Gate keep the unchosen looked-at cards in the library, shuffle, and gain 8 life.

### 1b. Whole-set selection assembly for "put those cards"

"Put those cards …" is not a remainder of a keep — it names the whole looked-at set, and a zero-keep look
never reaches remainder routing. Two authorities now place that set, chosen by what the look published:

- **public reveal / non-bottom destination** (our lowering): its own instruction over
  `TargetFilter::TrackedSet` with `QuantityRef::TrackedSetSize` (`PutAtLibraryPosition`, or
  `ChangeZoneAll` for a zone move), reading the set out of the **library** because a look leaves it
  there (Stomping Slabs; the bare-look graveyard reader).
- **bare private look** (main's assembly authority): re-anchored to a `DigSource::PriorLook` Dig whose
  `rest_destination` is the look ledger (Petals of Insight, Lim-Dûl's Vault class).

The `PutRest` continuation deliberately excludes the phrase (plus main's `you may` guard), pinned by
`put_those_cards_on_bottom_is_not_a_dig_remainder`.

Because two serialized pause payloads gained a policy field, the wire surface is bumped once, by the
registry CLI, in `protocol/changes/7e2fb31c-84f8-4ec2-b323-026d589fcd5d.json` (`surfaces: ["full", "wire"]`).
`Route` is omitted on the wire, so a peer that re-encodes without the field would default to the old
graveyard routing — that is exactly what the bump covers.

## 2. Real path surface at this head

**42 paths, +2.5k / −128** (not four): 41 contribution paths plus this r2 record.

| Group | Paths | What it is |
|---|---|---|
| `crates/engine/src` | 13 | `DigRestRemainder` re-exports and exhaustive-match arms (`database/hideaway.rs`, `game/ability_rw.rs`, `game/ability_scan.rs`, `game/mana_abilities.rs`, `game/visibility.rs`), the two Dig paths (`effects/dig.rs`, `engine_resolution_choices.rs`), whole-set readers (`effects/mod.rs`, `effects/choose_card.rs`, `effects/explore.rs`, `effects/reveal_until.rs`, `filter.rs`), state/protocol (`types/game_state.rs`) |
| `crates/engine/tests/integration` | 11 | `through_the_forest_gate.rs` (new, 675 lines / 8 tests) plus 9 existing fixtures updated for the new field/shape and `main.rs` |
| `crates/phase-parser/src` | 8 | `oracle_effect/{assembly,sequence,conditions,imperative,lower,mod,tests}.rs` and `follow_the_lumarets_ir.snap` (**changed**, +2) |
| `crates/phase-ai/src` | 5 | tracked-set / determinize readers updated for the new shape |
| `crates/phase-ast/src/types/ability.rs` | 1 | the enum and the `Dig`/pause fields |
| `crates/server-core/src/session.rs` | 1 | pause payload re-encode |
| `protocol/changes/7e2fb31c-….json` | 1 | wire/full-game bump record |
| `.claude/wf/through-the-forest-gate-addendum-r{1,2}.md` | 2 | the record (documentation only, no source) |

## 3. Measured parse impact — all seven movers, re-verified at this head

Artifact **13304181933** measured 7 cards / 18 signature clusters at `d33a0513` (0 Oracle-text changes,
0 added/removed cards). I re-parsed every mover through `parse_oracle_text` at **this** head. Six are
unchanged; one moved because of the merge. Removed summary signatures alone do not show lost behaviour:
the deeper nodes are still in the tree.

| Card | Change vs base | Parse at this head | Classification |
|---|---|---|---|
| **Through the Forest Gate** | target of the fix | `Dig{20, Battlefield, keep MAX, up_to, Land, rest_remainder StayInLibrary, tapped}` → `Shuffle` → `GainLife 8` | intended correction; 8 production tests |
| **Covenant of Minds** | `Draw(5)` top-level signature replaced by an unsupported marker | `RevealTop 3` → `Unimplemented{unparsed_verb_arguments, "choose to put those cards into your hand"}` → `Not(OptionalEffectPerformed)` → `ChangeZoneAll{Exile→Graveyard, ParentTarget}` → **`Draw 5` (still present, depth 3)** | **explicit unsupported change.** The old top-level `Draw(5)` advertised the "if they don't" branch unconditionally; the opponent choice was never modelled. No draw was lost. |
| **Explore the Vastlands** | `ChangeZoneAll.from ∅ → library` | `Dig{5, keep_count 0}` → `ChangeZoneAll{origin Library, destination Hand, target TrackedSetFiltered}` → `GainLife 3` | intended correction (CR 701.20e: a "look…" leaves cards in the library, so a whole-set move reads the library origin). Residual: `and/or` reveal breadth and per-player scope unmodelled. |
| **Harper Recruiter** | filter/count/reveal/destinations change | `Dig{4, Hand, keep 1, up_to, Cleric, rest Library random, reveal}` | intended correction of an inversion (old shape put the **rest into hand**). Residual: only the first subtype is captured; "and/or" narrows to one card. |
| **Munda, Ambush Leader** | filter/count/reveal/up-to change | `Dig{4, Library, keep MAX, up_to, Ally, rest Library player_choice, reveal}` | intended correction (kept Allies on top, rest on the bottom). Pinned by `put_those_cards_with_a_rest_clause_keeps_the_dig_partition`. Residual: per-pile "in any order" ordering not represented. |
| **Lim-Dûl's Vault** | `rest_to library→∅` + new whole-set bottom placement | `Dig{5, keep 0}` → `Unimplemented{"As many times as you choose"}` → `PayCost{PayLife 1}` → `PutAtLibraryPosition{TrackedSet, TrackedSetSize, Bottom}` → `Dig{5}` → `Shuffle` → `Unimplemented{"put the last cards … on top in any order"}` | intended correction; measured unchanged by the merge (main's rebind does **not** fire here — the intervening `PayCost`/`Unimplemented` clauses are not lookback-transparent). Card remains unsupported overall. No runtime test. |
| **Petals of Insight** | new whole-set bottom placement; `Bounce(conditional, self)` no longer top-level | `Dig{3, keep 0}` → **`Dig{source PriorLook, keep 0, rest_destination Library, rest_order player_choice}`** → **`Bounce{SelfRef}` gated on `EffectOutcome{OptionalEffectPerformed}`** | intended correction; **bounce not lost.** **This is the one mover whose shape the merge changed:** main's `bind_those_cards_bottom_to_prior_look` now re-anchors the placement to the private-look ledger (the r1 record described the pre-merge `PutAtLibraryPosition` form). The 5 `petals_of_insight` production tests pass at this head. Residual, pre-existing: the "Otherwise, draw three cards" branch is not in the chain. |
| **Stomping Slabs** | new whole-set bottom placement; `DealDamage(7)` no longer top-level | `RevealTop 7` → **`PutAtLibraryPosition{TrackedSet, TrackedSetSize, Bottom}`** → **`DealDamage{Fixed 7, Any}`** | intended correction; **damage not lost** (still parsed, depth 2). Residual, pre-existing: "If a card named Stomping Slabs was revealed this way" is still not modelled. No runtime test. |

## 4. Claim-to-test / support boundary

- Production-path runtime tests (merged head): Through the Forest Gate (kept-lands arm, decline-all arm,
  shuffle and `GainLife 8`), silent remainder not moved to the bottom, reorder form, zero-card selection,
  reveal visibility ending, graveyard-destination sibling (`through_the_forest_gate.rs`, 8 tests), and
  `petals_of_insight.rs` (5 tests: accept/bounce, decline/draw-and-graveyard, single-card no-prompt,
  opponent visibility, AST shape).
- Parser building-block tests: whole-set lowering for Stomping Slabs, the private-look rebind for
  Petals of Insight, the paired negative for a rest clause (Munda), the graveyard-origin reader, the
  `PutRest` exclusion, and the `DigRestRemainder` declaration tests (alternatives, standalone entry
  point, reorder form, Hideaway selection).
- **Not claimed as tested:** Lim-Dûl's Vault, Harper Recruiter, Munda, Explore the Vastlands and
  Covenant of Minds have **no** production runtime test. Their rows describe measured parses and Oracle
  text, not verified gameplay.

## 5. Verification at this head — local vs hosted

**Hosted results for the merged head: none yet.** The commit has not been pushed, so no hosted run
exists for it. The d33a0513 runs (CI `41790560716`, AI `41790560457`, both `success`) are **historical**
evidence for the pre-merge parser/engine shape and are not evidence for this merge. No new Gate A PASS is
fabricated; the gate was run locally and its exact line is recorded below.

Local checks on this working tree, in this order:

- `cargo fmt --all` — clean.
- `./scripts/check-parser-combinators.sh` — `Gate A PASS head=f0be2a81210e92221ef161ea709a124eac1c5313
  base=4b6808c7c9a5c81b6c0c7cedab5f1b24734ca81a`, re-run on the committed merge head (the pre-commit
  run reported the previous commit).
- `cargo test -p phase-parser --lib` — 12226 passed, 0 failed, 3 ignored.
- `cargo test -p phase-engine --test integration` — 2811 passed, 0 failed, 3 ignored (532 s).
- `cargo clippy -p phase-parser --lib -- -D warnings` — passed (exit 0, 3 m 11 s), covering the
  substantive Rust change. The workspace-wide `--all-targets` clippy over `phase-engine`, `phase-ai` and
  `server-core` did **not** finish within this environment's per-command time limit, so it is **not**
  claimed here; CI's clippy job is the authority for those crates.

## 6. Review-finding disposition

- **F1 (resolved, preserved):** global and standalone finished-tree repair passes remain removed; the
  selection/continuation paths use `declared_selection_remainder`, alternative construction reuses it,
  and explicit rest handling stays in `patch_rest_destination_recursively`. The merge preserved this:
  main's rebind only re-anchors the whole-set placement; it never restores a finished-card pass.
- **F2 (this record):** typed remainder/selection-assembly and engine scope (§1), real path surface
  (§2), all seven movers with claim-to-test boundaries (§3–§4), historical vs current verification
  (§5), old Gate A marked historical (§ preamble).
- **Inline bot comments:** all five marked "Addressed" in the review are addressed in commits
  `45fe44e`–`7eb997d`. The last open one (bare "put those cards" after a look lowered as a whole-set
  move with the tracked set's full cardinality) is satisfied by our lowering plus, after the merge,
  main's assembly authority; the merged shapes are pinned by the tests in §4.

---

## Historical body (superseded — kept only for the review trail)

<details>
<summary>Earlier revision, describing a `default_silent_dig_rest_to_library` pass and four paths</summary>

CR 701.20e: looking at cards in a library never moves them. "Look at the top twenty cards of your library, put
any number of land cards from among them onto the battlefield tapped, then shuffle. You gain 8 life." therefore
moves only the chosen lands — the other looked-at cards stay in the library, and the printed "then shuffle"
randomizes the whole library afterwards.

That revision described a finished-card pass (`default_silent_dig_rest_to_library`) applied at the end of
`parse_oracle_pipeline`, four changed paths (`parser/oracle.rs`, `oracle_effect/tests.rs`,
`tests/integration/through_the_forest_gate.rs`, `tests/integration/main.rs`), "one function plus three test
additions", unchanged `follow_the_lumarets` snapshots, and `Gate A PASS head=2c9125506aa3a0c9a23809d075eabeaff69ea5d7`.

**All of that is superseded and inaccurate for this head.** The global and standalone finished-tree repair
passes were removed; the remainder is declared once, at assembly, by `declared_selection_remainder`, and the
existing `patch_rest_destination_recursively` authority keeps explicit rest clauses. `follow_the_lumarets_ir.snap`
**is** changed (+2), the branch is 42 paths, and the quoted Gate A commit predates the current contribution.

</details>
