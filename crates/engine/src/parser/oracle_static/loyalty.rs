// CR 606.3 — planeswalker loyalty activation statics.
// CR 602.5e + CR 702.6a — tagged-ability-class activation timing statics.

use super::cost_mod::parse_taggable_ability_tag;
#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use crate::types::ability::TypedFilter;

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
        parse_static_condition(&condition_text).unwrap_or_else(|| {
            unparsed_gate_condition(&condition_text, ConditionGatePolarity::Positive)
        })
    })?;

    Some(
        StaticDefinition::new(StaticMode::ActivateAsInstant {
            cost_category: CostCategory::PaysLoyalty,
            keyword: None,
        })
        .affected(TargetFilter::SelfRef)
        .condition(condition)
        .description(text.to_string()),
    )
}

/// CR 602.5e + CR 702.6a: "You may activate equip abilities any time you could
/// cast an instant." (Leonin Shikari) and its class — any "You may activate
/// [tagged] abilities any time you could cast an instant" permission keyed to
/// one of the taggable ability classes ([`parse_taggable_ability_tag`]:
/// equip, power-up, exhaust, outlast, boast), composed rather than hard-coded
/// to equip alone so an equivalent card for another tagged class needs no new
/// parser branch. Unlike the loyalty form above, this permission isn't scoped
/// to the source's own abilities — it applies to every ability of the tagged
/// class its controller could activate, on any permanent (CR 702.6a doesn't
/// require the permission-granter to control the Equipment; the *player*
/// scope — the static's controller must be the activator — is already
/// enforced at the runtime check site, so `affected` only needs the
/// permanent/tag axis, not a redundant controller filter). `cost_category`
/// stays `ManaOnly` as a placeholder value: the runtime ignores it whenever
/// `keyword` is set, since CR 702.6a doesn't require Equip's cost to be mana
/// (a reconfigure- or sacrifice-cost equip-like ability still carries the tag).
pub(crate) fn parse_tagged_ability_activation_timing_permission(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let tag_value = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = tag("you may activate ").parse(i)?;
        let (i, tag_value) = parse_taggable_ability_tag(i)?;
        let (i, _) = tag(" abilities any time you could cast an instant").parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = all_consuming(value((), tag(""))).parse(i)?;
        Ok((i, tag_value))
    })
    .map(|(tag_value, _)| tag_value)?;

    Some(
        StaticDefinition::new(StaticMode::ActivateAsInstant {
            cost_category: CostCategory::ManaOnly,
            keyword: Some(tag_value),
        })
        .affected(TargetFilter::Typed(TypedFilter::permanent()))
        .description(text.to_string()),
    )
}
