//! Structural golden tests for the mtgish → engine converter.
//!
//! See `crates/mtgish-import/CLAUDE.md` § "Test discipline" and
//! `.claude/plans/lovely-dancing-codd.md` § "Test Discipline".
//!
//! These goldens lock in the **semantic content** of converter output for a
//! representative spread of conversion archetypes. A failing assertion is
//! load-bearing: it means the converter's output for an established card has
//! changed shape or content, and a human must review the diff manually before
//! any update lands.
//!
//! **No automatic re-bless.** Per the plan, this suite intentionally does not
//! support a `BLESS_STRUCTURAL=1` env-var path. If a golden needs to be
//! updated, edit the `expected.json` file by hand after reviewing the diff
//! the assertion prints.
//!
//! # Layout
//!
//! ```text
//! tests/golden/structural/<archetype>/
//!     input.json     — the mtgish OracleCard JSON object for one card
//!     expected.json  — canonical-form serialization of `Vec<EngineFaceStub>`
//! ```
//!
//! Each archetype has one input card chosen for clean conversion **today**.
//! If a planned archetype cannot be cleanly converted at the time of writing,
//! we document the gap on the test rather than golden-test a failure (per the
//! constraint "don't golden-test cards that currently strict-fail").
//!
//! # Canonicalization
//!
//! Engine types serialize via serde with stable, declaration-order field
//! emission (no `#[serde(skip_serializing_if = ...)]` reordering). We
//! re-serialize the result through `serde_json::Value` and pretty-print with
//! the default 2-space indent, which yields a deterministic byte-for-byte
//! output for any given `EngineFaceStub` value. Map keys (where present) are
//! emitted in `BTreeMap` order by the engine types or, for ad-hoc maps, by
//! `serde_json::Map`'s insertion order — which matches struct field
//! declaration order through serde. We intentionally **do not** post-walk
//! the JSON tree to sort keys: the canonical form is whatever the engine's
//! own serde derives produce, and changes to that emission are themselves
//! material structural events worth catching.

use mtgish_import::convert::{convert_card, EngineFaceStub};
use mtgish_import::report::{Ctx, ImportReport};
use mtgish_import::schema::types::OracleCard;

/// Run the converter on a single card's `input.json` and assert its canonical
/// serialization matches `expected.json` byte-for-byte.
fn assert_golden(archetype: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/structural")
        .join(archetype);
    let input_path = dir.join("input.json");
    let expected_path = dir.join("expected.json");

    let input_raw = std::fs::read_to_string(&input_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", input_path.display()));
    let card: OracleCard = serde_json::from_str(&input_raw)
        .unwrap_or_else(|e| panic!("deserialize {}: {e}", input_path.display()));

    let card_name = serde_json::from_str::<serde_json::Value>(&input_raw)
        .ok()
        .and_then(|v| v.get("Name").and_then(|n| n.as_str()).map(str::to_string))
        .unwrap_or_else(|| archetype.to_string());

    let mut report = ImportReport::default();
    let mut ctx = Ctx::new(card_name.clone(), &mut report);
    let stubs: Vec<EngineFaceStub> = match convert_card(&card, &mut ctx) {
        Ok(v) => v,
        Err(e) => panic!(
            "[{archetype}] {card_name} failed to convert (this golden requires clean conversion): {e}"
        ),
    };
    if ctx.finish() {
        panic!(
            "[{archetype}] {card_name} recorded a conversion gap during conversion; \
             goldens require clean cards"
        );
    }

    let actual =
        serde_json::to_string_pretty(&stubs).expect("serialize EngineFaceStub vector to JSON");

    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", expected_path.display()));

    // Trim trailing whitespace/newlines so editor-added newlines don't break
    // the comparison; the canonical form is otherwise byte-exact.
    if actual.trim_end() != expected.trim_end() {
        panic!(
            "\n[{archetype}] structural golden mismatch for {card_name}\n\
             expected (from {}):\n{expected}\n\n\
             actual:\n{actual}\n\n\
             To update: review the diff carefully, then overwrite the expected file by hand. \
             Auto-blessing via env var is intentionally not supported.\n",
            expected_path.display()
        );
    }
}

// ─── Archetype gaps documented (per the task brief) ─────────────────────────
//
// Several requested archetypes are not currently representable as clean
// converter output, so we do not golden-test them here:
//
//   * Modal `ChooseOne` spells — `Actions::Modal` is a top action-shape gap
//     (round-7 instrumentation). No clean modal card was found in the corpus
//     under the current converter. Re-add when `action::convert_list` learns
//     the modal shape.
//   * Saga (chapter triggers) — `Rule::Saga` does not yet convert cleanly.
//   * Cost-reduction static ("creatures you cast cost {1} less") — no clean
//     card found via `ReduceCost` mtgish predicate.
//   * Replacement: prevent-all / set-to damage — no clean card found.
//   * Multi-rule with `TriggerA` + `ActivatedAbility` — `ActivatedAbility`
//     does not yet convert cleanly (0 hits in the discovery scan).
//
// In place of those five we lock in five additional clean archetypes that
// represent equally important conversion shapes: `multi_keyword_protection`,
// `aura_grants_flying_and_pump`, `etb_replacement_plus_trigger`,
// `etb_and_ltb_lifegain`, and `etb_with_counters_and_trigger`. Together with
// the five planned archetypes that *do* convert cleanly today, this gives
// 10 archetypes total — meeting the task's coverage mandate while honoring
// "don't golden-test cards that currently strict-fail".
//
// ─── Planned archetypes that convert cleanly ────────────────────────────────

#[test]
fn vanilla_etb_trigger() {
    // Archetype 1: vanilla creature with one ETB trigger.
    // Card: Augury Owl — Flying + "When this enters, scry 3."
    // (ETB+draw was specified in the brief; no clean ETB+draw card exists in
    // the corpus today, so we substitute the closest clean one. Scry exercises
    // the same trigger+effect shape.)
    assert_golden("vanilla_etb_trigger");
}

#[test]
fn etb_tapped() {
    // Archetype 2: permanent that enters tapped (replacement effect via
    // `AsPermanentEnters` → `EntersTapped`).
    // Card: Alpine Meadow.
    assert_golden("etb_tapped");
}

#[test]
fn etb_with_counters() {
    // Archetype 3: creature that enters with N +1/+1 counters.
    // Card: Endless One — `EntersWithNumberCounters(ValueX, +1/+1)`.
    assert_golden("etb_with_counters");
}

#[test]
fn equipment_with_equip_cost() {
    // Archetype 5: Equipment with an Equip activated ability and a host-pump
    // static.
    // Card: Bonesplitter — equipped creature gets +2/+0; equip {1}.
    assert_golden("equipment_equip_cost");
}

#[test]
fn madness_alternative_cost() {
    // Archetype 8: alternative-cost spell.
    // Card: Reckless Wurm — Trample + Madness {2}{R}.
    assert_golden("madness_alternative_cost");
}

// ─── Substitute archetypes filling the five gap slots ───────────────────────

#[test]
fn multi_keyword_protection() {
    // Substitutes for archetype 4 (modal): a multi-keyword vanilla creature
    // with parameterized `Protection`. Locks in keyword conversion across
    // many variants (Flying/FirstStrike/Vigilance/Trample/Haste + Protection).
    // Card: Akroma, Angel of Wrath.
    assert_golden("multi_keyword_protection");
}

#[test]
fn aura_grants_flying_and_pump() {
    // Substitutes for archetype 6 (saga): an Aura that grants a continuous
    // P/T modification *and* a granted keyword via `PermanentLayerEffect` →
    // `AdjustPT` + `AddAbility(Flying)`. Locks in static-effect conversion.
    // Card: Arcane Flight.
    assert_golden("aura_grants_flying_and_pump");
}

#[test]
fn etb_replacement_plus_trigger() {
    // Substitutes for archetype 7 (cost-reduction static): an enchantment
    // with both a `ReplaceWouldEnter` (opp creatures ETB tapped) and a
    // `TriggerA` (gain 1 life when an opp creature ETBs). Locks in mixed
    // replacement+trigger output for filtered opponent permanents.
    // Card: Authority of the Consuls.
    assert_golden("etb_replacement_plus_trigger");
}

#[test]
fn etb_and_ltb_lifegain() {
    // Substitutes for archetype 9 (replacement damage): a creature with
    // symmetric ETB and LTB triggers (gain 2 life each). Locks in
    // `WhenAPermanentEntersTheBattlefield` + `WhenAPermanentLeavesTheBattlefield`
    // shape on the same card.
    // Card: Aven Riftwatcher.
    assert_golden("etb_and_ltb_lifegain");
}

#[test]
fn etb_with_counters_and_trigger() {
    // Substitutes for archetype 10 (trigger + activated): a creature that
    // combines `AsPermanentEnters` (enters with -1/-1 counters) with two
    // `TriggerA`s (remove counter when you cast a red/white spell). Locks
    // in multi-rule conversion across replacement + trigger paths.
    // Card: Belligerent Hatchling.
    assert_golden("etb_with_counters_and_trigger");
}
