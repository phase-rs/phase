//! CR 107.3c — the where-X lowering pass must assert its OWN post-condition.
//!
//! `apply_where_x_effect_expression` rewrites the quantity slots of the `Effect`
//! variants it enumerates. It originally enumerated 23 of the 62 `QuantityExpr` carriers;
//! the other 39 fell through a `_ => {}`. Task #95 enumerated them all (plus the 4
//! `PtValue` carriers), so today the wildcard is reached only by variants that carry no X
//! slot at all.
//!
//! Enumeration is necessary but NOT sufficient for a green face: a variant can be
//! enumerated and still fail to bind, because `parse_where_x_quantity_expression` cannot
//! represent every "where X is …" definition (see the unrepresentable-expression test).
//! The guard is what keeps that failure honest.
//!
//! Before the totality guard, the wildcard's failure mode was a FABRICATION, not a red:
//! the effect kept its bare `QuantityRef::Variable { name: "X" }`, which resolves to 0
//! at runtime (amass 0 / surveil 0 / discard 0) while the face still rendered as fully
//! supported. That lie is invisible to a red-count ledger (there is no `Unimplemented`
//! node to count) AND to the zero-raw-text invariant ("X" is the legitimate alias). A
//! control with an escape hatch is not a control.
//!
//! THE TRAP THIS FILE EXISTS TO GUARD: the guard must be keyed on the EXPRESSION, never
//! on tree-presence of `Variable("X")`. Two families legitimately bind X TO the
//! placeholder, and for them a residual `Variable("X")` is the CORRECT lowering:
//!
//!   Join Forces (CR 107.3i)   "where X is the total amount of mana paid this way"
//!                             -> resolved through the `chosen_x` machinery.
//!   Constraint tails (608.2g) "where X is less than or equal to <bound>"
//!                             -> BOUNDS the player's chosen X rather than defining it.
//!
//! A naive tree-presence guard would flip BOTH families from green to red. The control
//! tests below prove that: each asserts the face still carries a residual `Variable("X")`
//! (so a tree-presence guard WOULD fire on it) and that it nonetheless stays bound. The
//! control can fail, which is what makes it a control.
//!
//! Oracle text below is verbatim from MTGJSON, never paraphrased.

use engine::parser::parse_oracle_text;
use engine::types::ability::Effect;
use serde_json::Value;

/// Every `QuantityRef::Variable { name: "X" }` reachable in a parsed face.
///
/// This is deliberately the NAIVE predicate — tree-presence, exactly what the totality
/// guard must NOT key on. The preserve tests use it to prove a naive guard would fire.
fn retains_variable_x(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            let is_x = map.get("type").and_then(Value::as_str) == Some("Variable")
                && map
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.eq_ignore_ascii_case("X"));
            is_x || map.values().any(retains_variable_x)
        }
        Value::Array(items) => items.iter().any(retains_variable_x),
        _ => false,
    }
}

struct Parsed {
    tree: Value,
    has_where_x_gap: bool,
}

fn parse(oracle: &str, name: &str, types: &[&str]) -> Parsed {
    let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    let tree = serde_json::to_value(&parsed).expect("parse tree serializes");

    fn gap(v: &Value) -> bool {
        match v {
            Value::Object(m) => {
                let hit = m.get("type").and_then(Value::as_str) == Some("Unimplemented")
                    && m.get("name").and_then(Value::as_str) == Some("where_x_binding");
                hit || m.values().any(gap)
            }
            Value::Array(a) => a.iter().any(gap),
            _ => false,
        }
    }
    let has_where_x_gap = gap(&tree);
    Parsed {
        tree,
        has_where_x_gap,
    }
}

// ---------------------------------------------------------------------------
// PRESERVE CONTROLS — the naive guard MUST be able to fail these.
// ---------------------------------------------------------------------------

/// CR 107.3i — Join Forces. "where X is the total amount of mana paid this way" binds X
/// to the PLACEHOLDER on purpose: the value arrives through `chosen_x` after the
/// pay-any-amount loop, so `Variable("X")` IS the correct lowering.
///
/// This is one of exactly 6 faces in the pool (5 Join Forces + Well of Lost Dreams) for
/// which a residual `Variable("X")` is legitimate. A tree-presence guard flips it to red.
#[test]
fn join_forces_placeholder_bind_survives_the_totality_guard() {
    let parsed = parse(
        "Join forces — Starting with you, each player may pay any amount of mana. Each player \
         searches their library for up to X basic land cards, where X is the total amount of \
         mana paid this way, puts them onto the battlefield tapped, then shuffles.",
        "Collective Voyage",
        &["Sorcery"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "CR 107.3i: Join Forces binds X to the placeholder deliberately (the value arrives via \
         chosen_x after the pay-any-amount loop). The totality guard is keyed on the EXPRESSION, \
         so it must leave this face bound. A where_x_binding gap here means the guard regressed \
         to NAIVE tree-presence — which is exactly the failure this control exists to catch. \
         Tree: {:?}",
        parsed.tree
    );

    // NON-VACUITY: the face genuinely still carries a residual Variable("X"), which is what a
    // naive tree-presence guard would have fired on. Without this, "no gap" could pass for the
    // wrong reason (e.g. the clause never parsed) and the control would prove nothing.
    assert!(
        retains_variable_x(&parsed.tree),
        "control is vacuous: Collective Voyage no longer carries a residual Variable(\"X\"), so \
         it is no longer a witness that a tree-presence guard would misfire. Tree: {:?}",
        parsed.tree
    );
}

/// CR 608.2g — a comparator-shaped tail CONSTRAINS the player's chosen X rather than
/// defining it. Well of Lost Dreams pays {X} and draws X; the bound only limits what may
/// be chosen, and the drawn count is the amount actually paid (via `chosen_x`).
#[test]
fn constraint_tail_placeholder_bind_survives_the_totality_guard() {
    let parsed = parse(
        "Whenever you gain life, you may pay {X}, where X is less than or equal to the amount \
         of life you gained. If you do, draw X cards.",
        "Well of Lost Dreams",
        &["Artifact"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "CR 608.2g: the comparator tail BOUNDS the chosen X, it does not define it, so \
         Variable(\"X\") is the correct lowering and the face must stay bound. A gap here means \
         the guard regressed to NAIVE tree-presence. Tree: {:?}",
        parsed.tree
    );

    // NON-VACUITY, as above: this face really is a witness a tree-presence guard would misfire on.
    assert!(
        retains_variable_x(&parsed.tree),
        "control is vacuous: Well of Lost Dreams no longer carries a residual Variable(\"X\"). \
         Tree: {:?}",
        parsed.tree
    );
}

// ---------------------------------------------------------------------------
// THE GUARD ITSELF — an X that cannot be bound must RED, never fabricate.
// ---------------------------------------------------------------------------

/// CR 107.3c — the guard's positive witness: a where-X definition the parser cannot
/// REPRESENT must red, never fabricate.
///
/// HISTORY, so this pin is not misread: it originally asserted that `Effect::Surveil` was
/// one of the unenumerated `QuantityExpr` carriers, and that the red came from the
/// `_ => {}` wildcard. Task #95 enumerated Surveil, so that premise is now false — yet the
/// face still reds, for a DIFFERENT reason: "the number of opponents being attacked" has no
/// `QuantityRef`, so `parse_where_x_quantity_expression` returns `None` and the bind fails.
///
/// The invariant being pinned is therefore "an unrepresentable where-X reds", NOT "an
/// unenumerated variant reds" — enumeration is necessary but not sufficient for green, and
/// pinning the transient marker rather than the invariant would have let this test keep
/// passing while quietly meaning something else.
#[test]
fn unrepresentable_where_x_expression_reds_instead_of_fabricating() {
    let parsed = parse(
        "Flying\nWhenever you attack, surveil X, where X is the number of opponents being \
         attacked.",
        "Dimir Strandcatcher",
        &["Creature"],
    );

    assert!(
        parsed.has_where_x_gap,
        "CR 107.3c: 'the number of opponents being attacked' is not a representable quantity, \
         so the where-X slot cannot be bound and the totality guard must report the gap. \
         Leaving Variable(\"X\") here surveils 0 at runtime while rendering as fully supported \
         — a fabrication invisible to both the red-count ledger and the zero-raw-text \
         invariant. Tree: {:?}",
        parsed.tree
    );

    // And the fabrication is genuinely gone, not merely accompanied by a gap.
    assert!(
        !retains_variable_x(&parsed.tree),
        "the bare Variable(\"X\") must be REPLACED by the gap node, not left beside it. Tree: {:?}",
        parsed.tree
    );

    // NON-VACUITY: the red must be EXPRESSION-specific, not the guard redding every where-X
    // face it sees. The same effect with a representable expression must come out green.
    //
    // Scope of what this proves, stated precisely so it is not over-read: it discriminates a
    // blanket-red guard, and nothing more. It does NOT prove the effect-level enumeration is
    // load-bearing — the clause-level quantity parser already resolves this simple shape
    // before the where-X rewriter ever runs, so this assertion holds with or without
    // Surveil's arm. (Measured: on the pre-#95 parser this same text already bound to
    // ObjectCount.) The proof that enumeration is load-bearing is the COVERAGE section
    // below, whose faces were each watched failing on the pre-#95 parser.
    let representable = parse(
        "Whenever you attack, surveil X, where X is the number of creatures you control.",
        "~",
        &["Creature"],
    );
    assert!(
        !representable.has_where_x_gap,
        "control: a REPRESENTABLE where-X expression on the same effect must bind green. A gap \
         here means the guard is redding every where-X face wholesale, so the red above proves \
         nothing about expression coverage. Tree: {:?}",
        representable.tree
    );
}

/// NEGATIVE CONTROL for the guard — an ENUMERATED variant whose expression IS
/// representable must still bind to green. Without this, a guard that simply redded every
/// where-X face would pass every test above.
#[test]
fn enumerated_bindable_where_x_still_binds_green() {
    let parsed = parse(
        "You gain X life, where X is the number of creatures you control.",
        "~",
        &["Sorcery"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "an enumerated variant (GainLife) with a representable expression must still BIND. A \
         gap here means the totality guard is over-firing and redding good faces. Tree: {:?}",
        parsed.tree
    );

    let abilities = parse_oracle_text(
        "You gain X life, where X is the number of creatures you control.",
        "~",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        abilities
            .abilities
            .iter()
            .any(|d| matches!(&*d.effect, Effect::GainLife { .. })),
        "reach-guard: the face must actually parse to GainLife, so the assertion above is not \
         passing because the clause failed to parse at all. Parsed: {:?}",
        abilities.abilities
    );
}

// ---------------------------------------------------------------------------
// COVERAGE (task #95) — the representable where-X definitions must BIND to green.
//
// The guard (above) proves an unbindable X reds. These prove the complement: that the
// pass actually binds what it CAN represent. Without them, a pass that redded every
// where-X face in the pool would satisfy every assertion in this file.
//
// Each face below was a LYING GREEN before #5753 (bare Variable("X") resolving to 0 under
// a green badge), an honest red after it, and is bound for real here. Oracle text is
// verbatim from MTGJSON.
// ---------------------------------------------------------------------------

/// The newly-enumerated single-quantity carriers. Each owns one `QuantityExpr` slot that
/// previously fell through the `_ => {}` wildcard unbound.
///
/// Table-driven because the point is the CLASS (every quantity-carrying effect binds its
/// slot), not any one card — a per-card test here would pass while the next card with the
/// same shape silently fabricated.
#[test]
fn newly_enumerated_quantity_carriers_bind_their_where_x_slot() {
    // (card, types, oracle) — all verbatim MTGJSON text.
    let cases: [(&str, &[&str], &str); 5] = [
        (
            "Mind Burst",
            &["Sorcery"],
            "Target player discards X cards, where X is one plus the number of cards named \
             Mind Burst in all graveyards.",
        ),
        (
            "Arcane Omens",
            &["Instant"],
            "Converge — Target player discards X cards, where X is the number of colors of \
             mana spent to cast this spell.",
        ),
        (
            "Maester Seymour",
            &["Creature"],
            "{3}{G}{G}: Monstrosity X, where X is the number of counters among creatures you \
             control.",
        ),
        (
            "Clay Golem",
            &["Artifact", "Creature"],
            "{6}, Roll a d8: Monstrosity X, where X is the result.",
        ),
        (
            "Mycelic Ballad",
            &["Enchantment"],
            "Each player sacrifices X creatures and you gain X life, where X is this spell's \
             intensity.",
        ),
    ];

    for (name, types, oracle) in cases {
        let parsed = parse(oracle, name, types);
        assert!(
            !parsed.has_where_x_gap,
            "{name}: this where-X expression IS representable, so the effect's quantity slot must \
             BIND. A where_x_binding gap means its Effect variant is still falling through the \
             wildcard unbound (discard 0 / monstrosity 0 / sacrifice 0 at runtime). Tree: {:?}",
            parsed.tree
        );
        assert!(
            !retains_variable_x(&parsed.tree),
            "{name}: the where-X clause was consumed but a bare Variable(\"X\") SURVIVED in the \
             tree — that placeholder resolves to 0 at runtime while the face renders as fully \
             supported. Binding must replace it, not sit beside it. Tree: {:?}",
            parsed.tree
        );
    }
}

/// CR 122.1 — the enters-with-counters rider is its own where-X slot, BESIDE the token's
/// count and P/T. G'raha Tia creates a *fixed* 1/1 Hero, so `count`, `power` and
/// `toughness` are all concrete and X lives only in the rider: an arm that bound the other
/// three still left the Hero entering with 0 counters.
#[test]
fn token_enter_with_counters_rider_binds_where_x() {
    let parsed = parse(
        "Lifelink\nThrow Wide the Gates — Whenever you cast a noncreature spell, you may pay X \
         life, where X is that spell's mana value. If you do, create a 1/1 colorless Hero \
         creature token and put X +1/+1 counters on it. Do this only once each turn.",
        "G'raha Tia, Scion Reborn",
        &["Creature"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "CR 122.1: 'that spell's mana value' is representable, so the enters-with rider's counter \
         COUNT must bind. Tree: {:?}",
        parsed.tree
    );
    assert!(
        !retains_variable_x(&parsed.tree),
        "the rider's count kept a bare Variable(\"X\") — the Hero enters with 0 counters while the \
         face reads as supported. Tree: {:?}",
        parsed.tree
    );
}

/// CR 601.2e — a cast permission can be BOUNDED by X. Kiora's "mana value less than X" lives
/// in the `CastFromZone` permission constraint, not in the target filter, so a filter-only
/// bind left it a bare placeholder: mana value < 0, which permits nothing at all.
#[test]
fn cast_permission_constraint_binds_where_x() {
    let parsed = parse(
        "Vigilance, ward {3}\nWhenever you cast a Kraken, Leviathan, Octopus, or Serpent spell \
         from your hand, look at the top X cards of your library, where X is that spell's mana \
         value. You may cast a spell with mana value less than X from among them without paying \
         its mana cost. Put the rest on the bottom of your library in a random order.",
        "Kiora, Sovereign of the Deep",
        &["Creature"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "CR 601.2e: the cast-permission mana-value bound is a where-X slot and must bind. \
         Tree: {:?}",
        parsed.tree
    );
    assert!(
        !retains_variable_x(&parsed.tree),
        "the permission constraint kept a bare Variable(\"X\") — it would permit only mana value \
         < 0, i.e. nothing, while the face reads as supported. Tree: {:?}",
        parsed.tree
    );
}

/// CR 613.4b — an X-bearing P/T does not always reach the rewriter as `PtValue::Variable("X")`.
/// When the clause grammar has already lowered the slot to a quantity, X survives one level
/// down inside `PtValue::Quantity(...)`. Tivash's X/X Demon rides exactly that shape, and
/// matching only the bare-placeholder form left it entering as an 0/0 — dead on arrival to
/// state-based actions (CR 704.5f) while the face rendered as supported.
#[test]
fn pt_value_quantity_slot_binds_where_x() {
    let parsed = parse(
        "Lifelink\nAt the beginning of your end step, if you gained life this turn, you may pay \
         X life, where X is the amount of life you gained this turn. If you do, create an X/X \
         black Demon creature token with flying.",
        "Tivash, Gloom Summoner",
        &["Creature"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "CR 613.4: 'the amount of life you gained this turn' is representable, so the token's \
         X/X must bind. Tree: {:?}",
        parsed.tree
    );
    assert!(
        !retains_variable_x(&parsed.tree),
        "the token's power/toughness kept a bare Variable(\"X\") — it enters 0/0 and dies to SBAs \
         (CR 704.5f) while the face reads as supported. Tree: {:?}",
        parsed.tree
    );
}
