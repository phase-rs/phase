### Current-head author addendum — supersedes every earlier body revision

Remedy for review finding **F2** (published contribution record). This addendum describes head
`d33a0513c2e8f6d6bc0a7fc4f8865360486a71c2` against the base CI measured, `e524890ff0f7c9b0b8931976071dd240654c95f1`.
No source changed for this addendum; it reconciles the record with what the branch actually contains.

Everything in the **Historical body** section at the bottom is historical and must not be read as a
description of this head. Specifically void: the `default_silent_dig_rest_to_library` finished-card pass,
"four changed paths", "one function plus three test additions", "unchanged `follow_the_lumarets`
snapshots", and the Gate A line at `2c9125506aa3a0c9a23809d075eabeaff69ea5d7`.

---

## 1. What the change actually is

### 1a. A typed remainder on the look (the issue #7754 fix)

CR 701.20e: looking at cards in a library does not move them. The fix is **not** a card-parse-completion
pass. It is a typed enum carried on the effect and on every pause point that re-encodes it:

- `DigRestRemainder { Route, StayInLibrary }` — `crates/phase-ast/src/types/ability.rs:21551`
  - `Effect::Dig.rest_remainder` (`ability.rs:22480`), `#[serde(default, skip_serializing_if = "DigRestRemainder::is_route")]`
  - `WaitingFor::DigChoice.rest_remainder` — `crates/engine/src/types/game_state.rs`
  - `BatchCompletion::RevealRestPile.rest_remainder` — `game_state.rs`
- The policy is lowered **once, at assembly**, by `declared_selection_remainder`
  (`crates/phase-parser/src/parser/oracle_effect/sequence.rs:7066`). It answers `StayInLibrary` only when the
  clause names a kept destination, keeps a non-zero count, and names **no** remainder destination and **no**
  rest split; everything else is `Route`.
- Three call sites declare it from the same fields: the `DigFromAmong` continuation
  (`sequence.rs:6018`), the `ExileLookedAtCardsFaceDown` continuation (`sequence.rs:6894`), and the "instead"
  alternative construction (`conditions.rs:6195`).
- A later explicit "put the rest …" clause owns the remainder by **re-declaring** `Route` in its existing
  authority, `patch_rest_destination_recursively` (`sequence.rs:7111`). No new rest-handling authority was added.

Runtime consumers: both Dig paths live in `crates/engine/src/game/effects/dig.rs` —
`resolve_from_prior_look` (the interactive `WaitingFor::DigChoice`) and `resolve_mass_put_all` (the mass put).
Each now gates the rest pile on `rest_remainder.is_route()`: a non-route remainder produces an empty rest pile
and no destination, so the unchosen looked-at cards are never routed through
`rest_destination.unwrap_or(Zone::Graveyard)` (`engine_resolution_choices.rs:4977` and `:5052`). The mass path
then calls the new `end_reveal_for_cards_left_in_library` (`engine_resolution_choices.rs:461`) so the reveal
ends with the instruction instead of publishing the positions a later shuffle gives those cards
(CR 701.20a + CR 401.2). The submission arms are in `engine_resolution_choices.rs::handle_resolution_choice`
and `run_batch_completion`. This is what makes Through the Forest Gate keep the unchosen 20 looked-at cards in
the library, shuffle, and gain 8 life.

### 1b. Whole-set selection assembly for "put those cards"

The **same** seam also changed how a bare plural placement after a look/reveal is assembled. "Put those
cards …" is not a remainder of a keep — it names the whole looked-at set, and a zero-keep look never reaches
remainder routing. It therefore lowers to its own instruction over `TargetFilter::TrackedSet` with
`QuantityRef::TrackedSetSize` (`PutAtLibraryPosition`, or `ChangeZoneAll` for a zone move), reading the set
out of the **library** because a look leaves it there. The `PutRest` continuation deliberately excludes this
phrase (`sequence.rs:9150` region); `put_those_cards_on_bottom_is_not_a_dig_remainder` pins the exclusion.

Because two serialized pause payloads gained a policy field, the wire surface is bumped once, by the
registry CLI, in `protocol/changes/7e2fb31c-84f8-4ec2-b323-026d589fcd5d.json` (`surfaces: ["full", "wire"]`).
`Route` is omitted on the wire, so a peer that re-encodes without the field would default to the old
graveyard routing — that is exactly what the bump covers.

## 2. Real path surface

**39 code paths, +2321 / −122** (not four), plus this record file:

| Group | Paths | What it is |
|---|---|---|
| `crates/engine/src` | 13 | `DigRestRemainder` re-exports and exhaustive-match arms (`database/hideaway.rs`, `game/ability_rw.rs`, `game/ability_scan.rs`, `game/mana_abilities.rs`, `game/visibility.rs`), the two Dig paths (`effects/dig.rs`, `engine_resolution_choices.rs`), whole-set readers (`effects/mod.rs`, `effects/choose_card.rs`, `effects/explore.rs`, `effects/reveal_until.rs`, `filter.rs`), state/protocol (`types/game_state.rs`) |
| `crates/engine/tests/integration` | 11 | `through_the_forest_gate.rs` (new, 675 lines) plus 9 existing fixtures updated for the new field/shape and `main.rs` |
| `crates/phase-parser/src` | 7 | `oracle_effect/{sequence,conditions,imperative,lower,mod,tests}.rs`, `follow_the_lumarets_ir.snap` (**changed**, +2) |
| `crates/phase-ai/src` | 5 | tracked-set / determinize readers updated for the new shape |
| `crates/phase-ast/src/types/ability.rs` | 1 | the enum and the `Dig`/pause fields |
| `crates/server-core/src/session.rs` | 1 | pause payload re-encode |
| `protocol/changes/7e2fb31c-….json` | 1 | wire/full-game bump record |
| `.claude/wf/through-the-forest-gate-addendum-r1.md` | 1 | this addendum (documentation only, no source) |

## 3. Measured parse impact — all seven movers

Artifact **13304181933** (`parse-diff`, head `d33a0513`, base `e524890f`): 7 cards / 18 signature clusters,
**0 Oracle-text changes, 0 added or removed cards**. I re-parsed every mover at this head through
`parse_oracle_text` to classify each one. Signature removal alone is not behavior loss: three of the four
"removed" signatures are still in the tree, one level deeper.

| Card | Measured change | Actual current-head parse | Classification |
|---|---|---|---|
| **Through the Forest Gate** | target of the fix | `Dig{count 20, destination Battlefield, keep_count MAX, up_to, filter Land, rest_remainder StayInLibrary, enter_tapped}` → `Shuffle` → `GainLife 8` | intended correction; production test |
| **Covenant of Minds** | `Draw(5)` removed, `unparsed_verb_arguments` added | `RevealTop 3` → `Unimplemented{unparsed_verb_arguments, "choose to put those cards into your hand"}` → `Not(OptionalEffectPerformed)` → `ChangeZoneAll{Exile→Graveyard, ParentTarget}` → **`Draw 5` (still present, depth 3)** | **explicit unsupported change.** The old top-level `Draw(5)` was the conditional "and draw five cards" of "if they don't", advertised unconditionally; the opponent choice was never modeled. The card is now honestly unsupported. No draw was lost — the node still exists. |
| **Explore the Vastlands** | `ChangeZoneAll.from ∅ → library` | `Dig{5, keep_count 0}` → `ChangeZoneAll{origin Library, destination Hand, target TrackedSetFiltered}` → `GainLife 3` | intended correction. CR 701.20e: a "look …" leaves the cards in the library, so a whole-set move must read its origin as the library, not the impulse-exile default it otherwise scans (and moves nothing from). Field correction only; the card stays partially supported because the `and/or` reveal filter and per-player scope are not fully modeled. |
| **Harper Recruiter** | `filter ∅→Cleric`, `keep_count ∅→1`, `reveal`, `up_to`, `to ∅→hand`, `rest_to hand→library` | `Dig{4, destination Hand, keep_count 1, up_to, filter Cleric, rest_destination Library, rest_order random, reveal}` | intended correction. Oracle: reveal Cleric/Rogue/Warrior/Wizard "and put those cards into your hand. Put the rest on the bottom of your library in a random order." The old shape put the **rest into hand** and named no kept destination — a genuine inversion. Residual, stated: the filter captures only the first subtype (`Cleric`) and `keep_count 1` narrows "and/or" to one card. |
| **Munda, Ambush Leader** | `filter ∅→Ally`, `keep_count ∅→MAX`, `to ∅→library`, `reveal`, `up_to` | `Dig{4, destination Library, keep_count MAX, up_to, filter Ally, rest_destination Library, reveal}` | intended correction: kept Ally cards go on top, the rest on the bottom, "any number" is up-to. Pinned by `put_those_cards_with_a_rest_clause_keeps_the_dig_partition`. Residual: the per-pile "in any order" ordering is not represented. |
| **Lim-Dûl's Vault** | `rest_to library→∅` + new `PutAtLibraryPosition{TrackedSetSize, Bottom}`; old `Dig(count=5, keep_count=0)` signature gone | `Dig{5, keep_count 0}` → `Unimplemented{unrecognized_clause_head, "As many times as you choose"}` → `PayCost{PayLife 1}` → **`PutAtLibraryPosition{TrackedSet, TrackedSetSize, Bottom}`** → `Dig{5}` → `Shuffle` → `Unimplemented{"put the last cards you looked at this way on top in any order"}` | intended correction. "Put those cards on the bottom" is the whole looked-at set; folded into `rest_destination` on a zero-keep Dig it was **silently never applied**, because remainder routing does not run for a zero-keep selection. The Dig's own `count`/`keep_count` are unchanged; only the (inert) summary signature moved. Card remains unsupported overall (the repeat loop and the final top placement are typed gaps). No runtime test. |
| **Petals of Insight** | new `PutAtLibraryPosition{TrackedSetSize, Bottom}`; `Bounce(conditional, self)` and old `Dig` signature no longer top-level | `Dig{3, keep_count 0}` → **`PutAtLibraryPosition{TrackedSet, TrackedSetSize, Bottom}`** → **`Bounce{SelfRef}` gated on `EffectOutcome{OptionalEffectPerformed}`** | intended correction; **bounce not lost.** The `Bounce` is still parsed, now at depth 2 behind the whole-set move, gated on "If you do" — so only its top-level signature moved. The production test `put_those_cards_on_the_bottom_moves_every_looked_at_card` asserts the spell returns to its owner's hand, the "otherwise" draw never runs, and all three looked-at cards reach the bottom. Residual, pre-existing: the "Otherwise, draw three cards" branch is not in the chain. |
| **Stomping Slabs** | new `PutAtLibraryPosition{TrackedSetSize, Bottom}`; `DealDamage(7)` no longer top-level | `RevealTop 7` → **`PutAtLibraryPosition{TrackedSet, TrackedSetSize, Bottom}`** → **`DealDamage{Fixed 7, Any}`** | intended correction; **damage not lost.** The 7 damage is still parsed, now at depth 2 behind the whole-set move. Residual, pre-existing and unchanged: "If a card named Stomping Slabs was revealed this way" is still not modeled (the node carries no condition), so the damage is parsed unconditionally. No runtime test. |

The one correction worth repeating: **removed `Bounce`/`DealDamage` summary signatures do not show lost
runtime bounce or damage.** Both nodes are still present at this head; only their depth, and therefore their
top-level summary signature, changed.

## 4. Claim-to-test / support boundary

- Production-path runtime tests: Through the Forest Gate (kept-lands arm, decline-all arm, shuffle and
  `GainLife 8`), silent remainder not moved to the bottom, reorder form, zero-card selection, reveal
  visibility ending, `put_those_cards_on_the_bottom_moves_every_looked_at_card` (Petals of Insight), and the
  graveyard-destination sibling. 675 lines in the new file.
- Parser building-block tests: whole-set lowering for Stomping Slabs and Petals of Insight, the paired
  negative for a rest clause (Munda), the graveyard-origin reader, and the `DigRestRemainder` declaration
  tests.
- **Not claimed as tested:** Lim-Dûl's Vault, Harper Recruiter, Munda, Explore the Vastlands and Covenant of
  Minds have **no** production runtime test. Their entries above describe the measured parse and Oracle text,
  not verified gameplay. Covenant of Minds, Lim-Dûl's Vault, Harper Recruiter (four-subtype / and-or breadth)
  and Explore the Vastlands (and-or breadth, per-player scope) remain unsupported or partial.

## 5. Hosted results at this head

Genuine current-head results, read from GitHub for `d33a0513` (the checks the earlier review saw as pending
have since completed):

- CI run **41790560716** — `success`. Per job: Rust lint (fmt/clippy/parser gate), Rust tests shard 1–5/4,
  Rust tests (build archive), Rust (fmt/clippy/test/coverage-gate), Card data (generate/validate/coverage),
  Frontend, WASM compile check, Lobby worker, Card bot, Android debug APK (ARM64), Draft match proof (real
  WASM).
- AI gate run **41790560457** — `success`. Paired-seed AI gate and Decision-cost perf gate both pass.
- Action Pin Audit and Superagent Security Scan — pass. CodeRabbit, Draft pools, Tauri compile check and the
  nightly drift monitors report `skipping`, not failures.

**No new Gate A PASS is claimed at this head.** The earlier Gate A line named a superseded commit; it is
historical and is not evidence for this contribution. The parser gate, full Rust test suite, card-data
validation and the paired-seed AI gate above are the current evidence.

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
**is** changed (+2), the branch is 39 paths, and the quoted Gate A commit predates the current contribution.

</details>
