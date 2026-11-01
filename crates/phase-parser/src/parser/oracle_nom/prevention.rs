//! Shared grammar for prevention amounts that are relative to an in-flight
//! damage event. These forms must never fall through to a one-damage shield.

use std::num::NonZeroU32;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_till1};
use nom::combinator::{map, map_opt, opt, rest, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use crate::parser::oracle_nom::error::OracleResult;
use crate::parser::oracle_nom::primitives::parse_number;
use crate::types::ability::{PreventionFormula, RoundingMode};

/// CR 615.1a + CR 107.1a: parse the amount after `prevent ` when it is a
/// portion of the same damage event. The quantity-binding form deliberately
/// requires its `where X is` tail; an unbound X is not a zero or one fallback.
pub fn parse_damage_prevention_formula(input: &str) -> OracleResult<'_, PreventionFormula> {
    alt((
        map(
            terminated(parse_number, tag(" of that damage")),
            PreventionFormula::fixed,
        ),
        map(
            terminated(parse_number, tag(" damage that")),
            PreventionFormula::fixed,
        ),
        value(
            PreventionFormula::Fraction {
                numerator: 1,
                denominator: NonZeroU32::new(2).expect("2 is nonzero"),
                rounding: RoundingMode::Up,
            },
            tag("half that damage, rounded up"),
        ),
        value(
            PreventionFormula::Fraction {
                numerator: 1,
                denominator: NonZeroU32::new(2).expect("2 is nonzero"),
                rounding: RoundingMode::Down,
            },
            tag("half that damage, rounded down"),
        ),
        map_opt(
            preceded(tag("x of that damage, where x is "), rest),
            |quantity| {
                crate::parser::oracle_quantity::parse_cda_quantity(quantity)
                    .map(|quantity| PreventionFormula::Quantity { quantity })
            },
        ),
    ))
    .parse(input)
}

/// Classify a prevention clause that names an event-relative amount, including
/// unsupported unbound forms. Consumers use this to produce an honest parser
/// gap instead of `PreventDamage { amount: Next(1), .. }`.
pub fn has_event_relative_prevention_amount(input: &str) -> bool {
    // These are parser inputs already normalized to lowercase. `tag` keeps the
    // recognition at a word boundary rather than treating an arbitrary substring
    // as Oracle grammar.
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(input, |candidate| {
        alt((
            tag::<_, _, crate::parser::oracle_nom::error::OracleError<'_>>("x of that damage"),
            tag("half that damage"),
        ))
        .parse(candidate)
    })
    .is_some()
}

/// Classify an each-time prevention formula that needs a continuous, repeatable
/// damage-event watcher in addition to its event-relative amount.
pub fn has_each_time_event_relative_prevention(input: &str) -> bool {
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(
        input,
        parse_each_time_event_relative_prevention,
    )
    .is_some()
}

/// ORIGINAL-CASE input, case-sensitive — the OPPOSITE of this module's `has_*`
/// siblings, which document already-lowercased input. The contract is in the
/// name so a caller cannot silently feed it lowered text. Measured: across the
/// 38 corpus occurrences of this phrase (37 cards) ZERO are capitalized, so the
/// lowercase tag misses nothing today; if a capitalized printing ever appears,
/// lower the input at the call site rather than widening the tag, which would
/// also widen the adjacent Awe Strike arm in `oracle_effect/assembly.rs`.
///
/// CR 615.5: recognize the `"prevented this way"` back-reference anywhere in a
/// clause fragment. The anaphor can only bind to a prevention printed earlier in
/// the same effect chain, so its presence is the TEXTUAL half of "this clause is
/// a rider on that prevention". The STRUCTURAL half — whether the clause carries
/// a clause-level distributive quantifier over the prevented amount — is read
/// from the lowered IR by the caller (`ClauseIr::repeat_for`), never re-parsed
/// here: the parser already lowered it, and re-deriving it would put one truth
/// in two representations that can drift.
///
/// Scanned at word boundaries rather than matched as an arbitrary substring, so
/// only the complete phrase counts as Oracle grammar.
pub fn scan_original_case_prevented_this_way_back_reference(input: &str) -> bool {
    crate::parser::oracle_nom::primitives::scan_at_word_boundaries(
        input,
        tag::<_, _, crate::parser::oracle_nom::error::OracleError<'_>>("prevented this way"),
    )
    .is_some()
}

/// CR 615.5: parse the LEADING distributive quantifier of a prevention rider —
/// `"for each [<n> ]damage prevented this way,"` — and return `<n>` when one is printed.
///
/// ACCEPTED LANGUAGE == THE PRODUCER'S. The clause-level `repeat_for` this binds to is lowered
/// by `oracle_quantity::parse_for_each_clause`, which maps EXACTLY two literals to
/// `QuantityRef::EventContextAmount`: `"1 damage prevented this way"` and
/// `"damage prevented this way"`. The numeral is therefore `opt`: requiring it would refuse a
/// clause the producer accepts, silently losing a future bare printing. Mirroring the producer
/// is the binding; a stricter parallel grammar would be a second truth that can drift.
///
/// LOWERCASE input, like this module's `has_*` siblings and UNLIKE
/// `scan_original_case_prevented_this_way_back_reference`, whose doc prescribes lowering at the
/// call site rather than widening a tag (widening the shared scanner would also widen the
/// adjacent Awe Strike arm in `oracle_effect/assembly.rs`).
///
/// ANCHORED AT THE HEAD, and that is the whole point. `oracle_effect::lower::
/// strip_for_each_prefix_with_difference` matches `tag("for each ")` at the head of the clause
/// and terminates the inner phrase at the first `", "`, and the clause's stored fragment is the
/// pre-strip `normalized_text` (EVERY `.clause(` call site inside `parse_effect_chain_ir` passes
/// it — measured exhaustively over the whole of `oracle_effect::parse_effect_chain_ir`, every
/// `.clause(` site in it, 32 at the time of writing; the load-bearing claim is the
/// exhaustiveness, not the count, and not any line range). Parsing the SAME phrase in the SAME position is therefore a
/// structural binding of the quantifier to its anaphor, not a heuristic: a clause whose leading
/// quantifier names a DRAW ("for each card drawn this way") cannot satisfy this parser even when
/// the clause also mentions prevented damage later.
///
/// DIVERGENCE FROM THE STORED FRAGMENT, DISCLOSED AND MEASURED FAIL-CLOSED: three head/tail
/// rewrites sit between the emitted `normalized_text` (what gets stored as `source.fragment()`,
/// what this combinator sees) and the LOCAL `text` that
/// `strip_for_each_prefix_with_difference` itself consumes inside `parse_effect_chain_ir` —
/// `strip_unrecognized_conditional_head_when_body_optional` (only when no condition was
/// extracted), `strip_unless_entered_suffix`, and `strip_leading_instead` (only when a condition
/// WAS extracted). So for a hypothetical clause printed `"If <X>, for each 1 damage prevented
/// this way, …"`, the `repeat_for` PRODUCER could still see a stripped head and lower `repeat_for`
/// (conjunct 1 passes), while THIS combinator — applied to the unstripped stored fragment — would
/// refuse (conjunct 2 fails closed). That is fail-closed, not silently wrong: the clause falls
/// back to `SequentialSibling`, never to a wrongly-folded `ContinuationStep`. MEASURED: zero live
/// carriers today — all four leading-quantifier class members (Inkshield, Brace for Impact,
/// Temper, Test of Faith) begin the sentence with "For each", so no condition-head or
/// unless/instead strip ever precedes their `repeat_for` clause. Two `normalized_text` shadowing
/// sites exist earlier in the same loop iteration (a trailing coin-heads strip gated on the
/// PRIOR clause being a multi-coin-flip, and a `"starting with you, "` prefix strip); neither can
/// precede a "For each" head, so the head-anchoring conclusion stands for every corpus card.
pub fn parse_leading_prevented_this_way_for_each(input: &str) -> OracleResult<'_, Option<u32>> {
    preceded(
        tag("for each "),
        terminated(
            terminated(
                opt(terminated(parse_number, tag(" "))),
                tag("damage prevented this way"),
            ),
            opt(tag(",")),
        ),
    )
    .parse(input)
}

/// Recognize one complete event-relative prevention watcher. The event clause
/// and its prevention formula must share the same sentence, rather than two
/// independent scans accidentally binding unrelated phrases on a card.
fn parse_each_time_event_relative_prevention(input: &str) -> OracleResult<'_, ()> {
    preceded(
        tag("each time "),
        preceded(
            terminated(
                take_till1(|character| matches!(character, ',' | '.' | '\n' | '\r')),
                tag(", prevent "),
            ),
            // `parse_damage_prevention_formula` accepts the fully understood
            // forms. The bare heads remain deliberately unsupported, but this
            // classifier must recognize them so the caller emits an honest gap.
            alt((
                value((), parse_damage_prevention_formula),
                value((), tag("x of that damage")),
                value((), tag("half that damage")),
            )),
        ),
    )
    .parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_fractional_and_bound_quantity_forms() {
        assert!(matches!(
            parse_damage_prevention_formula("3 of that damage")
                .unwrap()
                .1,
            PreventionFormula::Fixed(3)
        ));
        assert!(matches!(
            parse_damage_prevention_formula("half that damage, rounded up")
                .unwrap()
                .1,
            PreventionFormula::Fraction {
                rounding: RoundingMode::Up,
                ..
            }
        ));
        assert!(matches!(
            parse_damage_prevention_formula(
                "x of that damage, where x is the number of Clerics you control"
            )
            .unwrap()
            .1,
            PreventionFormula::Quantity { .. }
        ));
    }

    #[test]
    fn classifies_each_time_event_relative_prevention() {
        assert!(has_each_time_event_relative_prevention(
            "until end of turn, each time damage is dealt to target creature or player, prevent x of that damage, where x is a number from 1 to 3 chosen at random each time."
        ));
        assert!(has_each_time_event_relative_prevention(
            "each time a source would deal damage to you, prevent half that damage."
        ));
        assert!(has_each_time_event_relative_prevention(
            "each time a source would deal damage to you, prevent half that damage, rounded up."
        ));
        assert!(!has_each_time_event_relative_prevention(
            "prevent half that damage, rounded up."
        ));
        assert!(!has_each_time_event_relative_prevention(
            "each time a source would deal damage to you. Prevent half that damage."
        ));
        assert!(!has_each_time_event_relative_prevention(
            "each time a source would deal damage to you\nprevent half that damage."
        ));
        assert!(!has_each_time_event_relative_prevention(
            "each time a source would deal damage to you\r\nprevent half that damage."
        ));
        assert!(!has_each_time_event_relative_prevention(
            "each time a source would deal damage to you\rprevent half that damage."
        ));
        assert!(!has_each_time_event_relative_prevention(
            "each time a player draws a card, they gain 1 life. Prevent half that damage."
        ));
    }

    #[test]
    fn scans_original_case_prevented_this_way_back_reference() {
        // Positive, leading order (Inkshield's fragment).
        assert!(scan_original_case_prevented_this_way_back_reference(
            "For each 1 damage prevented this way, create a 2/1 white and black Inkling creature token with flying."
        ));
        // Positive, trailing order (Immortal Coil's fragment). The recognizer
        // is position-agnostic, and so is the caller's predicate: nothing
        // tests clause position. Immortal Coil is refused there by the
        // `repeat_for` conjunct, not by position — it is a static replacement
        // whose `ChangeZone` rider has no count axis at all, so the trailing
        // for-each is dropped rather than absorbed and no clause-level
        // `repeat_for` survives either way.
        assert!(scan_original_case_prevented_this_way_back_reference(
            "Exile a card from your graveyard for each 1 damage prevented this way."
        ));
        // Negative: a different anaphor over the same "for each ... this way"
        // grammar (Read the Runes) must not be recognized as this one.
        assert!(!scan_original_case_prevented_this_way_back_reference(
            "for each card drawn this way, discard a card"
        ));
        // Negative: the bare word alone is not the phrase.
        assert!(!scan_original_case_prevented_this_way_back_reference(
            "prevented"
        ));
        // Negative: CASE CONTRACT. This combinator takes ORIGINAL-case input
        // and its tag is lowercase-only, unlike this module's `has_*` siblings
        // which document lowercased input. A capitalized printing must not
        // match until the input is explicitly lowered at the call site.
        assert!(!scan_original_case_prevented_this_way_back_reference(
            "Prevented this way"
        ));
    }

    /// #8849 (M-3, M-9): the leading distributive quantifier combinator's
    /// accepted language must equal `oracle_quantity::parse_for_each_clause`'s
    /// (measured, §P8 of the plan): exactly `"1 damage prevented this way"` and
    /// the bare `"damage prevented this way"`.
    #[test]
    fn parses_leading_distributive_prevented_this_way_quantifier() {
        // Positive: Inkshield's exact leading phrase.
        assert_eq!(
            parse_leading_prevented_this_way_for_each(
                "for each 1 damage prevented this way, create a token"
            )
            .unwrap()
            .1,
            Some(1)
        );
        // Positive (M-9): the bare numeral-less form — the accepted language
        // equals the producer's, not a stricter invented grammar.
        assert_eq!(
            parse_leading_prevented_this_way_for_each(
                "for each damage prevented this way, create a token"
            )
            .unwrap()
            .1,
            None
        );
        // Positive: a numeral other than 1 still PARSES — the caller's gate
        // (conjunct 2's `count.is_none_or(|n| n == 1)`), not the combinator,
        // is what refuses it (M-6).
        assert_eq!(
            parse_leading_prevented_this_way_for_each(
                "for each 2 damage prevented this way, create a token"
            )
            .unwrap()
            .1,
            Some(2)
        );
        // Negative: a different leading anaphor (Read the Runes' draw cohort)
        // over the same "for each ... this way" grammar must not match.
        assert!(parse_leading_prevented_this_way_for_each(
            "for each card drawn this way, discard a card"
        )
        .is_err());
        // Negative: NOT HEAD-ANCHORED — the phrase is present but not at the
        // head of the clause (Immortal Coil's printed order).
        assert!(parse_leading_prevented_this_way_for_each(
            "exile a card from your graveyard for each 1 damage prevented this way"
        )
        .is_err());
        // Negative: CASE CONTRACT — this combinator takes LOWERCASE input,
        // unlike `scan_original_case_prevented_this_way_back_reference`. A
        // capitalized printing must not match until the caller lowers it.
        assert!(parse_leading_prevented_this_way_for_each(
            "For each 1 damage prevented this way, create a token"
        )
        .is_err());
    }
}
