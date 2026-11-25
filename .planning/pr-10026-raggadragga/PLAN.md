# PR phase-rs/phase#10026 — Raggadragga, Goreguts Boss: building-block plan

Branch: `rebase-implement-pr-10026-fix` (on upstream/main 588a37340).
Prior step: RED-FIRST coverage committed as 60d8da8fd (parser unit tests +
`crates/engine/tests/integration/raggadragga_goreguts_boss.rs`, 1 RED / 3 green).
This plan implements the fix. Building-block only; no card-specific hacks.

## Step 0: premise (verified from fixture, not memory)

`crates/engine/tests/fixtures/integration_cards/r.jsonl`, key
`raggadragga, goreguts boss`, `oracle_text` verbatim:

> Each creature you control with a mana ability gets +2/+2.
> Whenever a creature you control with a mana ability attacks, untap it.
> Whenever you cast a spell, if at least seven mana was spent to cast it, untap
> target creature. It gets +7/+7 and gains trample until end of turn.

Fixture rulings (all three, governing design notes in brackets):

1. "Most mana abilities found on creatures are activated mana abilities. An
   activated mana ability is an activated ability that could produce mana as it
   resolves, does not require a target, and isn't a loyalty ability."
   [Predicate reads the live ability set via the existing classifier; no new
   mana-ability definition.]
2. "Raggadragga's last ability can target any creature, including one that's
   untapped." [Clause 3 target is untyped `Typed Creature`, already green.]
3. "Some creatures may have triggered mana abilities. A triggered mana ability
   is a triggered ability that could produce mana as it resolves, triggers on
   the resolution of an activated mana ability or on mana being added, and
   doesn't require a target." [Again the classifier's job, already wired.]

Clause verdicts from step 1 (re-verified by direct reads for this plan):

- Clause 1 (anthem): SUPPORTED, green. No work.
- Clause 2 (attack trigger): GAP. Single root cause below. This plan fixes it.
- Clause 3 (seven-mana spell trigger): SUPPORTED, green end-to-end
  (`ManaSpentToCast{TriggeringSpell,Total} GE 7` parses; cast stamping,
  SpellCast intervening-if routing, and metric read all verified present).
  The task brief's expected second defect (mana-spent Total metric evaluation)
  does not exist. No work; the committed green tests guard it.

## Confirmed defect (single root cause)

`parse_type_phrase_folding_unbound`
(`crates/phase-parser/src/parser/oracle_target.rs:3464`) — the shared
type-phrase suffix scanner every trigger subject flows through — has arms for
mana-value, base-P/T, power, superlative, counter, "with no abilities"
(:4482-4494), and keyword suffixes, but NO "with a mana ability" arm.

Failure chain, all read directly:

1. `parse_single_subject` (`oracle_trigger.rs:15851`, the "a "/"an " arm) calls
   `parse_type_phrase_folding_with_ctx` on `creature you control with a mana
   ability attacks, untap it.` The type phrase consumes through `you control`
   and stops; rest = ` with a mana ability attacks, ...`.
2. `parse_keyword_suffix` (:9483) cannot claim the rest: it needs a leading
   keyword match, and `a mana ability` is not one — returns `None`, rest intact.
3. `try_parse_event`'s Attacks arm (:17178) requires rest to start with
   `attacks`/`attack`; rest starts with ` with ...` → falls through to
   `TriggerMode::Unknown` (the stale fixture shape).

The only "with a mana ability" arms in the tree are context-local and already
green: the static-subject trailing-strip (`oracle_static/shared.rs:5547`) and
the library-search loop (`oracle_effect/search.rs:2623`).

Runtime needs NO changes — chain verified end to end by reading:
`attack_firings` → `attack_declaration_attacker_matches`
(`trigger_matchers.rs:2715`, consults `valid_card` when present) →
`valid_card_matches` (:761) → `target_filter_matches_object` →
`FilterProp::HasManaAbility => obj.abilities.iter().any(is_mana_ability)`
(`filter.rs:11692`). Once the subject parses to
`Typed(Creature, You, [HasManaAbility])` with mode `Attacks`, the committed RED
runtime test goes green with zero engine edits.

## Step 0.5: class enumeration (fixture-scoped; AtomicCards absent)

Task constraint: MTGJSON `AtomicCards.json` is absent and full oracle-gen is
forbidden, so the skill's canonical census query cannot run. Census over
committed integration fixtures instead (record:
`.planning/pr-10026-raggadragga/class-fixtures.json`):

- `with a mana ability` as an object qualifier in `oracle_text`: 1 card —
  Raggadragga, Goreguts Boss. (Two other raw-text hits are rulings prose, not
  Oracle text: Chandra's Regulator, Spiders-Man.)
- Sibling ability-qualifier phrases proving the general pattern:
  `with no abilities` → Jasmine Boreal of the Seven, Muraganda Petroglyphs,
  Ruxa, Patient Professor; `with a cycling ability` → Spellpyre Phoenix;
  `with an exhaust ability` → Sala, Deck Boss.

The fix is still class-shaped, not card-shaped: it completes the shared
type-phrase "with X" qualifier authority (which already owns power, mana value,
counters, keywords, and "no abilities") so EVERY current and future
"with a mana ability" subject — trigger subjects, targets, search filters —
parses through one grammar. The skill's "if the answer is 1, stop" gate is
satisfied by the sibling qualifier class sharing the identical seam.

## Step 1: applicable skills

- `/oracle-parser` (authoritative parser reference) — read; checklists drive
  steps 1-3 below.
- `/add-trigger` — read; the attack trigger's subject/matcher wiring follows its
  subject-filter checklist (no trigger-file change needed; verified).
- `/card-test` — read; already applied by the committed RED tests (production
  entry `GameRunner::cast`/`declare_attackers`, paired reach-guards).
- `/add-engine-variant` — NOT triggered: no new enum variant; `HasManaAbility`
  exists in `FilterProp` with full eval coverage.
- `/add-engine-effect`, `/add-static-ability`, `/add-interactive-effect`,
  `/add-card-data-pipeline` — not applicable (no new effect, static already
  green, no choices, no export-shape change).

## Step 2/3: analogous trace + files read

Traced feature: the sibling `HasNoAbilities` qualifier ("with no abilities",
Ruxa / Muraganda Petroglyphs) through the exact seam this plan extends:

`oracle_nom/filter.rs::parse_no_abilities` (:184, bare-predicate authority) →
`oracle_target.rs:4482` ("with " lead + offset arm in the shared type-phrase
scan) → `parse_single_subject` (:15851, trigger subjects) →
`try_parse_event` Attacks arm (:17178, rest now starts with `attacks`) →
`attack_firings` → `attack_declaration_attacker_matches` (:2715) →
`valid_card_matches` (:761) → `filter.rs` eval. Plus the two local mirrors:
`search.rs:2630` delegation and `oracle_static/shared.rs:5547` trailing-strip.

Files read for this plan (body, not grep-only): `oracle_target.rs` §§2998,
3417-3566, 4380-4505, 9483; `oracle_nom/filter.rs` §§150-230, 990-1015;
`oracle_trigger.rs` §§15430-15548, 15549-15915, 16861-16910, 17153-17240;
`oracle_static/shared.rs` §§5533-5572; `search.rs` §§2615-2640, 3542-3556;
`trigger_matchers.rs` §§759-771, 2418-2426, 2570-2602, 2715-2730;
`filter.rs` HasManaAbility arms incl. :11692; `gen-test-fixture.py` header;
`raggadragga_goreguts_boss.rs` whole; `r.jsonl` Raggadragga row whole.

## Architectural sections

### Pattern Coverage

Class: Oracle-text object qualifiers naming an ability category —
`with a mana ability` (CR 605.1) and its sibling `with no abilities`
(CR 113.1). Fixture census above; record file + generator command in
`class-fixtures.json`. Bearing rulings: fixture rulings 1 and 3 (mana-ability
definition, classifier-owned — drive Verification rows 1-2's choice of live
dork fixtures over synthetic props).

### Sizing — 1 unit

Unit: trigger-subject (and every type-phrase consumer) `with a mana ability`
predicate grammar. Registration surfaces: none new (existing `FilterProp`,
existing matchers). Discriminating test: the already-committed RED parser test
`raggadragga_attack_trigger_subject_keeps_mana_ability`. No inter-unit edges.
Expected scope paths (phase-fit counting): `oracle_nom/filter.rs` (combinator
+ its unit tests, one path), `oracle_target.rs` (one arm), `search.rs`
(delegation; same behavior, existing test pins it). No fixture, export, or
protocol paths: nothing pins the stale `r.jsonl` row (searched: only the
fixture itself mentions it; all tests parse Oracle text at runtime), so the row
refreshes on the next full oracle-gen, out of this step's scope.

### Building Blocks

- `parse_no_abilities` (`oracle_nom/filter.rs:184`) — the closest existing
  authority. New `parse_mana_ability` is its sibling: bare predicate, call
  sites supply the "with " lead. Extending `parse_with_inner` does NOT fit:
  that parser serves P/T + counters inner content and is not on the
  type-phrase suffix path; the :4482 arm is.
- `parse_type_phrase_folding_unbound` suffix scan (:4405-4505) — owns every
  other "with X" qualifier; the missing arm belongs here, not in
  `oracle_trigger.rs` (a trigger-local strip would be the card-specific hack
  this task forbids, and would leave targets/search behind).
- `FilterProp::HasManaAbility` + `mana_abilities::is_mana_ability` — eval
  already complete; no new predicate, no new variant.
- `try_parse_event` Attacks arm, `attack_firings`, `valid_card_matches` —
  consume the fixed subject with no edits.

### Logic Placement

- Owning function for the grammar: `parse_mana_ability` in
  `oracle_nom/filter.rs` (predicate spelling authority).
- Owning call site: the CR 113.1 arm block in
  `parse_type_phrase_folding_unbound` (`oracle_target.rs:4482-4494`), extended
  to `alt` over both bare predicates. Callers delegating to it unchanged:
  `parse_single_subject` (trigger subjects), `parse_target_*` (targets),
  union folding.
- `search.rs:2623`: re-delegated to the shared combinator (same accepted
  language, single spelling authority).
- `oracle_static/shared.rs:5547`: INTENTIONALLY UNTOUCHED — different input
  shape (whole-subject trailing-strip with `TextPair` recursion, already green
  with its own test). Unifying it would risk a green path for zero behavior
  gain; the shared combinator unifies the two nom call sites.

### Rust Idioms

No new types. `FilterProp::HasManaAbility` reused. `nom::branch::alt` over the
two disjoint-head predicates (`a ...` vs `no ...`; order immaterial — state
why in a comment). Existing `(tag("with"), space1)` + offset-advance idiom
copied from the adjacent arm.

### Nom Compliance (mandatory — parser files change)

- New combinator (bare predicate, mirroring `parse_no_abilities`):
  `value(FilterProp::HasManaAbility, tag("a mana ability")).parse(input)`.
  Singular only: the qualifier position is singular in every attested Oracle
  printing (`with a mana ability`); plural `mana abilities` appears only in
  rulings prose. No `contains`/`starts_with` dispatch anywhere.
- Type-phrase arm: extend the :4488 block's inner parse to
  `alt((nom_filter::parse_mana_ability, nom_filter::parse_no_abilities))`
  on `with_rest`, keeping the existing `pos` advance.
- Search loop: replace the inline `tag("with a mana ability")` with
  `preceded((tag("with"), space1), nom_filter::parse_mana_ability)`,
  mirroring the `parse_no_abilities` delegation 7 lines below.
- Static trailing-strip keeps its existing `take_until` + `tag` needle (proven
  shape, green test); out of scope per Logic Placement.

### Extension vs Creation

Pure extension of the established "bare predicate in `oracle_nom/filter.rs` +
call-site grammar" pattern (precedent: `parse_no_abilities` with three call
sites). No new pattern. Precedence audit (implementer must confirm by test):
no earlier arm in the suffix scan claims `with a ...` — mana-value needs
`mana value`, power needs `power`, counter needs `counter(s)`, keyword needs a
leading keyword — and the full `phase-parser` suite must stay green.

### Analogous Trace

`HasNoAbilities` ("with no abilities") — full path in Step 2/3 above. The new
predicate walks the identical path; every layer it needs already evaluates
`HasManaAbility` (filter arms, characteristic reads, matcher).

### Variant Discoverability

N/A — no enum variant added. (`cargo engine-inventory` not consulted; nothing
to register.)

### Verification Matrix

| # | Claim | Changed seam | Production entry | Test | Revert-fail | Negative / hostile |
|---|-------|--------------|------------------|------|-------------|--------------------|
| 1 | `Attacks` mode + `Typed(Creature,You,[HasManaAbility])` subject | type-phrase arm (`oracle_target.rs:4482`) via `parse_single_subject` | `parse_trigger_line` on the verbatim clause-2 line | COMMITTED RED `raggadragga_attack_trigger_subject_keeps_mana_ability` → green | revert arm → `Unknown` again (assert mode first, then props) | paired positive: Raggadragga clause-3 shape test in same file stays green (proves harness parses triggers at all) |
| 2 | attacking mana dork untaps; vanilla attacker stays tapped | none (runtime already wired; row proves the seam) | `declare_attackers` → `advance_until_stack_empty` | COMMITTED RED `raggadragga_untaps_attacking_mana_dork_but_not_vanilla_attacker` → green | revert arm → dork stays tapped | vanilla attacker's tapped state is the reach-guard (proves it attacked and was NOT matched); hostile: Raggadragga itself (trigger+static, no mana ability) must not self-match — covered by anthem row's (4,4) assertion |
| 3 | bare predicate parses exactly `a mana ability` | new `parse_mana_ability` (`oracle_nom/filter.rs`) | combinator unit call | NEW trio mirroring `test_parse_no_abilities*`: ok / residual (`a mana ability and more` leaves ` and more`) / failure (`flying` errs) | delete combinator → compile fail at both call sites | failure case + `no abilities` still routes to `HasNoAbilities` (existing trio, unchanged) |
| 4 | type phrase `creature you control with a mana ability` folds with empty rest | same arm | `parse_type_phrase_folding` | NEW test: Typed(Creature, You, [HasManaAbility]), rest `""`; plus mid-phrase rest passthrough (`... with a mana ability attacks` leaves ` attacks`) | revert arm → rest keeps ` with ...` | `creature with flying` → keyword prop, NO HasManaAbility (adjacent-suffix guard); `creature you control with no abilities` unchanged |
| 5 | search delegation unchanged behavior | `search.rs:2623` re-delegation | `parse_search_filter` | EXISTING `parse_search_filter_handles_card_with_mana_ability` stays green | revert → still green (same language; this row is a no-regression pin, not a discriminator) | existing `parse_search_filter` no-abilities test stays green |
| 6 | no over-fire / no regressions | whole tree | full suites | `cargo test -p phase-parser`, engine integration suite incl. clause-1/3 tests + `cr605_1a_library_criterion.rs`; `cargo fmt`; `cargo clippy -D warnings` | — | hostile fixtures already committed: cheap-spell silence (no trigger prompt), mill-cost creature NOT pumped (classifier boundary: cost vs ability) |
| 7 | coverage honesty | none | — | No Oracle text newly accepted with deferred semantics: the arm lowers to a fully-evaluated prop; `Unknown` count for the card drops 1→0. No `Effect::unimplemented` involved. | — | — |

### Reference Readings

One preservation row: the static anthem's `Typed(Creature, You,
[HasManaAbility])` in `r.jsonl` is ALREADY the derived-correct value (verbatim
clause 1 + CR 605.1 read the live ability set); the committed anthem test
asserts (3,3)/(2,2)/(4,4) from runtime Oracle parsing, not from the fixture —
no parity-with-fixture assertion exists, so no defective-reference risk. No
other row copies a sibling route's result.

### Identity / Provenance Contract

N/A — no "this way"/chosen/target-identity binding introduced. The
`TriggeringSource` in the attack trigger's `SetTapState` effect is pre-existing
parser output (fixture-verified) and names the event subject per CR 603.2
attacker semantics; the untouched runtime test asserts its effect.

## CR citations (NOT grep-verified — see note)

NOTE: `docs/MagicCompRules.txt` is gitignored and absent in this checkout
(only the `docs/MagicCompRules.edition` pin: effective September 25, 2026);
`scripts/fetch-comp-rules.sh` cannot run it down without network egress this
step does not assume. Every citation below matches pervasive existing usage in
the same files (`CR 605.1` cited 30× across `crates/phase-parser/src/parser`,
incl. the adjacent :4482 arm's `CR 113.1 + CR 113.3` and `filter.rs:11692`'s
`CR 605.1`). The implementer re-greps after fetching the pinned edition; if a
renumbering moved any rule, the step migrates the citation text only.

- CR 605.1 (mana abilities — what counts): new combinator doc comment, arm
  comment. Precedent: `filter.rs:750,11690`; `shared.rs:5547`; `search.rs`
  context; fixture test comments.
- CR 113.1 + CR 113.3 ("no abilities" object predicate): retained on the
  extended arm; the `alt` pairs the two ability-category predicates.
- CR 508.1 (declare attackers; attacker is the event subject) + CR 603.2
  (trigger subject filter on the event): unchanged matcher path comments;
  cited in the committed tests already.
- CR 603.4 (intervening-if): clause 3, already green; no new citation.

## Step-by-step implementation

1. `crates/phase-parser/src/parser/oracle_nom/filter.rs`: add
   `pub fn parse_mana_ability(input) -> OracleResult<'_, FilterProp>` directly
   below `parse_no_abilities` (:184), same doc shape with CR 605.1:
   `value(FilterProp::HasManaAbility, tag("a mana ability")).parse(input)`.
   Add the unit-test trio beside `test_parse_no_abilities*` (:993-1012).
2. `crates/phase-parser/src/parser/oracle_target.rs`: in the :4485-4494 block,
   replace the single `parse_no_abilities(with_rest)` call with
   `alt((nom_filter::parse_mana_ability, nom_filter::parse_no_abilities))`;
   extend the block comment (`CR 605.1 + CR 113.1 + CR 113.3`, Raggadragga
   clause-2 example). Add the row-4 type-phrase tests in the file's test
   module (find the `with no abilities` type-phrase test and mirror it).
3. `crates/phase-parser/src/parser/oracle_effect/search.rs`: replace the
   :2623 inline `tag("with a mana ability")` with the composed
   `(tag("with"), space1)` + `nom_filter::parse_mana_ability` form; keep the
   loop's `remaining` advance identical. No test change (row 5 pins it).
4. Run: `cargo fmt`, `cargo clippy -p phase-parser --all-targets -D warnings`,
   `cargo test -p phase-parser`, then the engine integration tests
   (`raggadragga_goreguts_boss`, `cr605_1a_library_criterion`, trigger suites).
   Expect: row-1/2 RED→green, everything else green, clippy clean.
5. Confirm no test reads the stale `r.jsonl` Raggadragga row (searched clean
   at plan time; re-search after the change since diagnostics text may shift),
   then commit locally. NEVER push (parent pushes).

Explicit non-goals: no `oracle_trigger.rs` change, no engine/runtime change,
no fixture/export regeneration, no `oracle_static/shared.rs` change, no new
enum variant, no clause-1/3 work.

## Rollback / mutation strategy

- Rollback: revert the single commit; tree returns to 60d8da8fd state
  (1 RED parser test + 1 RED runtime test, both intended proofs). Each step
  1-3 is independently revertible; step 2 alone reverts behavior (steps 1+3
  without 2 leave a tested-but-unwired combinator and identical search
  behavior).
- Mutation check (implementer runs before commit): (a) temporarily restore
  `Unknown` by commenting the new `alt` arm — rows 1, 2, 4 must fail and ONLY
  those (+ no suite-wide fallout proves the arm claims nothing else);
  (b) feed `creature with a mana ability` (no controller) and
  `artifacts with a mana ability` through `parse_type_phrase_folding` in a
  scratch test — both must fold (class generality), then delete the scratch;
  (c) `cargo test -p phase-parser` green proves no earlier arm's language
  shrank (the suite pins every adjacent suffix).
