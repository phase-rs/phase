---
name: card-test
description: Canonical recipe for writing engine cast-pipeline tests. Use GameScenario + GameRunner::cast(...).resolve() and assert via CastOutcome deltas. Covers the six test-harness foot-guns (hand-written TargetRef vectors, incomplete modal target submission, wrong-point hand baseline, inline-keyword cards, AST-internal flag assertions, vacuous negative assertions) and the right-way fix for each. Use whenever writing or porting a runtime test that casts a spell and checks its effect.
---

# card-test — the canonical cast-pipeline test recipe

This skill gives ONE rigid recipe for runtime engine tests that cast a spell and
assert its effect. It exists because six test-harness foot-guns recur; the
[`SpellCast`] driver and [`CastOutcome`] (in `crates/engine/src/game/scenario.rs`)
make them structurally impossible when you follow this recipe.

Reference ports that demonstrate the pattern:

- `crates/engine/tests/chord_of_calling.rs` — X-spell, convoke, `final_waiting_for()` boundary.
- `crates/engine/src/game/casting.rs` — `exsanguinate_*` (life deltas, 2- and 3-player), `vicious_rivalry_*` (X-cost filter + zones), `kozileks_command_modes_*` (modal + X + multi-target).

## The recipe

```rust
use engine::game::scenario::{GameScenario, GameRunner};
// ... typed imports for the effect under test ...

#[test]
fn my_card_does_the_thing() {
    // 1. BUILD STATE via GameScenario (or wrap a raw GameState with
    //    GameRunner::from_state(state) when the test builds state imperatively).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "My Card", /* is_instant */ true, ORACLE)
        .id();
    // ... stage targets / mana / library as needed ...
    let mut runner = scenario.build();

    // 2. CAST through the pipeline. Declare INTENT, never targets-by-hand.
    let outcome = runner
        .cast(spell)
        .modes(&[0, 2])              // modal "choose N" — omit if non-modal
        .x(3)                        // announce X — omit if non-X
        .target_player(P1)           // declare a player intent (reusable across slots)
        .target_objects(&[victim])   // declare object intents (one per slot, in order)
        .convoke_with(&[creature])   // tap creatures to pay via Convoke — omit otherwise
        .resolve();

    // 3. ASSERT via CastOutcome deltas — behavior/semantics, never AST internals.
    outcome.assert_life_delta(P1, -3);
    outcome.assert_zone(&[victim], Zone::Exile);
    outcome.assert_hand_drawn(P0, 1);
    // For "no further prompt" boundaries (e.g. fail-to-find), inspect the halt state:
    assert!(matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }));
}
```

### What the driver does for you

`resolve()` runs a bounded state-machine over `state.waiting_for` (CR 601.2a–h):

- `ModeChoice` → submits `.modes(..)` (panics if modal but no modes declared).
- `ChooseXValue` → submits `.x(..)` (panics if X needed but not declared).
- `ManaPayment { convoke_mode }` → taps each `.convoke_with(..)` creature with
  mana of its color, then finalizes (CR 702.51b). Pool-funded casts auto-pay and
  never surface this window.
- `TargetSelection` → answers **one slot at a time, in written order**
  (CR 601.2c). Object intent is consumed (one declared object per slot); player
  intent is reusable (one player may be targeted by several modes).
- `Priority` (post-cast) → captures the per-player hand baseline at stack commit
  (CR 601.2a) and proceeds to resolution.
- Resolution auto-answers `OrderTriggers` (CR 603.3b) and `ScryChoice` (keeps
  looked-at cards on top, CR 701.22a); it stops at stack-empty or at any prompt
  it is not taught to answer (e.g. `SearchChoice`) so you can assert on it via
  `final_waiting_for()`.
- Any unhandled prompt → a clear `extend the driver or drive this case manually`
  panic. Extending the driver is the correct response; never assert around a
  silent skip.

### CastOutcome accessors

| Accessor | Returns |
|---|---|
| `hand_drawn(p)` / `assert_hand_drawn(p, n)` | net cards drawn since stack commit (the clean resolution delta) |
| `zone_of(o)` / `assert_zone(&[o], zone)` | an object's current zone |
| `life_delta(p)` / `assert_life_delta(p, n)` | net life change since before the cast |
| `final_waiting_for()` | the state the pipeline halted in |
| `state()` | read-only `&GameState` for assertions the typed accessors don't cover |

## Anti-patterns — the six foot-guns and the right-way fix

1. **Hand-written `TargetRef` vectors.** Building a flat `Vec<TargetRef>` and
   submitting `SelectTargets { targets }` is fragile (`TargetRef` is non-`Copy`;
   `.copied()` won't compile, and the order must match the slots exactly).
   *Fix:* declare intent with `.target_object(..)` / `.target_player(..)`; the
   driver matches each intent to its slot.

2. **Incomplete modal target submission.** Omitting one mode's slot yields
   `InvalidAction("Illegal target selected")` because targets are validated one
   per slot, in written order (`ability_utils::choose_target`).
   *Fix:* the driver answers every slot via `ChooseTarget` in order — you cannot
   forget a slot.

3. **Hand/zone baseline captured at the wrong point.** `handle_cast_spell` does
   NOT remove the spell from hand; CR 601.2a removes it only when the cast
   commits to the stack (the `Priority` window). A baseline taken before that is
   off by one.
   *Fix:* `CastOutcome::hand_drawn` is measured against the stack-commit
   baseline the driver captures for you. Use `assert_hand_drawn`, never a
   hand-picked `let hand_before = ...`.

4. **Keywords fed as inline Oracle reminder text.** Writing `"Convoke (Your
   creatures ...)"` as plain text parses to `Effect::Unimplemented`.
   *Fix:* build keyworded cards via
   `from_oracle_text_with_keywords(&["Convoke"], text)`, and pay convoke via
   `.convoke_with(&[..])` (which the driver routes through `TapForConvoke`).

5. **Asserting representation-internal dual-encoded flags.** Asserting an
   AST field such as `ChangeZone.up_to` couples the test to one encoding of the
   spec rather than the behavior.
   *Fix:* assert behavior via `CastOutcome` deltas (`zone_of`, `life_delta`,
   `hand_drawn`). When a SHAPE assertion is genuinely needed (parser structure),
   label the test SHAPE and assert via semantic accessors (e.g. a
   `MultiTargetSpec`), not internal bools.

6. **Vacuous negative assertions.** A negative assertion (`!detector(...)`,
   "no longer parses to X", "counter is NOT applied") passes for the wrong
   reason when an upstream guard short-circuits before the code under test
   runs. Canonical instance: `check_swallowed_clauses` returns early when any
   parsed ability contains `Effect::Unimplemented`, so
   `!has_swallowed_clause(...)` passes on a card that failed to parse at all —
   the single most common review finding on contributor PRs.
   *Fix:* every negative assertion must be paired with a positive reach-guard
   in the same test proving the input got past the short-circuit (parse
   succeeded, zero `Effect::Unimplemented` entries, the expected positive AST
   shape, or a positive runtime delta on the sibling path). Better still,
   replace the negative with a runtime regression whose assertion flips when
   the fix is reverted.

## Hard rules

- **Never call the raw `resolve()` stack function directly.** Drive through the
  `apply()` pipeline (via `runner.cast(..).resolve()` or `runner.act(..)`).
  Calling `stack::resolve_top` / `effect::resolve` directly bypasses the
  intervening-if recheck and the `cast_from_zone` carry-through, so cast-trigger
  bugs (cascade/storm/casualty) go invisible. See the memory landmine
  `project_cast_from_zone_stack_wipe`.
- **Prefer runtime-behavior tests.** Reserve AST-SHAPE tests for parser
  structure; label them SHAPE and assert via semantic accessors, not internal
  flags. The two Kozilek/Chord SHAPE tests
  (`kozileks_command_full_four_mode_parse`,
  `chord_of_calling_parses_x_mana_value_search_shape`) are the correct pattern
  for that and are intentionally NOT driven through `cast()`.
- **CR-annotate any assertion that encodes a rule** (verify the number against
  `docs/MagicCompRules.txt` before writing it).
- **Use the card's verbatim Oracle text, never a paraphrase.** Build the test
  card from the real card's exact Oracle text (`add_spell_to_hand_from_oracle`
  with the verbatim string, or `add_real_card` for integration fixtures).
  Paraphrased text can take a different parser branch than the real card, so
  the test goes green while the actual card stays broken until card-data is
  regenerated (see `project_parser_fix_inert_until_data_regen`).
- **Regenerate the integration fixture when your parser change touches a card
  it contains.** `add_real_card` reads the committed
  `crates/engine/tests/fixtures/integration_cards.json` snapshot.
  `python3 scripts/gen-test-fixture.py --check` verifies **coverage only — that every
  referenced card is PRESENT (a key-set comparison). It never compares the stored parse
  VALUES against a fresh export.** So a parser change that alters a fixture card's parse
  leaves the fixture stale while the gate stays green: the integration test then asserts
  against the *old* parse and proves nothing about the code you shipped. If your change
  alters how any fixture card parses, re-run `python3 scripts/gen-test-fixture.py` (no
  `--check`) and commit the regenerated fixture in the same PR.
