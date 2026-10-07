//! CR 614.1c: an enters-with replacement places the counters it names. When
//! its leading amount is dynamic ("X", "twice X", "that many") and is placed
//! once FOR EACH object of a "for each" count — alone or conjoined with a
//! further "and … for each" placement — the total is a product of two
//! game-state values, and `QuantityExpr` has no product of two dynamic
//! quantities (`Multiply` scales by a constant factor). The whole replacement
//! is declined; it never becomes a supported count that drops the leading
//! amount (X=3 over two other red and one other green creature must be
//! 3×2+1=7, never 2+1).
//!
//! The same holds when the clause's sentence carries a per-each connective
//! ("for each" / "plus") that no tail parser reads — an operand with no
//! quantity reading, a conjunct or bonus placing a different counter kind, a
//! recipient outside the pronoun set ("on that creature"), a comma before
//! "plus", or a conjoined counter list — and any amount is dynamic: a dynamic
//! amount is itself a quantity reference, so the `DynamicQty` swallow detector
//! that surfaces the unread tail behind a FIXED amount cannot see it, and the
//! replacement must decline instead.
//!
//! A conjoined counter list with a dynamic element and an amount override
//! ("…, where X is …" / "… equal to …") is declined too: the override rewrites
//! only the single-counter count, so the list would publish its element's bare
//! `CostXPaid` with the override's quantity dropped.
//!
//! Unsupported is shown through the public coverage authority
//! (`card_face_gaps`, via `assert_unsupported`) — except where the declined
//! line falls through to the effect parser's verb-less for-each put-counter
//! arm (issue #9659: an X base plus an additional counter of another kind, a
//! counter list led by an X amount with a per-each tail, a comma before
//! "plus"). There no enters-with replacement is published and the line is
//! surfaced by the `Replacement` swallow detector, which whole-card coverage
//! reads (`assert_declined_to_replacement_swallow`).
//!
//! Under "plus an additional … for each" the leading amount is an addend, not
//! a factor, so a dynamic base composes faithfully as a sum and stays
//! represented, with Sheriff of Safe Passage as its control, and places its
//! counters at runtime. A conjoined counter list whose later element carries
//! its own count arithmetic ("and X plus one charge counters on it") has no
//! per-each tail and stays represented.
//!
//! Controls, per test:
//! * the further-conjunct and single-clause tests pair each dynamic line with a
//!   fixed amount over the same populations that stays represented — exact
//!   count shape, no coverage gap, no gap-producing parse warning; the
//!   further-conjunct control (`FIXED_COMPOUND`) also places its counters at
//!   runtime;
//! * the amount-override counter-list test pairs its dynamic lines with a
//!   represented fixed counter list (exact placements) and with a fixed
//!   counter list beside a ", where X is …" definition, which keeps its exact
//!   placements and is surfaced by the `DynamicQty` swallow detector;
//! * the mixed-kind, other-kind-bonus, per-each counter-list and
//!   unread-connective tests pair each dynamic line with a fixed amount over the
//!   same text whose dropped tail the `DynamicQty` swallow detector surfaces —
//!   honest, but not represented;
//! * the unreadable-operand test's fixed control is pinned in `swallow_check`
//!   (`enters_with_unparsed_for_each_operand_is_a_swallow`); the where-X test
//!   has no fixed control.
//!
//! Synthetic class cards (no printed card yet), except Sheriff of Safe Passage
//! (verbatim Oracle text).

use engine::game::coverage::card_face_gaps;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, FilterProp, QuantityExpr, QuantityRef, TargetFilter,
    TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DYNAMIC_COMPOUND: &str = "This creature enters with X +1/+1 counters on it for each other \
     red creature you control and a +1/+1 counter on it for each other green creature you \
     control.";
const FIXED_COMPOUND: &str = "This creature enters with two +1/+1 counters on it for each other \
     red creature you control and a +1/+1 counter on it for each other green creature you \
     control.";
const DYNAMIC_BASE_PLUS_ADDITIONAL: &str = "This creature enters with X +1/+1 counters on it plus \
     an additional +1/+1 counter on it for each other creature you control.";
const DYNAMIC_MIXED_KIND: &str = "This creature enters with X +1/+1 counters on it for each other \
     red creature you control and a charge counter on it for each other green creature you \
     control.";
const FIXED_MIXED_KIND: &str = "This creature enters with two +1/+1 counters on it for each other \
     red creature you control and a charge counter on it for each other green creature you \
     control.";
const DYNAMIC_PLUS_ADDITIONAL_OTHER_KIND: &str =
    "This creature enters with X +1/+1 counters on it \
     plus an additional charge counter on it for each other creature you control.";
const FIXED_PLUS_ADDITIONAL_OTHER_KIND: &str = "This creature enters with a +1/+1 counter on it \
     plus an additional charge counter on it for each other creature you control.";

fn face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Creature".to_string()], &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        keywords: parsed.extracted_keywords,
        parse_warnings: parsed.parse_warnings,
        ..Default::default()
    }
}

/// Whether the parser surfaced a swallowed clause from `detector`.
fn has_swallow(face: &CardFace, detector: &str) -> bool {
    face.parse_warnings.iter().any(|warning| {
        matches!(
            warning,
            OracleDiagnostic::SwallowedClause { detector: found, .. } if found == detector
        )
    })
}

/// The counts of every enters-with `PutCounter` the face's replacements place.
fn enters_with_counts(face: &CardFace) -> Vec<QuantityExpr> {
    face.replacements
        .iter()
        .filter_map(|def| def.execute.as_deref())
        .filter_map(|execute| match execute.effect.as_ref() {
            Effect::PutCounter { count, .. } => Some(count.clone()),
            _ => None,
        })
        .collect()
}

/// Every counter placement of the face's enters-with replacements, walking each
/// execute chain (a conjoined counter list chains one `PutCounter` per element).
fn enters_with_placements(face: &CardFace) -> Vec<(CounterType, QuantityExpr)> {
    let mut placements = Vec::new();
    for def in &face.replacements {
        let mut next: Option<&AbilityDefinition> = def.execute.as_deref();
        while let Some(ability) = next {
            if let Effect::PutCounter {
                counter_type,
                count,
                ..
            } = ability.effect.as_ref()
            {
                placements.push((counter_type.clone(), count.clone()));
            }
            next = ability.sub_ability.as_deref();
        }
    }
    placements
}

/// Assert the face is represented: no coverage gap from `card_face_gaps` AND no
/// gap-producing parse warning. Whole-card coverage also reads
/// `parse_warnings`, which `card_face_gaps` ignores, and turns every
/// `SwallowedClause`, `TargetFallback`, and `CascadeLoss` into a gap
/// (`parse_warning_gap_label`); only `IgnoredRemainder` produces none.
fn assert_represented(face: &CardFace) {
    assert_eq!(card_face_gaps(face), Vec::<String>::new(), "{}", face.name);
    assert!(
        face.parse_warnings
            .iter()
            .all(|warning| matches!(warning, OracleDiagnostic::IgnoredRemainder { .. })),
        "{}: {:?}",
        face.name,
        face.parse_warnings
    );
}

/// Assert the line is unsupported: no counter placement is claimed, the whole
/// line is recorded as an unimplemented replacement structure, and that is the
/// card's coverage gap.
fn assert_unsupported(text: &str) {
    // The parser normalizes the self-reference to `~`; the rest is verbatim.
    let body = text
        .strip_prefix("This creature ")
        .expect("synthetic lines name the creature");
    let face = face("Dynamic Hellion", text);
    assert_eq!(
        enters_with_counts(&face),
        Vec::<QuantityExpr>::new(),
        "{text:?}: no partial count may be published"
    );
    // No placement may hide anywhere else on the face either (a nested or
    // re-routed `PutCounter` would slip past the root-only count above).
    assert!(
        face.replacements.is_empty()
            && face.triggers.is_empty()
            && face.static_abilities.is_empty(),
        "{text:?}: {face:#?}"
    );
    // The unimplemented node is the face's ONLY ability: a supported
    // placement beside it (an effect-parser fall-through) would be published.
    assert!(
        matches!(
            face.abilities.as_slice(),
            [only] if only
                .effect
                .unimplemented_description()
                .is_some_and(|line| line.ends_with(body))
        ),
        "{text:?}: the whole line must be the only, unimplemented ability, got {:?}",
        face.abilities
    );
    assert_eq!(
        card_face_gaps(&face),
        vec!["Effect:replacement_structure".to_string()],
        "{text:?}"
    );
}

/// Assert the enters-with replacement is declined and the line surfaced, for a
/// line that falls through to the effect parser's verb-less for-each
/// put-counter arm (issue #9659): no replacement, trigger, or static is
/// published and the `Replacement` swallow detector reports the line, so
/// whole-card coverage is red. The fall-through ability itself is #9659's
/// defect and is deliberately not pinned here.
fn assert_declined_to_replacement_swallow(text: &str) {
    let face = face("Dynamic Hellion", text);
    assert!(
        face.replacements.is_empty()
            && face.triggers.is_empty()
            && face.static_abilities.is_empty(),
        "{text:?}: {face:#?}"
    );
    assert!(
        has_swallow(&face, "Replacement"),
        "{text:?}: {:?}",
        face.parse_warnings
    );
}

/// CR 614.1c: X counters for each other red creature and a counter
/// for each other green creature is unsupported, never the sum of the two
/// populations; two counters for each other red creature over the same
/// populations is the represented control.
#[test]
fn dynamic_amount_per_each_with_a_further_conjunct_is_unsupported() {
    let control = face("Fixed Hellion", FIXED_COMPOUND);
    let counts = enters_with_counts(&control);
    assert!(
        matches!(
            counts.as_slice(),
            [QuantityExpr::Sum { exprs }]
                if matches!(
                    exprs.as_slice(),
                    [
                        QuantityExpr::Multiply { factor: 2, inner },
                        QuantityExpr::Ref { .. },
                    ] if matches!(inner.as_ref(), QuantityExpr::Ref { .. })
                )
        ),
        "reach guard: {counts:?}"
    );
    assert_represented(&control);

    assert_unsupported(DYNAMIC_COMPOUND);
    assert_unsupported(
        "This creature enters with twice X +1/+1 counters on it for each other red creature \
         you control and a +1/+1 counter on it for each other green creature you control.",
    );
}

/// CR 614.1c: a single per-each clause scaled by a dynamic amount
/// is unsupported, never the bare population count; a fixed amount is the
/// represented control.
#[test]
fn dynamic_amount_for_each_single_clause_is_unsupported() {
    let control = face(
        "Fixed Hellion",
        "This creature enters with two +1/+1 counters on it for each other creature you \
         control.",
    );
    let counts = enters_with_counts(&control);
    assert!(
        matches!(
            counts.as_slice(),
            [QuantityExpr::Multiply { factor: 2, inner }]
                if matches!(inner.as_ref(), QuantityExpr::Ref { .. })
        ),
        "reach guard: {counts:?}"
    );
    assert_represented(&control);

    assert_unsupported(
        "This creature enters with X +1/+1 counters on it for each other creature you control.",
    );
}

/// CR 614.1c: a dynamic base under "plus an additional … for each"
/// is an addend, so it composes as `X + count` and stays supported — never the
/// bare base; Sheriff of Safe Passage's fixed base (verbatim) is the control.
#[test]
fn dynamic_base_plus_additional_for_each_sums_base_and_bonus() {
    let sheriff = face(
        "Sheriff of Safe Passage",
        "This creature enters with a +1/+1 counter on it plus an additional +1/+1 counter on \
         it for each other creature you control.",
    );
    let counts = enters_with_counts(&sheriff);
    assert!(
        matches!(
            counts.as_slice(),
            [QuantityExpr::Offset { offset: 1, inner }]
                if matches!(inner.as_ref(), QuantityExpr::Ref { .. })
        ),
        "reach guard: {counts:?}"
    );
    assert_represented(&sheriff);

    let dynamic = face("Dynamic Hellion", DYNAMIC_BASE_PLUS_ADDITIONAL);
    assert_eq!(
        enters_with_counts(&dynamic),
        vec![QuantityExpr::Sum {
            exprs: vec![
                QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid,
                },
                // "other creature you control": another creature, controlled
                // by the entering creature's controller.
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .properties(vec![FilterProp::Another]),
                        ),
                    },
                },
            ],
        }]
    );
    assert_represented(&dynamic);
}

/// CR 614.1c + CR 107.3m: a conjoined counter list whose later element carries
/// its own count arithmetic ("and X plus one charge counters on it") has no
/// per-each tail: the list reads every element, so the line stays represented
/// with both placements — its "plus" is the element's count, not a per-each
/// connective.
#[test]
fn counter_list_element_count_arithmetic_stays_represented() {
    let list = face(
        "Arithmetic Hellion",
        "This creature enters with a +1/+1 counter and X plus one charge counters on it.",
    );
    assert_eq!(
        enters_with_placements(&list),
        vec![
            (CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 }),
            (
                CounterType::Generic("charge".to_string()),
                QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid,
                    }),
                    offset: 1,
                },
            ),
        ]
    );
    assert_represented(&list);
}

/// CR 614.1c: a dynamic amount per object of a per-each operand with no
/// quantity reading ("+1/+1 counter on the sacrificed creature") is
/// unsupported, never the bare amount. The fixed-amount control is the
/// `DynamicQty` swallow pinned by `swallow_check`'s
/// `enters_with_unparsed_for_each_operand_is_a_swallow` over the same operand.
#[test]
fn dynamic_amount_for_each_unreadable_operand_is_unsupported() {
    assert_unsupported(
        "This creature enters with X +1/+1 counters on it for each +1/+1 counter on the \
         sacrificed creature.",
    );
}

/// CR 614.1c: X counters for each other red creature conjoined with a charge
/// counter for each other green creature is unsupported, never the bare X; two
/// counters for each other red creature over the same conjunct is the control,
/// whose dropped conjunct the `DynamicQty` swallow detector surfaces.
#[test]
fn dynamic_amount_per_each_with_a_mixed_kind_conjunct_is_unsupported() {
    let control = face("Fixed Hellion", FIXED_MIXED_KIND);
    assert_eq!(
        enters_with_counts(&control),
        vec![QuantityExpr::Fixed { value: 2 }],
        "reach guard"
    );
    assert!(
        has_swallow(&control, "DynamicQty"),
        "{:?}",
        control.parse_warnings
    );

    assert_unsupported(DYNAMIC_MIXED_KIND);
}

/// CR 614.1c: X counters plus an additional charge counter for each other
/// creature publishes no enters-with count — never the bare X with the bonus
/// dropped — and the line is surfaced; one counter plus the same bonus is the
/// control, whose dropped bonus the `DynamicQty` swallow detector surfaces.
#[test]
fn dynamic_base_plus_additional_of_another_kind_publishes_no_count() {
    let control = face("Fixed Hellion", FIXED_PLUS_ADDITIONAL_OTHER_KIND);
    assert_eq!(
        enters_with_counts(&control),
        vec![QuantityExpr::Fixed { value: 1 }],
        "reach guard"
    );
    assert!(
        has_swallow(&control, "DynamicQty"),
        "{:?}",
        control.parse_warnings
    );

    assert_declined_to_replacement_swallow(DYNAMIC_PLUS_ADDITIONAL_OTHER_KIND);
}

/// CR 614.1c: X counters and a charge counter on it for each other creature is
/// declined — the conjoined list must not publish its counts with the per-each
/// tail dropped — and so is a dynamic later list element; two counters and a
/// charge counter over the same tail is the control, whose dropped tail the
/// `DynamicQty` swallow detector surfaces.
#[test]
fn dynamic_counter_list_with_a_per_each_tail_is_declined() {
    let control = face(
        "Fixed Hellion",
        "This creature enters with two +1/+1 counters and a charge counter on it for each \
         other creature you control.",
    );
    assert_eq!(
        enters_with_counts(&control),
        vec![QuantityExpr::Fixed { value: 2 }],
        "reach guard"
    );
    assert!(
        has_swallow(&control, "DynamicQty"),
        "{:?}",
        control.parse_warnings
    );

    assert_declined_to_replacement_swallow(
        "This creature enters with X +1/+1 counters and a charge counter on it for each other \
         creature you control.",
    );
    assert_unsupported(
        "This creature enters with a +1/+1 counter and X charge counters on it for each other \
         creature you control.",
    );
}

/// CR 614.1c: a dynamic amount whose per-each connective follows a recipient
/// outside the pronoun set ("on that creature", "on each of them") or a comma
/// ("on it, plus …") is declined, never the bare amount; two counters over the
/// same text is the control, whose dropped tail the `DynamicQty` swallow
/// detector surfaces.
#[test]
fn dynamic_amount_with_an_unread_per_each_connective_is_declined() {
    // (fixed control, dynamic line, how the dynamic line is shown declined)
    type Case = (&'static str, &'static str, fn(&str));
    let cases: [Case; 3] = [
        (
            "This creature enters with two +1/+1 counters on that creature for each other \
             creature you control.",
            "This creature enters with X +1/+1 counters on that creature for each other \
             creature you control.",
            assert_unsupported,
        ),
        (
            "This creature enters with two +1/+1 counters on each of them for each other \
             creature you control.",
            "This creature enters with X +1/+1 counters on each of them for each other \
             creature you control.",
            assert_unsupported,
        ),
        (
            "This creature enters with two +1/+1 counters on it, plus an additional +1/+1 \
             counter on it for each other creature you control.",
            "This creature enters with X +1/+1 counters on it, plus an additional +1/+1 \
             counter on it for each other creature you control.",
            assert_declined_to_replacement_swallow,
        ),
    ];
    for (fixed, dynamic, assert_declined) in cases {
        let control = face("Fixed Hellion", fixed);
        assert_eq!(
            enters_with_counts(&control),
            vec![QuantityExpr::Fixed { value: 2 }],
            "reach guard: {fixed:?}"
        );
        assert!(
            has_swallow(&control, "DynamicQty"),
            "{fixed:?}: {:?}",
            control.parse_warnings
        );

        assert_declined(dynamic);
    }
}

/// CR 107.3 + CR 614.1c: a conjoined counter list with a dynamic element and an
/// amount override — a ", where X is …" definition or an "equal to …" count —
/// is unsupported: the override binds only the single-counter count, so the
/// list would publish the element's bare `CostXPaid` and drop the override's
/// quantity. A fixed counter list is the represented control, and a fixed
/// counter list beside a where-X definition pins that only a dynamic element
/// declines.
#[test]
fn dynamic_counter_list_with_an_amount_override_is_unsupported() {
    let control = face(
        "Fixed Hellion",
        "This creature enters with a +1/+1 counter and two charge counters on it.",
    );
    assert_eq!(
        enters_with_placements(&control),
        vec![
            (CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 }),
            (
                CounterType::Generic("charge".to_string()),
                QuantityExpr::Fixed { value: 2 },
            ),
        ]
    );
    assert_represented(&control);

    // A fixed list beside an override is not declined: the where-X definition
    // binds nothing in a list with no X, so the list keeps its exact
    // placements, and the `DynamicQty` swallow detector surfaces the unbound
    // definition. This pins the guard's dynamic-ness check.
    let fixed_with_override = face(
        "Fixed Hellion",
        "This creature enters with a +1/+1 counter and two charge counters on it, where X is \
         the number of creatures you control.",
    );
    assert!(
        !fixed_with_override.replacements.is_empty(),
        "{fixed_with_override:#?}"
    );
    assert_eq!(
        enters_with_placements(&fixed_with_override),
        vec![
            (CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 }),
            (
                CounterType::Generic("charge".to_string()),
                QuantityExpr::Fixed { value: 2 },
            ),
        ]
    );
    assert!(
        has_swallow(&fixed_with_override, "DynamicQty"),
        "{:?}",
        fixed_with_override.parse_warnings
    );

    assert_unsupported(
        "This creature enters with a +1/+1 counter and X charge counters on it, where X is the \
         number of creatures you control.",
    );
    assert_unsupported(
        "This creature enters with X +1/+1 counters and a charge counter on it, where X is the \
         number of creatures you control.",
    );
    assert_unsupported(
        "This creature enters with a +1/+1 counter and X charge counters on it equal to the \
         number of creatures you control.",
    );
}

/// CR 107.3 + CR 614.1c: an X defined by a trailing "where X is" clause is
/// still a dynamic amount. Scaled per object it needs a product; under "plus an
/// additional … for each" the X-binding override would replace the composed
/// count wholesale. Both decline the whole replacement — never the bare
/// "where X" quantity with the per-each count dropped.
#[test]
fn where_x_amount_with_a_per_each_tail_is_unsupported() {
    assert_unsupported(
        "This creature enters with X +1/+1 counters on it for each other creature you \
         control, where X is the number of cards in your hand.",
    );
    assert_unsupported(
        "This creature enters with X +1/+1 counters on it plus an additional +1/+1 counter on \
         it for each other creature you control, where X is the number of cards in your hand.",
    );
}

/// CR 614.1c: the fixed control places two counters per other red
/// creature and one per other green creature: two red allies and one green ally
/// give 2×2+1=5; the opponent's red creature counts for neither.
#[test]
fn fixed_amount_per_each_with_a_further_conjunct_places_its_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Red Ally A", "Red Ally B"] {
        scenario
            .add_creature(P0, name, 2, 2)
            .with_color(vec![ManaColor::Red]);
    }
    scenario
        .add_creature(P0, "Green Ally", 2, 2)
        .with_color(vec![ManaColor::Green]);
    scenario
        .add_creature(P1, "Red Foe", 2, 2)
        .with_color(vec![ManaColor::Red]);
    let hellion = scenario
        .add_creature_to_hand_from_oracle(P0, "Fixed Hellion", 0, 0, FIXED_COMPOUND)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(hellion).resolve();
    outcome.assert_zone(&[hellion], Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&hellion]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&5)
    );
}

/// CR 107.3m + CR 614.1c: cast for X=4 beside two other creatures you control
/// (and one opponent's), the dynamic base plus one counter per other creature
/// places 4+2=6 — neither the base alone (4), the bonus alone (2), nor the
/// product (8).
#[test]
fn dynamic_base_plus_additional_for_each_places_base_and_bonus() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Ally A", "Ally B"] {
        scenario.add_creature(P0, name, 2, 2);
    }
    scenario.add_creature(P1, "Foe", 2, 2);
    let hellion = scenario
        .add_creature_to_hand_from_oracle(P0, "Dynamic Hellion", 0, 0, DYNAMIC_BASE_PLUS_ADDITIONAL)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        std::iter::once(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .chain((0..4).map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(hellion).x(4).resolve();
    outcome.assert_zone(&[hellion], Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&hellion]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&6)
    );
}
