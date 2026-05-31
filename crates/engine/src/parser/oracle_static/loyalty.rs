// CR 606.3 — planeswalker loyalty activation statics.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

pub(crate) fn parse_self_loyalty_activation_permission(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            tag("you may activate "),
            opt(alt((
                tag("her "),
                tag("his "),
                tag("its "),
                tag("their "),
                tag("~'s "),
            ))),
            tag("loyalty abilities any time you could cast an instant"),
        ),
    )
    .parse(input)
}

pub(crate) fn parse_loyalty_activation_timing_permission(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let condition = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, condition_text) =
            preceded(tag("as long as "), terminated(take_until(", "), tag(", "))).parse(i)?;
        let (i, _) = parse_self_loyalty_activation_permission(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = all_consuming(value((), tag(""))).parse(i)?;
        Ok((i, condition_text.to_string()))
    })
    .map(|(condition_text, _)| {
        parse_static_condition(&condition_text).unwrap_or(StaticCondition::Unrecognized {
            text: condition_text,
        })
    })?;

    Some(
        StaticDefinition::new(StaticMode::ActivateAsInstant {
            cost_category: CostCategory::PaysLoyalty,
        })
        .affected(TargetFilter::SelfRef)
        .condition(condition)
        .description(text.to_string()),
    )
}
