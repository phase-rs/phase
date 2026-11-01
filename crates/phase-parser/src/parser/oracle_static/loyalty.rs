// CR 606.3 — planeswalker loyalty activation statics.
// CR 602.5e + CR 702.6a — tagged-ability-class activation timing statics.

use super::cost_mod::parse_taggable_ability_tag;
#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use crate::types::ability::TypedFilter;
use nom::bytes::complete::take_till;
use nom::combinator::map_opt;

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

/// CR 606.3: "You may activate [the] loyalty abilities of \<subject\> \<count\>
/// each turn rather than only once" — the loyalty-activation SET-cap static
/// (Oath of Teferi: subject "planeswalkers you control"; Urza, Planeswalker:
/// subject `~` after card-name normalization).
///
/// Composes the boast count axis (`twice`/`thrice`/N, mirroring the
/// `dispatch.rs` boast branch rather than extracting it) with the Eidolon
/// subject axis ("loyalty abilities of \<subject\>"). Emits the existing
/// `StaticMode::ModifyActivationLimit` with the `"loyalty"` keyword
/// convention the cost gate already matches (`casting.rs`), consumed by the
/// loyalty legality check against `is_loyalty_ability_cost` — never by
/// `AbilityTag`, which loyalty abilities deliberately lack.
///
/// Scope wording selects mechanism: only the "each turn" continuous scope is
/// accepted. A "this turn" one-shot scope fails the tail and returns `None`,
/// failing closed to today's honest `Unimplemented` so a future
/// permanent-printed "twice this turn" line can never silently acquire
/// permanent semantics. An unrecognized subject (including empty) is likewise
/// rejected rather than emitting a match-anything filter.
pub(crate) fn parse_loyalty_activation_limit_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (subject, new_limit) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = tag("you may activate ").parse(i)?;
        let (i, _) = opt(tag("the ")).parse(i)?;
        let (i, _) = tag("loyalty abilities of ").parse(i)?;
        let (i, (subject, new_limit)) = alt((
            map(
                terminated(
                    take_until(" twice each turn rather than "),
                    tag(" twice each turn rather than "),
                ),
                |subject: &str| (subject.to_string(), 2u8),
            ),
            map(
                terminated(
                    take_until(" thrice each turn rather than "),
                    tag(" thrice each turn rather than "),
                ),
                |subject: &str| (subject.to_string(), 3u8),
            ),
            map(
                (
                    take_till(|c: char| c.is_ascii_digit()),
                    map_opt(nom_primitives::parse_number, |n| {
                        u8::try_from(n).ok().filter(|&limit| limit != 0)
                    }),
                    tag(" times each turn rather than "),
                ),
                |(subject, new_limit, _)| (subject.to_string(), new_limit),
            ),
        ))
        .parse(i)?;
        let (i, _) = opt(tag("only ")).parse(i)?;
        let (i, _) = tag("once").parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = all_consuming(value((), tag(""))).parse(i)?;
        Ok((i, (subject, new_limit)))
    })
    .map(|((subject, new_limit), _)| (subject, new_limit))?;

    // CR 201.5 + CR 201.5c: a self-reference subject scopes the static to its source
    // (the `dispatch.rs` "~" pattern); anything else folds through the typed
    // subject phrase. Reject an empty or unrecognized subject outright.
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        return None;
    }
    let affected = if trimmed == "~" {
        TargetFilter::SelfRef
    } else {
        let (affected, rest) = parse_type_phrase_folding(trimmed);
        if !rest.trim().is_empty() {
            return None;
        }
        affected
    };

    Some(
        StaticDefinition::new(StaticMode::ModifyActivationLimit {
            keyword: "loyalty".to_string(),
            new_limit,
        })
        .affected(affected)
        .description(text.to_string()),
    )
}
