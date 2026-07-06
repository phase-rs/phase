// CR 604.3 — characteristic-defining ability statics.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// Parse CDA power/toughness equality patterns like:
/// - "~'s power and toughness are each equal to the number of creatures you control."
/// - "~'s power is equal to the number of card types among cards in all graveyards
///   and its toughness is equal to that number plus 1."
/// - "~'s toughness is equal to the number of cards in your hand."
pub(crate) fn parse_cda_pt_equality(lower: &str, text: &str) -> Option<StaticDefinition> {
    // CR 611.3 + CR 613.7c: peel a leading turn-window timing condition so a CDA
    // scoped to "During your turn," / "During turns other than yours," carries
    // that condition (Angry Mob), mirroring the base-P/T-set path. Such a card's
    // two clauses are split into separate sentences upstream by
    // `parse_multi_sentence_statics`, so each clause reaches here independently.
    let (lower, text, timing_condition) =
        if let Some(rest) = nom_tag_lower(lower, lower, "during your turn, ") {
            (
                rest,
                &text[text.len() - rest.len()..],
                Some(StaticCondition::DuringYourTurn),
            )
        } else if let Some(rest) = nom_tag_lower(lower, lower, "during turns other than yours, ") {
            (
                rest,
                &text[text.len() - rest.len()..],
                Some(StaticCondition::Not {
                    condition: Box::new(StaticCondition::DuringYourTurn),
                }),
            )
        } else {
            (lower, text, None)
        };

    // Detect framing
    let both = nom_primitives::scan_contains(lower, "power and toughness are each equal to");
    let power_only = !both && nom_primitives::scan_contains(lower, "power is equal to");
    let toughness_only =
        !both && !power_only && nom_primitives::scan_contains(lower, "toughness is equal to");
    // CR 613.4c: constant characteristic-defining P/T — "~'s power and toughness
    // are each N" (Angry Mob's off-turn clause "... are each 2"): a fixed base
    // value, not a dynamic quantity. Guarded by `!both` so the dynamic "are each
    // equal to" framing (which also contains "are each ") keeps priority.
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
            .cda()
            .description(text.to_string());
        if let Some(cond) = timing_condition {
            def = def.condition(cond);
        }
        return Some(def);
    }

    // Extract the quantity text after "equal to "
    let quantity_start = if both {
        lower
            .find("are each equal to ") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .map(|p| p + "are each equal to ".len())
    } else if power_only {
        lower
            .find("power is equal to ") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .map(|p| p + "power is equal to ".len())
    } else {
        lower
            .find("toughness is equal to ") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .map(|p| p + "toughness is equal to ".len())
    };
    let quantity_text = &lower[quantity_start?..];

    // Strip trailing clause for split P/T ("and its toughness is equal to...")
    let quantity_text = quantity_text
        .split(" and its toughness")
        .next()
        .unwrap_or(quantity_text)
        .trim_end_matches('.');

    let qty = parse_cda_quantity(quantity_text)?;

    let mut modifications = Vec::new();

    if both {
        modifications.push(ContinuousModification::SetDynamicPower { value: qty.clone() });
        modifications.push(ContinuousModification::SetDynamicToughness { value: qty });
    } else if power_only {
        modifications.push(ContinuousModification::SetDynamicPower { value: qty.clone() });
        // Check for split P/T: "and its toughness is equal to that number plus N"
        if let Some(after_plus) = strip_after(lower, "that number plus ") {
            let n_str = after_plus
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("0");
            let offset = n_str.parse::<i32>().unwrap_or(0);
            modifications.push(ContinuousModification::SetDynamicToughness {
                value: QuantityExpr::Offset {
                    inner: Box::new(qty),
                    offset,
                },
            });
        }
    } else {
        // toughness_only
        modifications.push(ContinuousModification::SetDynamicToughness { value: qty });
    }

    let mut def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(modifications)
        .cda()
        .description(text.to_string());
    if let Some(cond) = timing_condition {
        def = def.condition(cond);
    }
    Some(def)
}
