// CR 604.3: unconditional and conditional self power/toughness equality statics.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// Parse self power/toughness equality, using ordinary setters for gated
/// abilities and characteristic-defining setters for unconditional abilities:
/// - "~'s power and toughness are each equal to the number of creatures you control."
/// - "~'s power is equal to the number of card types among cards in all graveyards
///   and its toughness is equal to that number plus 1."
/// - "~'s toughness is equal to the number of cards in your hand."
pub(crate) fn parse_cda_pt_equality(lower: &str, text: &str) -> Option<StaticDefinition> {
    // CR 604.3a + CR 611.3a: an equality that only applies within a turn
    // window or while a condition holds is not characteristic defining.
    // Preserve the full description while parsing only the equality body.
    let description = text;
    let tp = TextPair::new(text, lower);
    let (lower, text, timing_condition) = if let Some(rest) = nom_tag_tp(&tp, "during your turn, ")
    {
        (
            rest.lower,
            rest.original,
            Some(StaticCondition::DuringYourTurn),
        )
    } else if let Some(rest) = nom_tag_tp(&tp, "during turns other than yours, ") {
        (
            rest.lower,
            rest.original,
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            }),
        )
    } else {
        (lower, text, None)
    };

    let tp = TextPair::new(text, lower);
    // "For as long as" belongs to effect duration, not this static gate.
    if nom_primitives::scan_contains(lower, "for as long as ") {
        return None;
    }
    let (body, gate_condition) = match split_trailing_gate_condition_with_body(&tp) {
        Some((body, gate)) => (
            body,
            Some(
                parse_static_condition(gate).unwrap_or_else(|| StaticCondition::Unrecognized {
                    text: gate.to_string(),
                }),
            ),
        ),
        None => (text, None),
    };
    let condition = super::evasion::combine_conditions(timing_condition, gate_condition);
    let body_lower = body.to_lowercase();
    let lower = body_lower.as_str();

    // Detect framing
    let both = nom_primitives::scan_contains(lower, "power and toughness are each equal to");
    let power_only = !both && nom_primitives::scan_contains(lower, "power is equal to");
    let toughness_only =
        !both && !power_only && nom_primitives::scan_contains(lower, "toughness is equal to");
    // Fixed equality shares the same conditional/CDA classification.
    let both_const = !both
        && !power_only
        && !toughness_only
        && nom_primitives::scan_contains(lower, "power and toughness are each ");

    if !both && !power_only && !toughness_only && !both_const {
        return None;
    }

    if both_const {
        let after = strip_after(lower, "power and toughness are each ")?;
        let digits: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let value = digits.parse::<i32>().ok()?;
        let mut def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![
                ContinuousModification::SetPower { value },
                ContinuousModification::SetToughness { value },
            ])
            .description(description.to_string());
        if let Some(cond) = condition {
            def = def.condition(cond);
        } else {
            def = def.cda();
        }
        return Some(def);
    }

    // Extract the quantity body through the shared word-boundary scanner.
    let quantity_text = nom_primitives::scan_at_word_boundaries(lower, |i| {
        preceded(
            alt((
                tag::<_, _, OracleError<'_>>("are each equal to "),
                tag("power is equal to "),
                tag("toughness is equal to "),
            )),
            rest,
        )
        .parse(i)
    })?;

    // Strip trailing clause for split P/T ("and its toughness is equal to...")
    let quantity_text = quantity_text
        .split(" and its toughness")
        .next()
        .unwrap_or(quantity_text)
        .trim_end_matches('.');

    let qty = parse_cda_quantity(quantity_text)?;

    // CR 604.3a + CR 613.4a + CR 613.4b: gated equality uses ordinary
    // layer-7b setters; unconditional characteristic definitions use layer 7a.
    let set_power = |value| {
        if condition.is_some() {
            ContinuousModification::SetPowerDynamic { value }
        } else {
            ContinuousModification::SetDynamicPower { value }
        }
    };
    let set_toughness = |value| {
        if condition.is_some() {
            ContinuousModification::SetToughnessDynamic { value }
        } else {
            ContinuousModification::SetDynamicToughness { value }
        }
    };
    let mut modifications = Vec::new();

    if both {
        modifications.push(set_power(qty.clone()));
        // CR 208.2 + CR 613.4a: "... are each equal to <qty> and its toughness is
        // equal to that number plus N" (Subgoyf: */1+*). Power stays bare `qty`;
        // toughness takes the same "+N" offset the `power_only` branch applies,
        // so a distinct-subtype count of 2 yields 2/3, not 2/2. Absent the plus
        // clause, toughness stays bare `qty` (the ordinary "each equal to" case).
        let toughness_value = match parse_that_number_plus_offset(lower) {
            Some(offset) => QuantityExpr::Offset {
                inner: Box::new(qty),
                offset,
            },
            None => qty,
        };
        modifications.push(set_toughness(toughness_value));
    } else if power_only {
        modifications.push(set_power(qty.clone()));
        // Check for split P/T: "and its toughness is equal to that number plus N"
        if let Some(offset) = parse_that_number_plus_offset(lower) {
            modifications.push(set_toughness(QuantityExpr::Offset {
                inner: Box::new(qty),
                offset,
            }));
        }
    } else {
        // toughness_only
        modifications.push(set_toughness(qty));
    }

    let mut def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(modifications)
        .description(description.to_string());
    if let Some(cond) = condition {
        def = def.condition(cond);
    } else {
        def = def.cda();
    }
    Some(def)
}

/// CR 208.2: Extract the `N` from a trailing "... its toughness is equal to that
/// number plus N" toughness override. Shared by the `power_only` and `both`
/// framings so a split-P/T offset (Subgoyf's `*/1+*`) is applied identically in
/// each. Returns `None` when the phrase is absent (bare "each equal to").
fn parse_that_number_plus_offset(lower: &str) -> Option<i32> {
    let after_plus = strip_after(lower, "that number plus ")?;
    let n_str = after_plus
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("0");
    n_str.parse::<i32>().ok()
}
