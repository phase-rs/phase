//! Printed-order regression for `ParsedAbilities::{triggers, abilities}`.
//!
//! CR 707.9a: "Some copy effects cause the copy to gain an ability as part of the
//! copying process." The Unstable Shapeshifter example — "becomes a copy of that
//! creature, except it has this ability" — requires the engine to identify *which*
//! ability is meant. It does so by index: `result.triggers[i]` and
//! `result.abilities[i]` are the card's *printed* ability slots.
//!
//! `parse_oracle_ir` builds those vectors in two concatenated stages. Four
//! preprocessors run before the per-line dispatch loop starts (`oracle.rs:2914`)
//! and `extend` the vectors with everything they find anywhere on the card:
//!
//! ```text
//! oracle.rs:2773  parse_saga_chapters              -> triggers
//! oracle.rs:2784  parse_attraction_visit_triggers  -> triggers
//! oracle.rs:2799  parse_level_blocks               -> statics
//! oracle.rs:2870  parse_level_blocks (in-block)    -> triggers
//! oracle.rs:2888  parse_spacecraft_threshold_lines -> statics, triggers, abilities
//! oracle.rs:2914  while i < lines.len()            -- dispatch loop starts HERE
//! ```
//!
//! So each vector is `[pre-loop] ++ [dispatch-loop]`, which has nothing to do with
//! printed order. Whenever a preprocessor claims a line printed *below* a line the
//! dispatch loop claims, the vector comes back inverted and every printed slot
//! after the inversion point is wrong.
//!
//! These are the six cards in the current corpus where that happens. Each assertion
//! anchors on a *discriminating field*, never on a position, so the test cannot
//! pass vacuously by matching the very ordering it is trying to pin down.
//!
//! # This file is necessary but NOT sufficient. Do not read a green here as "fixed."
//!
//! These tests assert the order of the *item vector*. They say nothing about the
//! *printed slot index*, which is a separate value: `emit` advances it at emission
//! time (`oracle_ir/doc.rs`, `spells_emitted.push` / `trigger_index += 1`) while the
//! item map is keyed by source position. Emission order can never equal source order
//! here, because the dispatch loop's skip sets are *produced by* the preprocessors and
//! so the preprocessors must emit first.
//!
//! A change that gives the preprocessors exact spans — and nothing else — turns every
//! test in this file green while `RetainPrintedTriggerFromSource { source_trigger_index }`
//! keeps the wrong integer, baked in at parse time by `parse_has_this_ability`. The
//! defect would survive behind a vector that looks correct.
//!
//! The complete fix is late binding: store the enclosing `OracleItemId` (the clause is a
//! self-reference, not an absolute index) and resolve it once at `finish()` from the
//! source-ordered vector. That fix needs its own **synthetic** fixture — the copy-except
//! clause and the six cards below are disjoint in the current corpus, so no printed card
//! exercises the interaction. See `ISSUES.md` #12.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, ActivationRestriction};
use engine::types::triggers::TriggerMode;
use engine::types::TriggerDefinition;

/// The sole index of the element matching `pred`.
///
/// Panics when zero or several elements match. That panic is the point: it is an
/// in-test control. If a parser change stops producing the ability an assertion
/// anchors on, this test aborts instead of quietly passing on an anchor that no
/// longer selects anything.
fn sole_index<T>(
    card: &str,
    vector: &str,
    marker: &str,
    items: &[T],
    pred: impl Fn(&T) -> bool,
) -> usize {
    let hits: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| pred(item))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "{card}: expected exactly one element of `{vector}` to match the anchor `{marker}`, \
         found {}. The anchor no longer discriminates, so this test can no longer detect the \
         printed-order bug it exists to detect. Fix the anchor before trusting a green.",
        hits.len(),
    );
    hits[0]
}

/// Asserts that `anchors`, listed in printed order, occupy strictly increasing
/// positions in the parsed vector.
fn assert_printed_order(card: &str, vector: &str, anchors: &[(&str, usize)]) {
    for pair in anchors.windows(2) {
        let (earlier_marker, earlier_pos) = pair[0];
        let (later_marker, later_pos) = pair[1];
        assert!(
            earlier_pos < later_pos,
            "{card}: `{vector}` is not in printed order. `{earlier_marker}` is printed above \
             `{later_marker}`, but lands at index {earlier_pos} while `{later_marker}` lands at \
             index {later_pos}. CR 707.9a binds an \"except it has this ability\" clause by \
             printed slot, so the copy would gain the wrong ability.",
        );
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn parse_spacecraft(oracle: &str, name: &str) -> engine::parser::oracle::ParsedAbilities {
    // Spacecraft print `Station` as an MTGJSON keyword ability; card-data is
    // generated WITH it, so the `Station` reminder line is consumed as a keyword.
    // Passing it here matches production — without it the bare `Station` line falls
    // through to an `Unimplemented` ability that pollutes the `abilities` vector
    // (irrelevant to the printed-ORDER property under test).
    parse_oracle_text(
        oracle,
        name,
        &strings(&["Station"]),
        &strings(&["Artifact"]),
        &strings(&["Spacecraft"]),
    )
}

fn has_mode(mode: TriggerMode) -> impl Fn(&TriggerDefinition) -> bool {
    move |t: &TriggerDefinition| t.mode == mode
}

fn unrestricted(a: &AbilityDefinition) -> bool {
    a.activation_restrictions.is_empty()
}

fn counter_threshold(a: &AbilityDefinition) -> bool {
    a.activation_restrictions
        .iter()
        .any(|r| matches!(r, ActivationRestriction::CounterThreshold { .. }))
}

fn level_range(min: u32, max: Option<u32>) -> impl Fn(&AbilityDefinition) -> bool {
    move |a: &AbilityDefinition| {
        a.activation_restrictions.iter().any(|r| {
            matches!(
                r,
                ActivationRestriction::LevelCounterRange { minimum, maximum }
                    if *minimum == min && *maximum == max
            )
        })
    }
}

// ---------------------------------------------------------------------------
// triggers — Spacecraft threshold blocks printed below an ETB trigger
// ---------------------------------------------------------------------------

/// Candela's `8+` block spans a *continuation* line (line 4 carries no `8+ |`
/// prefix), so a scan keyed on text after the pipe misses it entirely.
#[test]
fn candela_triggers_are_in_printed_order() {
    const ORACLE: &str = "Flash\n\
        When Candela enters, return up to one target creature you control to its owner's hand. It perpetually gets +1/+1.\n\
        Station\n\
        8+ | Flying\n\
        Whenever Candela deals combat damage to a player, you may put a creature card with mana value 3 or less from your hand onto the battlefield.";

    let parsed = parse_spacecraft(ORACLE, "Candela, Aegis of Adagia");
    let card = "Candela, Aegis of Adagia";

    let etb = sole_index(
        card,
        "triggers",
        "ChangesZone (line 1)",
        &parsed.triggers,
        has_mode(TriggerMode::ChangesZone),
    );
    let combat = sole_index(
        card,
        "triggers",
        "DamageDone (line 4)",
        &parsed.triggers,
        has_mode(TriggerMode::DamageDone),
    );

    assert_printed_order(
        card,
        "triggers",
        &[
            ("ChangesZone (line 1)", etb),
            ("DamageDone (line 4)", combat),
        ],
    );
}

#[test]
fn infinite_guideline_station_triggers_are_in_printed_order() {
    const ORACLE: &str = "When Infinite Guideline Station enters, create a tapped 2/2 colorless Robot artifact creature token for each multicolored permanent you control.\n\
        Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 12+.)\n\
        12+ | Flying\n\
        Whenever Infinite Guideline Station attacks, draw a card for each multicolored permanent you control.";

    let parsed = parse_spacecraft(ORACLE, "Infinite Guideline Station");
    let card = "Infinite Guideline Station";

    let etb = sole_index(
        card,
        "triggers",
        "ChangesZone (line 0)",
        &parsed.triggers,
        has_mode(TriggerMode::ChangesZone),
    );
    let attacks = sole_index(
        card,
        "triggers",
        "Attacks (line 3)",
        &parsed.triggers,
        has_mode(TriggerMode::Attacks),
    );

    assert_printed_order(
        card,
        "triggers",
        &[("ChangesZone (line 0)", etb), ("Attacks (line 3)", attacks)],
    );
}

#[test]
fn specimen_freighter_triggers_are_in_printed_order() {
    const ORACLE: &str = "When this Spacecraft enters, return up to two target non-Spacecraft creatures to their owners' hands.\n\
        Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 9+.)\n\
        9+ | Flying\n\
        Whenever this Spacecraft attacks, defending player mills four cards.";

    let parsed = parse_spacecraft(ORACLE, "Specimen Freighter");
    let card = "Specimen Freighter";

    let etb = sole_index(
        card,
        "triggers",
        "ChangesZone (line 0)",
        &parsed.triggers,
        has_mode(TriggerMode::ChangesZone),
    );
    let attacks = sole_index(
        card,
        "triggers",
        "Attacks (line 3)",
        &parsed.triggers,
        has_mode(TriggerMode::Attacks),
    );

    assert_printed_order(
        card,
        "triggers",
        &[("ChangesZone (line 0)", etb), ("Attacks (line 3)", attacks)],
    );
}

// ---------------------------------------------------------------------------
// triggers — Attraction visit line printed below an ETB trigger
// ---------------------------------------------------------------------------

/// The visit trigger is recognized by the literal `"Visit — "` prefix
/// (`oracle_attraction.rs:19-20`), not by the prose `"When you visit"`, and it
/// carries `description: None`. Both facts have already caused corpus scans to
/// silently match zero Attraction cards.
#[test]
fn memory_test_triggers_are_in_printed_order() {
    const ORACLE: &str = "When this Attraction enters, target opponent exiles cards from the bottom of their library until they exile five nonland cards, then turns those nonland cards face down.\n\
        Visit — Name the exiled face-down cards, then look at them. If you named them correctly, turn them face up, then claim the prize!\n\
        Prize — Create three 1/1 red Balloon creature tokens with flying, then sacrifice this Attraction and open an Attraction.";

    let parsed = parse_oracle_text(
        ORACLE,
        "Memory Test",
        &[],
        &strings(&["Artifact"]),
        &strings(&["Attraction"]),
    );
    let card = "Memory Test";

    let etb = sole_index(
        card,
        "triggers",
        "ChangesZone (line 0)",
        &parsed.triggers,
        has_mode(TriggerMode::ChangesZone),
    );
    let visit = sole_index(
        card,
        "triggers",
        "VisitAttraction (line 1)",
        &parsed.triggers,
        has_mode(TriggerMode::VisitAttraction),
    );

    assert_printed_order(
        card,
        "triggers",
        &[
            ("ChangesZone (line 0)", etb),
            ("VisitAttraction (line 1)", visit),
        ],
    );
}

// ---------------------------------------------------------------------------
// abilities — threshold / level blocks printed below a plain activated ability
// ---------------------------------------------------------------------------

/// Both abilities are `{T}` → mana, so mode and cost cannot tell them apart.
/// The only discriminating field is `activation_restrictions`.
#[test]
fn the_eternity_elevator_abilities_are_in_printed_order() {
    const ORACLE: &str = "{T}: Add {C}{C}{C}.\n\
        Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery.)\n\
        20+ | {T}: Add X mana of any one color, where X is the number of charge counters on The Eternity Elevator.";

    let parsed = parse_spacecraft(ORACLE, "The Eternity Elevator");
    let card = "The Eternity Elevator";

    let plain = sole_index(
        card,
        "abilities",
        "unrestricted {T}: Add {C}{C}{C} (line 0)",
        &parsed.abilities,
        unrestricted,
    );
    let gated = sole_index(
        card,
        "abilities",
        "CounterThreshold charge>=20 (line 2)",
        &parsed.abilities,
        counter_threshold,
    );

    assert_printed_order(
        card,
        "abilities",
        &[
            ("unrestricted (line 0)", plain),
            ("CounterThreshold (line 2)", gated),
        ],
    );
}

/// `parse_level_blocks` runs with no subtype guard (`oracle.rs:2797`), so a Land
/// reaches it. The unrestricted `{T}: Add {C}.` on line 1 is printed *above* both
/// `LEVEL` blocks yet lands last.
#[test]
fn under_construction_skyscraper_abilities_are_in_printed_order() {
    const ORACLE: &str =
        "Level up {1} ({1}: Put a level counter on this. Level up only as a sorcery.)\n\
        {T}: Add {C}.\n\
        LEVEL 1-7\n\
        {T}: Add {W}, {B}, {G}, or {C}.\n\
        LEVEL 8+\n\
        {T}: Add {W}, {B}, {G}, or {C}. Scry 1.";

    let parsed = parse_oracle_text(
        ORACLE,
        "Under-Construction Skyscraper",
        &[],
        &strings(&["Land"]),
        &[],
    );
    let card = "Under-Construction Skyscraper";

    let plain = sole_index(
        card,
        "abilities",
        "unrestricted {T}: Add {C} (line 1)",
        &parsed.abilities,
        unrestricted,
    );
    let low = sole_index(
        card,
        "abilities",
        "LevelCounterRange 1..=7 (line 3)",
        &parsed.abilities,
        level_range(1, Some(7)),
    );
    let high = sole_index(
        card,
        "abilities",
        "LevelCounterRange 8.. (line 5)",
        &parsed.abilities,
        level_range(8, None),
    );

    assert_printed_order(
        card,
        "abilities",
        &[
            ("unrestricted (line 1)", plain),
            ("LevelCounterRange 1..=7 (line 3)", low),
            ("LevelCounterRange 8.. (line 5)", high),
        ],
    );
}
